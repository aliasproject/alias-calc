//! The Wayland window: an xdg toplevel drawn on the CPU into a `wl_shm`
//! buffer, sharp at fractional scales through `fractional-scale-v1` and
//! `viewporter`.
//!
//! Everything runs on the one thread. The loop blocks in `poll` on the
//! Wayland socket and only wakes when the compositor sends something (or a
//! held Backspace is due to repeat), so an idle window costs no CPU at all.

use std::error::Error;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{
    CopyPasteSource, DataSourceHandler,
};
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, WritePipe};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::backend::WaylandError;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::{
    wl_data_device::WlDataDevice, wl_data_source::WlDataSource, wl_keyboard, wl_output, wl_pointer,
    wl_seat, wl_shm, wl_surface,
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

use crate::clipboard::{self, Paste, Progress};
use crate::engine::{Calc, Key};
use crate::input;
use crate::render::{Canvas, Fonts};
use crate::textsize;
use crate::theme;
use crate::trace;
use crate::ui::{self, Palette, View};

/// What people see in the launcher and the title bar. "Alias Calc" is the
/// product name; the window says what it does.
const TITLE: &str = "Calculator";
const APP_ID: &str = "com.aliasproject.calc";

/// Logical size when the compositor leaves it to us (a floating window).
const DEFAULT_SIZE: (u32, u32) = (320, 480);
/// 400 tall keeps every key at least 44 px (ui::MIN_KEY) at the smallest size.
const MIN_SIZE: (u32, u32) = (260, 400);

/// Key repeat when the compositor doesn't say (it always does on Hyprland).
const DEFAULT_REPEAT: (Duration, Duration) =
    (Duration::from_millis(600), Duration::from_millis(40));

/// How long "Copied" shows after a copy.
const NOTICE_TIME: Duration = Duration::from_millis(1200);

/// The left mouse button (linux/input-event-codes.h BTN_LEFT).
const BTN_LEFT: u32 = 0x110;

/// `size` grown by the text scale (never shrunk below it).
fn scaled((w, h): (u32, u32), text_scale: f32) -> (u32, u32) {
    let k = text_scale.max(1.0);
    ((w as f32 * k).round() as u32, (h as f32 * k).round() as u32)
}

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
    let data_manager = DataDeviceManagerState::bind(&globals, &qh).ok();

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

    // Larger aliasOS text means a larger window, so the layout holds.
    let text_scale = textsize::load();
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title(TITLE);
    window.set_app_id(APP_ID);
    window.set_min_size(Some(scaled(MIN_SIZE, text_scale)));
    // The first commit has no buffer: it asks the compositor for a
    // configure, and the first draw answers it.
    window.commit();
    trace::mark("window requested");

    // Parse the fonts while the compositor works on the configure.
    let fonts = Fonts::load();
    trace::mark("fonts loaded");
    // Watch first, so a theme swapped in between isn't missed.
    let theme_watch = theme::Watch::new();
    let palette = theme::load().unwrap_or_default();
    trace::mark("theme read");

    let (w, h) = scaled(DEFAULT_SIZE, text_scale);
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
        notice_until: None,
        data_manager,
        data_device: None,
        copied: None,
        paste: None,
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
        palette,
        size: (w, h),
        text_scale,
        focus: None,
        focus_visible: false,
        scaling,
        configured: false,
        drawn: false,
        keyboard_seat: None,
        pointer_seat: None,
        input_seat: None,
        dirty: false,
        exit: false,
    };

    event_loop(&mut queue, &mut app, theme_watch)
}

type Queue = smithay_client_toolkit::reexports::client::EventQueue<App>;

