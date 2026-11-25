//! The Wayland window: an xdg toplevel drawn on the CPU into a `wl_shm`
//! buffer, sharp at fractional scales through `fractional-scale-v1` and
//! `viewporter`.
//!
//! Everything runs on the one thread. The loop blocks in `poll` on the
//! Wayland socket and only wakes when the compositor sends something, so an
//! idle window costs no CPU at all.

use std::error::Error;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::backend::WaylandError;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::trace;

/// What people see in the launcher and the title bar. "Alias Calc" is the
/// product name; the window says what it does.
const TITLE: &str = "Calculator";
const APP_ID: &str = "com.aliasproject.calc";

/// Logical size when the compositor leaves it to us (a floating window).
const DEFAULT_SIZE: (u32, u32) = (320, 480);
const MIN_SIZE: (u32, u32) = (260, 380);

/// Stand-in until the theme lands (ALI-2103).
const BACKGROUND: [u8; 4] = argb(0x1a, 0x1b, 0x26);

/// `wl_shm` ARGB8888 is little-endian: blue, green, red, alpha in memory.
const fn argb(r: u8, g: u8, b: u8) -> [u8; 4] {
    [b, g, r, 0xff]
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

    let (w, h) = DEFAULT_SIZE;
    let pool = SlotPool::new((w * h * 4) as usize, &shm)?;
    let mut app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm,
        window,
        pool,
        buffer: None,
        size: DEFAULT_SIZE,
        scaling,
        configured: false,
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
        match poll(&mut fds, None) {
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
            Ok(n) => trace::mark(&format!("woke: {n} event(s)")),
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
    /// Logical size, in surface coordinates.
    size: (u32, u32),
    scaling: Scaling,
    configured: bool,
    /// Something changed since the last frame.
    dirty: bool,
    exit: bool,
}

impl App {
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

    /// Draws and commits a frame. An error means the window can't be shown
    /// (no memory for the buffer), which ends the app rather than leaving it
    /// running unseen.
    fn draw(&mut self) -> Result<(), Box<dyn Error>> {
        self.dirty = false;
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

        canvas.as_chunks_mut::<4>().0.fill(BACKGROUND);

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
        trace::mark(&format!("frame committed ({pw}x{ph} px)"));
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

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
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
