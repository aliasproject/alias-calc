//! The Wayland window: an xdg toplevel drawn on the CPU into a `wl_shm`
//! buffer, sharp at fractional scales through `fractional-scale-v1` and
//! `viewporter`.
//!
//! Everything runs on the one thread. The loop blocks in `poll` on the
//! Wayland socket and only wakes when the compositor sends something, so an
//! idle window costs no CPU at all.

use std::error::Error;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::backend::WaylandError;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{
    wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface,
};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo,
};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::engine::{Calc, Key};
use crate::input;
use crate::render::{Canvas, Fonts};
use crate::trace;
use crate::ui::{self, Palette, View};

/// What people see in the launcher and the title bar. "Alias Calc" is the
/// product name; the window says what it does.
const TITLE: &str = "Calculator";
const APP_ID: &str = "com.aliasproject.calc";

/// Logical size when the compositor leaves it to us (a floating window).
const DEFAULT_SIZE: (u32, u32) = (320, 480);
const MIN_SIZE: (u32, u32) = (260, 380);

/// Key repeat when the compositor doesn't say (it always does on Hyprland).
const DEFAULT_REPEAT: (Duration, Duration) =
    (Duration::from_millis(600), Duration::from_millis(40));

/// The left mouse button (linux/input-event-codes.h BTN_LEFT).
const BTN_LEFT: u32 = 0x110;

/// Fractional scales arrive as 120ths (`wp_fractional_scale_v1`).
const SCALE_DENOMINATOR: u32 = 120;

pub fn run() -> Result<(), Box<dyn Error>> {
    let conn = Connection::connect_to_env()?;
    trace::mark("connected");
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();
    trace::mark("globals");

    let compositor = CompositorState::bind(&globals, &qh)?;
    let xdg_shell = XdgShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    // Both are optional: without them the window falls back to integer
    // buffer scales.
    let fractional_manager: Option<WpFractionalScaleManagerV1> =
        globals.bind(&qh, 1..=1, NoEvents).ok();
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, NoEvents).ok();
    let cursor_shape = CursorShapeManager::bind(&globals, &qh).ok();

    let surface = compositor.create_surface(&qh);
    let scaling = match (fractional_manager, viewporter) {
        (Some(manager), Some(viewporter)) => {
            manager.get_fractional_scale(&surface, &qh, ScaleData);
            let viewport = viewporter.get_viewport(&surface, &qh, NoEvents);
            Scaling::Fractional {
                viewport,
                scale120: SCALE_DENOMINATOR,
            }
        }
        _ => Scaling::Integer(1),
    };

    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title(TITLE);
    window.set_app_id(APP_ID);
    window.set_min_size(Some(MIN_SIZE));
    // The first commit has no buffer: it asks the compositor for a
    // configure, and the first draw answers it.
    window.commit();
    trace::mark("window requested");

    // Parse the fonts while the compositor works on the configure.
    let fonts = Fonts::load();
    trace::mark("fonts loaded");

    let (w, h) = DEFAULT_SIZE;
    let pool = SlotPool::new((w * h * 4) as usize, &shm)?;
    let mut app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        keyboard: None,
        pointer: None,
        cursor_shape,
        cursor: None,
        modifiers: Modifiers::default(),
        calc: Calc::new(),
        mouse_down: None,
        mouse_at: (0.0, 0.0),
        key_down: None,
        repeat_rate: Some(DEFAULT_REPEAT),
        repeat: None,
        shm,
        window,
        pool,
        buffer: None,
        fonts,
        palette: Palette::default(),
        size: DEFAULT_SIZE,
        scaling,
        configured: false,
        drawn: false,
        keyboard_seat: None,
        pointer_seat: None,
        dirty: false,
        exit: false,
    };

    event_loop(&mut queue, &mut app)
}

type Queue = smithay_client_toolkit::reexports::client::EventQueue<App>;