/// Dispatch, draw if anything changed, then sleep until the compositor
/// writes to the socket, the theme changes or a paste arrives. The only
/// timeouts are a held Backspace's next repeat and the end of a showing
/// "Copied", so an idle window never wakes. `theme` is None
/// when there's no theme to follow.
fn event_loop(
    queue: &mut Queue,
    app: &mut App,
    mut theme: Option<theme::Watch>,
) -> Result<(), Box<dyn Error>> {
    loop {
        queue.dispatch_pending(app)?;
        if app.exit {
            return Ok(());
        }
        // A busy socket (pointer motion while Backspace is held) would keep
        // poll from ever timing out, so a due repeat fires here too.
        if app
            .repeat
            .as_ref()
            .is_some_and(|r| r.next <= Instant::now())
        {
            app.repeat_tick();
        }
        if app
            .notice_until
            .is_some_and(|until| until <= Instant::now())
        {
            app.notice_until = None;
            app.dirty = true;
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
        let theme_fd = theme.as_ref().map(|w| w.fd());
        // Out of `app` while polled, so its fd can be borrowed alongside.
        let mut paste = app.paste.take();
        let paste_fd = paste.as_ref().map(|p| p.fd());
        let flags = if unsent {
            PollFlags::IN | PollFlags::OUT
        } else {
            PollFlags::IN
        };
        // Only a held Backspace or a showing "Copied" sets a timeout;
        // otherwise sleep until something arrives.
        let deadline = match (app.repeat.as_ref().map(|r| r.next), app.notice_until) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let timeout = deadline.map(|next| {
            let wait = next.saturating_duration_since(Instant::now());
            Timespec {
                tv_sec: wait.as_secs() as _,
                tv_nsec: wait.subsec_nanos() as _,
            }
        });
        // Without a theme watch or a paste, a slot polls the Wayland fd
        // again with no events asked for. It's never read in that case.
        let idle = || PollFd::from_borrowed_fd(fd, PollFlags::empty());
        let mut fds = [
            PollFd::new(&fd, flags),
            theme_fd
                .as_ref()
                .map_or_else(idle, |f| PollFd::new(f, PollFlags::IN)),
            paste_fd
                .as_ref()
                .map_or_else(idle, |f| PollFd::new(f, PollFlags::IN)),
        ];
        let polled = poll(&mut fds, timeout.as_ref());
        let wayland_revents = fds[0].revents();
        let wayland_in = wayland_revents.contains(PollFlags::IN);
        let theme_ready = theme_fd.is_some() && !fds[1].revents().is_empty();
        let paste_ready = paste_fd.is_some() && !fds[2].revents().is_empty();
        drop(theme_fd);
        drop(paste_fd);
        // Before anything that can `continue`, so the paste is never lost.
        if paste_ready && let Some(mut reading) = paste.take() {
            paste = app.paste_progress(reading.read(), reading);
        }
        app.paste = paste;
        match polled {
            // A repeat or the end of "Copied" came due: the top of the
            // loop handles both.
            Ok(0) => {
                drop(guard);
                continue;
            }
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(err) => return Err(err.into()),
        }
        if theme_ready && let Some(watch) = &theme {
            match watch.changed() {
                Ok(changed) => {
                    if changed.theme {
                        app.reload_theme();
                    }
                    if changed.text {
                        app.reload_text_size();
                    }
                }
                // A broken watch would keep the fd readable and spin the
                // loop: stop following the theme instead.
                Err(err) => {
                    eprintln!("alias-calc: stopped following the theme: {err}");
                    theme = None;
                }
            }
        }
        // A broken socket without data to read would make poll return at
        // once forever; there's nothing to recover, so end.
        if !wayland_in
            && wayland_revents.intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL)
        {
            return Err("lost the connection to the compositor".into());
        }
        if !wayland_in {
            // Only writable, or only the theme: go round.
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
    /// The aliasOS text size as a factor (crate::textsize).
    text_scale: f32,
    /// The key with keyboard focus, and whether its ring shows (from the
    /// first Tab or arrow until the next click).
    focus: Option<(usize, usize)>,
    focus_visible: bool,
    /// When "Copied" stops showing, while it shows.
    notice_until: Option<Instant>,
    /// Copy and paste; None when the compositor has no data device manager.
    data_manager: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    /// What this app last copied, while it still owns the selection.
    copied: Option<(CopyPasteSource, String)>,
    paste: Option<Paste>,
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
    /// The seat the keyboard and pointer were bound from.
    input_seat: Option<wl_seat::WlSeat>,
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
    /// Forgets the keyboard and anything it held: a lit key, a repeat.
    fn drop_keyboard(&mut self) {
        self.keyboard_seat = None;
        if let Some(keyboard) = self.keyboard.take() {
            keyboard.release();
        }
        if self.key_down.take().is_some() {
            self.dirty = true;
        }
        self.repeat = None;
        self.modifiers = Modifiers::default();
        // The data device belongs to the keyboard's seat; a keyboard bound
        // later (maybe on another seat) makes a new one.
        self.data_device = None;
    }

    /// Forgets the pointer and a button it held down.
    fn drop_pointer(&mut self) {
        self.pointer_seat = None;
        if let Some(cursor) = self.cursor.take() {
            cursor.destroy();
        }
        if let Some(pointer) = self.pointer.take() {
            pointer.release();
        }
        if self.mouse_down.take().is_some() {
            self.dirty = true;
        }
    }

    fn bind_keyboard(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) {
        self.input_seat = Some(seat.clone());
        match self.seats.get_keyboard(qh, seat, None) {
            Ok(keyboard) => self.keyboard = Some(keyboard),
            Err(err) => eprintln!("alias-calc: no keyboard: {err}"),
        }
        // Copy and paste are keyboard shortcuts, so they share its seat.
        if self.data_device.is_none()
            && let Some(manager) = &self.data_manager
        {
            self.data_device = Some(manager.get_data_device(qh, seat));
        }
    }

    /// Ctrl+C: offers the result, without separators. An error isn't copied.
    fn copy(&mut self, qh: &QueueHandle<Self>, serial: u32) {
        if self.calc.is_error() {
            return;
        }
        let (Some(manager), Some(device)) = (&self.data_manager, &self.data_device) else {
            return;
        };
        let source = manager.create_copy_paste_source(qh, clipboard::OFFERED);
        source.set_selection(device, serial);
        // Replacing the old source destroys it.
        self.copied = Some((source, self.calc.copy_text()));
        self.notice_until = Some(Instant::now() + NOTICE_TIME);
        self.dirty = true;
    }

    /// Ctrl+V: starts reading the selection; `event_loop` finishes it.
    fn paste(&mut self) {
        // Asking to paste ends "Copied", whether or not anything arrives.
        self.end_notice();
        let Some(offer) = self
            .data_device
            .as_ref()
            .and_then(|d| d.data().selection_offer())
        else {
            return;
        };
        let Some(format) = offer.with_mime_types(clipboard::pick_format) else {
            return;
        };
        // A paste that can't start leaves one still being read alone.
        if let Some(paste) = offer.receive(format).ok().and_then(Paste::new) {
            self.paste = Some(paste);
        }
    }

    /// Takes a paste's read result: keeps reading, or pastes the text.
    fn paste_progress(&mut self, progress: Progress, reading: Paste) -> Option<Paste> {
        match progress {
            Progress::Pending => Some(reading),
            Progress::Done(text) => {
                if self.calc.paste(&text) {
                    self.dirty = true;
                }
                None
            }
            Progress::Dropped => None,
        }
    }

    fn bind_pointer(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) {
        self.input_seat = Some(seat.clone());
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
        // Carrying on hides "Copied" at once.
        self.end_notice();
        self.dirty = true;
    }

    fn end_notice(&mut self) {
        if self.notice_until.take().is_some() {
            self.dirty = true;
        }
    }

    /// Follows a change to the aliasOS text size: new text sizes now, and a
    /// new minimum window. The window keeps its size; the compositor or the
    /// person resizes it.
    fn reload_text_size(&mut self) {
        let text_scale = textsize::load();
        if text_scale != self.text_scale {
            self.text_scale = text_scale;
            self.window.set_min_size(Some(scaled(MIN_SIZE, text_scale)));
            self.dirty = true;
            trace::mark("text size changed");
        }
    }

    /// Reads the theme again. A theme that can't be read keeps the colors
    /// already showing rather than flashing the built-in ones.
    fn reload_theme(&mut self) {
        let Some(palette) = theme::load() else { return };
        if palette != self.palette {
            self.palette = palette;
            self.dirty = true;
            trace::mark("theme changed");
        }
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
            self.text_scale,
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
            notice: self.notice_until.map(|_| "Copied"),
            focus: self.focus.filter(|_| self.focus_visible),
            text_scale: self.text_scale,
        };
        let mut canvas = Canvas::new(canvas, pw as usize, ph as usize);
        let logical = (self.size.0 as f32, self.size.1 as f32);
        ui::draw(
            &mut canvas,
            &mut self.fonts,
            &self.palette,
            &view,
            logical,
            scale,
        );

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
        match capability {
            Capability::Keyboard => self.drop_keyboard(),
            Capability::Pointer => self.drop_pointer(),
            _ => {}
        }
    }

    /// SCTK reports a removed seat only here, without removing its
    /// capabilities first, so the same teardown happens.
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.keyboard_seat.as_ref() == Some(&seat) || self.input_seat.as_ref() == Some(&seat) {
            self.drop_keyboard();
        }
        if self.pointer_seat.as_ref() == Some(&seat) || self.input_seat.as_ref() == Some(&seat) {
            self.drop_pointer();
        }
        if self.input_seat.as_ref() == Some(&seat) {
            self.input_seat = None;
        }
    }
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
        // Modifiers released elsewhere aren't reported either; the next
        // enter sends the real state.
        self.modifiers = Modifiers::default();
    }

    fn press_key(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        let m = self.modifiers;
        // Ctrl+C / Super+C copy and Ctrl+V / Super+V paste, as in omacalc.
        // aliasOS's universal Super+C/V reach apps as Ctrl+C/V, or as
        // Ctrl+Insert / Shift+Insert for some, so those work too.
        if (m.ctrl || m.logo) && !m.alt && !m.shift {
            match event.keysym {
                // Only the main Insert key: Shift or Ctrl with numpad 0 under
                // NumLock is KP_Insert, and that should stay a 0.
                Keysym::c | Keysym::C | Keysym::Insert => self.copy(qh, serial),
                Keysym::v | Keysym::V => self.paste(),
                _ => {}
            }
            return;
        }
        if m.shift && !m.ctrl && !m.alt && !m.logo && event.keysym == Keysym::Insert {
            self.paste();
            return;
        }
        // Other chords belong to the compositor, never to the keypad.
        if m.ctrl || m.alt || m.logo {
            return;
        }
        // Keyboard focus: Tab, Shift+Tab and the arrows move it (and show
        // it); Space presses the focused key.
        let movement = match event.keysym {
            // Shift+Tab is ISO_Left_Tab on standard layouts, Tab with Shift
            // held on others.
            Keysym::Tab if m.shift => Some(ui::Move::Previous),
            Keysym::Tab => Some(ui::Move::Next),
            Keysym::ISO_Left_Tab => Some(ui::Move::Previous),
            Keysym::Up => Some(ui::Move::Up),
            Keysym::Down => Some(ui::Move::Down),
            Keysym::Left => Some(ui::Move::Left),
            Keysym::Right => Some(ui::Move::Right),
            _ => None,
        };
        if let Some(how) = movement {
            // The first press only shows where focus is (the last key
            // clicked, else the first key); later presses move it.
            self.focus = Some(match (self.focus_visible, self.focus) {
                (true, from) => ui::move_focus(from, how),
                (false, Some(cell)) => cell,
                (false, None) => ui::move_focus(None, how),
            });
            self.focus_visible = true;
            self.dirty = true;
            return;
        }
        let key = if event.keysym == Keysym::space {
            match self.focus.filter(|_| self.focus_visible) {
                Some(cell) => ui::action(cell),
                None => return,
            }
        } else {
            let Some(key) = input::key_for(event.keysym) else {
                return;
            };
            key
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
        // A chord starting mid-repeat (Super for a shortcut) mustn't keep
        // deleting digits.
        if modifiers.ctrl || modifiers.alt || modifiers.logo {
            self.repeat = None;
        }
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
        if self.repeat_rate.is_none() {
            self.repeat = None;
        }
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
                // A press that leaves without a release (the compositor
                // took the pointer) is over.
                PointerEventKind::Leave { .. } => self.mouse_down = None,
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => {
                    // Any click ends "Copied", on a key or not.
                    self.end_notice();
                    self.mouse_down = self.button_under_pointer();
                    // A click moves focus to the key, but the ring is for
                    // keyboard users: it hides until the next Tab or arrow.
                    if let Some(cell) = self.mouse_down {
                        self.focus = Some(cell);
                    }
                    if self.focus_visible {
                        self.focus_visible = false;
                        self.dirty = true;
                    }
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

impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &wl_surface::WlSurface,
    ) {
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    // The selection is read only when Ctrl+V asks for it.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for App {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for App {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        _: String,
        mut fd: WritePipe,
    ) {
        use std::io::Write;
        // A number is far smaller than a pipe's buffer, so this never blocks.
        if let Some((ours, text)) = &self.copied
            && ours.inner() == source
        {
            let _ = fd.write_all(text.as_bytes());
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self
            .copied
            .as_ref()
            .is_some_and(|(ours, _)| ours.inner() == source)
        {
            self.copied = None;
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
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