/// Dispatch, draw if anything changed, then sleep until the compositor
/// writes to the socket. No timeouts: an idle window never wakes.
fn event_loop(queue: &mut Queue, app: &mut App) -> Result<(), Box<dyn Error>> {
    loop {
        queue.dispatch_pending(app)?;
        if app.exit {
            return Ok(());
        }
        if app.dirty && app.configured {
            app.draw()?;
            if app.keyboard_seat.is_some() || app.pointer_seat.is_some() {
                // Send the first frame on its own: a new keyboard makes the
                // compositor build and send a keymap, and that shouldn't
                // hold up mapping the window.
                let _ = queue.flush();
                app.bind_input(&queue.handle());
            }
        }
        // A full socket leaves requests queued; then wait until it can take
        // more as well as for events.
        let unsent = match queue.flush() {
            Ok(()) => false,
            Err(WaylandError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(err) => return Err(err.into()),
        };

        // Events already queued mean there's nothing to wait for.
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let fd = guard.connection_fd();
        let flags = if unsent {
            PollFlags::IN | PollFlags::OUT
        } else {
            PollFlags::IN
        };
        let mut fds = [PollFd::new(&fd, flags)];
        // Only a held Backspace sets a timeout; otherwise sleep until the
        // compositor sends something.
        let timeout = app.repeat.as_ref().map(|r| {
            let wait = r.next.saturating_duration_since(Instant::now());
            Timespec {
                tv_sec: wait.as_secs() as _,
                tv_nsec: wait.subsec_nanos() as _,
            }
        });
        match poll(&mut fds, timeout.as_ref()) {
            Ok(0) => {
                drop(guard);
                app.repeat_tick();
                continue;
            }
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(err) => return Err(err.into()),
        }
        if !fds[0].revents().contains(PollFlags::IN) {
            // Only writable: go round and flush.
            drop(guard);
            continue;
        }
        match guard.read() {
            Ok(n) => {
                if trace::on() {
                    trace::mark(&format!("woke: {n} event(s)"));
                }
            }
            Err(WaylandError::Io(err)) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(err.into()),
        }
    }
}

enum Scaling {
    /// `wp_fractional_scale_v1` + `wp_viewport`: the buffer is drawn at the
    /// exact pixel size and the viewport maps it to the logical size.
    Fractional { viewport: WpViewport, scale120: u32 },
    /// `wl_surface.set_buffer_scale` with the output's integer scale.
    Integer(u32),
}

struct App {
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    window: Window,
    pool: SlotPool,
    buffer: Option<Buffer>,
    fonts: Fonts,
    palette: Palette,
    seats: SeatState,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_shape: Option<CursorShapeManager>,
    cursor: Option<WpCursorShapeDeviceV1>,
    modifiers: Modifiers,
    calc: Calc,
    /// The button the left mouse button went down on.
    mouse_down: Option<(usize, usize)>,
    /// The pointer's last logical position.
    mouse_at: (f64, f64),
    /// The keyboard key held down (its raw code) and the button it lights.
    key_down: Option<(u32, Option<(usize, usize)>)>,
    /// Repeat delay and interval, or None when the compositor disables repeat.
    repeat_rate: Option<(Duration, Duration)>,
    repeat: Option<Repeat>,
    /// Logical size, in surface coordinates.
    size: (u32, u32),
    scaling: Scaling,
    configured: bool,
    /// A frame has been committed.
    drawn: bool,
    /// Seats whose keyboard and pointer wait for the first frame.
    keyboard_seat: Option<wl_seat::WlSeat>,
    pointer_seat: Option<wl_seat::WlSeat>,
    /// Something changed since the last frame.
    dirty: bool,
    exit: bool,
}

/// A key being repeated while held.
struct Repeat {
    raw_code: u32,
    key: Key,
    next: Instant,
}

impl App {
    fn bind_keyboard(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) {
        match self.seats.get_keyboard(qh, seat, None) {
            Ok(keyboard) => self.keyboard = Some(keyboard),
            Err(err) => eprintln!("alias-calc: no keyboard: {err}"),
        }
    }

    fn bind_pointer(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) {
        match self.seats.get_pointer(qh, seat) {
            Ok(pointer) => {
                self.cursor = self
                    .cursor_shape
                    .as_ref()
                    .map(|m| m.get_shape_device(&pointer, qh));
                self.pointer = Some(pointer);
            }
            Err(err) => eprintln!("alias-calc: no pointer: {err}"),
        }
    }

    /// Binds the keyboard and pointer held back for the first frame.
    fn bind_input(&mut self, qh: &QueueHandle<Self>) {
        if let Some(seat) = self.keyboard_seat.take() {
            self.bind_keyboard(qh, &seat);
        }
        if let Some(seat) = self.pointer_seat.take() {
            self.bind_pointer(qh, &seat);
        }
    }

    fn press(&mut self, key: Key) {
        self.calc.press(key);
        self.dirty = true;
    }

    fn repeat_tick(&mut self) {
        let Some(repeat) = &mut self.repeat else {
            return;
        };
        let key = repeat.key;
        let interval = self.repeat_rate.map_or(DEFAULT_REPEAT.1, |r| r.1);
        repeat.next += interval;
        // After a stall (a suspended machine), don't fire a burst.
        let now = Instant::now();
        if repeat.next < now {
            repeat.next = now + interval;
        }
        self.press(key);
    }

    /// The button under the pointer, if any.
    fn button_under_pointer(&self) -> Option<(usize, usize)> {
        let (w, h) = self.size;
        let (x, y) = self.mouse_at;
        let scale = self.pixel_size().0 as f32 / w as f32;
        ui::hit_test(
            &self.fonts,
            (w as f32, h as f32),
            scale,
            (x as f32, y as f32),
        )
    }

    /// The buffer's size in pixels for the current logical size and scale.
    fn pixel_size(&self) -> (u32, u32) {
        let (w, h) = self.size;
        match self.scaling {
            // Round to nearest, as the protocol asks.
            Scaling::Fractional { scale120, .. } => (
                (w * scale120 + SCALE_DENOMINATOR / 2) / SCALE_DENOMINATOR,
                (h * scale120 + SCALE_DENOMINATOR / 2) / SCALE_DENOMINATOR,
            ),
            Scaling::Integer(scale) => (w * scale, h * scale),
        }
    }

    /// The button drawn pressed: the mouse's, while the pointer is still on
    /// it, else the one whose keyboard key is held.
    fn pressed_button(&self) -> Option<(usize, usize)> {
        let mouse = self
            .mouse_down
            .filter(|&cell| self.button_under_pointer() == Some(cell));
        mouse.or(self.key_down.and_then(|(_, cell)| cell))
    }

    /// Draws and commits a frame. An error means the window can't be shown
    /// (no memory for the buffer), which ends the app rather than leaving it
    /// running unseen.
    fn draw(&mut self) -> Result<(), Box<dyn Error>> {
        self.dirty = false;
        let expression = self.calc.expression();
        let display = self.calc.display();
        let pressed = self.pressed_button();
        let (pw, ph) = self.pixel_size();
        let stride = pw as i32 * 4;

        // Reuse the one buffer when its size still fits and the compositor
        // has released it; otherwise make a new one in the same pool (the
        // pool reuses the freed space, so memory stays flat).
        let reusable = self.buffer.as_ref().is_some_and(|b| {
            b.height() == ph as i32 && b.stride() == stride && self.pool.canvas(b).is_some()
        });
        if !reusable {
            match self
                .pool
                .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
            {
                Ok((buffer, _)) => self.buffer = Some(buffer),
                Err(err) => return Err(format!("can't allocate a {pw}x{ph} buffer: {err}").into()),
            }
        }
        let Some(buffer) = self.buffer.as_ref() else {
            return Err("no buffer".into());
        };
        let Some(canvas) = self.pool.canvas(buffer) else {
            return Err("the new buffer is busy".into());
        };

        let scale = pw as f32 / self.size.0 as f32;
        let view = View {
            expression: &expression,
            display: &display,
            pressed,
        };
        let mut canvas = Canvas::new(canvas, pw as usize, ph as usize);
        ui::draw(&mut canvas, &mut self.fonts, &self.palette, &view, scale);

        let surface = self.window.wl_surface();
        match &self.scaling {
            Scaling::Fractional { viewport, .. } => {
                viewport.set_destination(self.size.0 as i32, self.size.1 as i32);
            }
            Scaling::Integer(scale) => surface.set_buffer_scale(*scale as i32),
        }
        surface.damage_buffer(0, 0, pw as i32, ph as i32);
        buffer
            .attach_to(surface)
            .map_err(|err| format!("can't attach the buffer: {err}"))?;
        self.window.commit();
        self.drawn = true;
        if trace::on() {
            trace::mark(&format!("frame committed ({pw}x{ph} px)"));
        }
        Ok(())
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if let Scaling::Integer(scale) = &mut self.scaling {
            let factor = factor.max(1) as u32;
            if *scale != factor {
                *scale = factor;
                self.dirty = true;
            }
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        if !self.configured {
            trace::mark("configured");
        }
        // Each axis the compositor leaves at 0 is ours to choose.
        let w = configure.new_size.0.map_or(self.size.0, |w| w.get());
        let h = configure.new_size.1.map_or(self.size.1, |h| h.get());
        self.size = (w.max(1), h.max(1));
        self.configured = true;
        // Every configure needs a commit to answer it, even when the size
        // is unchanged.
        self.dirty = true;
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            // Compiling the keymap takes ~1.5 ms, and the compositor sends it
            // with the first configure. Nobody types or clicks in the first
            // frame, so input is bound right after it (see `event_loop`).
            if self.drawn {
                self.bind_keyboard(qh, &seat);
            } else {
                self.keyboard_seat = Some(seat.clone());
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            if self.drawn {
                self.bind_pointer(qh, &seat);
            } else {
                self.pointer_seat = Some(seat);
            }
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            self.keyboard_seat = None;
        }
        if capability == Capability::Pointer {
            self.pointer_seat = None;
        }
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
            self.key_down = None;
            self.repeat = None;
        }
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            if let Some(cursor) = self.cursor.take() {
                cursor.destroy();
            }
            pointer.release();
            self.mouse_down = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        // Keys released elsewhere never send their release here.
        if self.key_down.take().is_some_and(|(_, cell)| cell.is_some()) {
            self.dirty = true;
        }
        self.repeat = None;
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        // Ctrl, Alt and Super chords belong to the compositor and to copy
        // and paste (ALI-2102), never to the keypad.
        if self.modifiers.ctrl || self.modifiers.alt || self.modifiers.logo {
            return;
        }
        let Some(key) = input::key_for(event.keysym) else {
            return;
        };
        self.key_down = Some((event.raw_code, ui::cell_of(key)));
        self.repeat = match self.repeat_rate {
            Some((delay, _)) if input::repeats(key) => Some(Repeat {
                raw_code: event.raw_code,
                key,
                next: Instant::now() + delay,
            }),
            _ => None,
        };
        self.press(key);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
        // Only SCTK's calloop integration calls this; the event loop repeats.
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if self.key_down.is_some_and(|(raw, _)| raw == event.raw_code) {
            self.key_down = None;
            self.dirty = true;
        }
        if self
            .repeat
            .as_ref()
            .is_some_and(|r| r.raw_code == event.raw_code)
        {
            self.repeat = None;
        }
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }

    fn update_repeat_info(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        info: RepeatInfo,
    ) {
        self.repeat_rate = match info {
            RepeatInfo::Repeat { rate, delay } => Some((
                Duration::from_millis(delay.into()),
                Duration::from_secs(1) / rate.get(),
            )),
            RepeatInfo::Disable => None,
        };
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            let before = self.pressed_button();
            self.mouse_at = event.position;
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(cursor) = &self.cursor {
                        cursor.set_shape(serial, Shape::Default);
                    }
                }
                PointerEventKind::Leave { .. } => {}
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => {
                    self.mouse_down = self.button_under_pointer();
                }
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } => {
                    // A click counts only if it ends on the button it began on.
                    if let Some(cell) = self.mouse_down.take()
                        && self.button_under_pointer() == Some(cell)
                    {
                        self.press(ui::action(cell));
                    }
                }
                _ => {}
            }
            let after = self.pressed_button();
            if before != after {
                self.dirty = true;
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

/// User data for objects that send no events we use.
struct NoEvents;

impl Dispatch2<WpFractionalScaleManagerV1, App> for NoEvents {
    fn event(
        &self,
        _: &mut App,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<WpViewporter, App> for NoEvents {
    fn event(
        &self,
        _: &mut App,
        _: &WpViewporter,
        _: <WpViewporter as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<WpViewport, App> for NoEvents {
    fn event(
        &self,
        _: &mut App,
        _: &WpViewport,
        _: <WpViewport as smithay_client_toolkit::reexports::client::Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

/// User data for the surface's `wp_fractional_scale_v1`.
struct ScaleData;

impl Dispatch2<WpFractionalScaleV1, App> for ScaleData {
    fn event(
        &self,
        app: &mut App,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Scaling::Fractional { scale120, .. } = &mut app.scaling
            && *scale120 != scale
        {
            *scale120 = scale.max(1);
            app.dirty = true;
        }
    }
}

delegate_registry!(App);
delegate_dispatch2!(App);
