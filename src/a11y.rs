//! Screen reader support. AccessKit, which speaks AT-SPI to Orca, lives in
//! a separate library (`a11y/`, `libalias_calc_a11y.so`) that's loaded with
//! dlopen only when a screen reader is running, checked once after the
//! first frame. Without one, alias-calc stays one thread, 1 MB and 2.4 MiB;
//! with one, the library adds three threads and about 2 MiB. Starting Orca
//! later means relaunching the calculator.
//!
//! The library's thread queues requests and writes to an eventfd that the
//! event loop polls, so the calculator's state is only ever touched on the
//! main thread.

use std::ffi::{CStr, CString, c_char, c_void};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use rustix::event::{EventfdFlags, eventfd};

use crate::engine::{Key, Op};
use crate::ui::{self, KEYS};

/// Whether a screen reader is running: a process named `orca`. Reads only
/// each process's 16-byte `comm`.
pub fn screen_reader_running() -> bool {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return false;
    };
    procs.flatten().any(|entry| {
        let name = entry.file_name();
        name.as_encoded_bytes().iter().all(u8::is_ascii_digit)
            && std::fs::read(entry.path().join("comm")).is_ok_and(|comm| is_screen_reader(&comm))
    })
}

fn is_screen_reader(comm: &[u8]) -> bool {
    comm.strip_suffix(b"\n").unwrap_or(comm) == b"orca"
}

/// What the screen reader asked for.
#[derive(Debug, PartialEq)]
pub enum Request {
    /// It wants the whole tree.
    Tree,
    /// Press the key.
    Press((usize, usize)),
    /// Move keyboard focus to the key.
    Focus((usize, usize)),
}

/// What the screen reader is told.
pub struct State<'a> {
    pub expression: &'a str,
    pub display: &'a str,
    /// The focused key, if any; otherwise the window has focus.
    pub focus: Option<(usize, usize)>,
    /// Each key's rectangle (x, y, width, height) in logical pixels.
    pub keys: [[(f32, f32, f32, f32); 4]; 5],
    pub size: (f32, f32),
}

/// The library's interface version (`ABI` in a11y/src/lib.rs).
const ABI: u32 = 1;
const LIBRARY: &str = "libalias_calc_a11y.so";
const REQUEST_TREE: u32 = 1;
const REQUEST_PRESS: u32 = 2;
const REQUEST_FOCUS: u32 = 3;
const COLUMNS: usize = 4;

/// The library's `Str`, `Button` and `State` (a11y/src/lib.rs).
#[repr(C)]
#[derive(Clone, Copy)]
struct Str {
    ptr: *const u8,
    len: usize,
}

impl From<&str> for Str {
    fn from(text: &str) -> Self {
        Self {
            ptr: text.as_ptr(),
            len: text.len(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Button {
    name: Str,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[repr(C)]
struct CState {
    title: Str,
    expression: Str,
    result: Str,
    buttons: *const Button,
    button_count: usize,
    focus: i32,
    width: f32,
    height: f32,
}

enum Bridge {}

type Start = unsafe extern "C" fn(i32) -> *mut Bridge;
type Next = unsafe extern "C" fn(*mut Bridge, *mut u32) -> u32;
type Update = unsafe extern "C" fn(*mut Bridge, *const CState);
type WindowFocused = unsafe extern "C" fn(*mut Bridge, bool);
type Abi = unsafe extern "C" fn() -> u32;

struct Library {
    start: Start,
    next: Next,
    update: Update,
    window_focused: WindowFocused,
}

const RTLD_NOW: i32 = 2;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

/// Where the library may be: beside the binary (a build tree, or a package
/// that keeps them together), then where the package installs it.
fn library_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        paths.push(dir.join(LIBRARY));
    }
    paths.push(PathBuf::from("/usr/lib/alias-calc").join(LIBRARY));
    paths
}

impl Library {
    /// Loads the first library that loads and speaks this ABI, so a stale
    /// copy beside the binary doesn't hide the installed one. It's never
    /// unloaded: its thread runs until the process ends.
    fn load() -> Result<Self, String> {
        let mut errors = Vec::new();
        for path in library_paths().into_iter().filter(|p| p.exists()) {
            match Self::load_from(&path) {
                Ok(library) => return Ok(library),
                Err(err) => errors.push(err),
            }
        }
        if errors.is_empty() {
            errors.push(format!("{LIBRARY} not found"));
        }
        Err(errors.join("; "))
    }

    fn load_from(path: &std::path::Path) -> Result<Self, String> {
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: a NUL-terminated path; the library has no constructors
        // beyond Rust's own.
        let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            return Err(last_error());
        }
        let symbol = |name: &CStr| {
            // SAFETY: a live handle and a NUL-terminated name.
            let found = unsafe { dlsym(handle, name.as_ptr()) };
            if found.is_null() {
                Err(format!(
                    "{} has no {}",
                    path.display(),
                    name.to_string_lossy()
                ))
            } else {
                Ok(found)
            }
        };
        // SAFETY: each symbol is the function a11y/src/lib.rs exports under
        // that name, with that signature, at the ABI checked first.
        unsafe {
            let abi = std::mem::transmute::<*mut c_void, Abi>(symbol(c"alias_calc_a11y_abi")?);
            let found = abi();
            if found != ABI {
                return Err(format!(
                    "{} speaks version {found}, not {ABI}",
                    path.display()
                ));
            }
            Ok(Self {
                start: std::mem::transmute::<*mut c_void, Start>(symbol(c"alias_calc_a11y_start")?),
                next: std::mem::transmute::<*mut c_void, Next>(symbol(c"alias_calc_a11y_next")?),
                update: std::mem::transmute::<*mut c_void, Update>(symbol(
                    c"alias_calc_a11y_update",
                )?),
                window_focused: std::mem::transmute::<*mut c_void, WindowFocused>(symbol(
                    c"alias_calc_a11y_window_focused",
                )?),
            })
        }
    }
}

fn last_error() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated message.
    let message = unsafe { dlerror() };
    if message.is_null() {
        "dlopen failed".into()
    } else {
        // SAFETY: as above.
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

pub struct Access {
    library: Library,
    bridge: *mut Bridge,
    wake: OwnedFd,
}

impl Access {
    /// Loads the library and starts the bridge. `None`, with the reason on
    /// stderr, when it can't.
    pub fn start() -> Option<Self> {
        let started = (|| {
            let library = Library::load()?;
            let wake = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
                .map_err(|e| e.to_string())?;
            // SAFETY: an open fd, which the library duplicates.
            let bridge = unsafe { (library.start)(wake.as_raw_fd()) };
            if bridge.is_null() {
                return Err("the bridge didn't start".to_string());
            }
            Ok(Self {
                library,
                bridge,
                wake,
            })
        })();
        started
            .map_err(|err| eprintln!("alias-calc: no screen reader support: {err}"))
            .ok()
    }

    /// Readable when there are requests.
    pub fn fd(&self) -> impl AsFd + '_ {
        self.wake.as_fd()
    }

    /// The requests waiting, oldest first. Call when the fd is readable.
    pub fn requests(&mut self) -> Vec<Request> {
        let mut count = [0u8; 8];
        let _ = rustix::io::read(&self.wake, &mut count);
        let mut requests = Vec::new();
        loop {
            let mut button = 0u32;
            // SAFETY: a live bridge and a writable u32.
            let kind = unsafe { (self.library.next)(self.bridge, &mut button) };
            let cell = cell_of_button(button as usize);
            requests.push(match (kind, cell) {
                (REQUEST_TREE, _) => Request::Tree,
                (REQUEST_PRESS, Some(cell)) => Request::Press(cell),
                (REQUEST_FOCUS, Some(cell)) => Request::Focus(cell),
                (REQUEST_PRESS | REQUEST_FOCUS, None) => continue,
                _ => return requests,
            });
        }
    }

    /// Sends what's on screen, if a screen reader has asked for the tree.
    pub fn update(&mut self, state: &State) {
        let buttons: Vec<Button> = (0..KEYS.len() * COLUMNS)
            .map(|index| {
                let (row, col) = (index / COLUMNS, index % COLUMNS);
                let (x, y, width, height) = state.keys[row][col];
                Button {
                    name: spoken_name(ui::action((row, col))).into(),
                    x,
                    y,
                    width,
                    height,
                }
            })
            .collect();
        let c = CState {
            title: crate::window::TITLE.into(),
            expression: state.expression.into(),
            result: state.display.into(),
            buttons: buttons.as_ptr(),
            button_count: buttons.len(),
            focus: state
                .focus
                .map_or(-1, |(row, col)| (row * COLUMNS + col) as i32),
            width: state.size.0,
            height: state.size.1,
        };
        // SAFETY: a live bridge; `c` and what it points to live for the call.
        unsafe { (self.library.update)(self.bridge, &c) };
    }

    pub fn window_focused(&mut self, focused: bool) {
        // SAFETY: a live bridge.
        unsafe { (self.library.window_focused)(self.bridge, focused) };
    }
}

/// The key a button index (reading order) is.
fn cell_of_button(index: usize) -> Option<(usize, usize)> {
    (index < KEYS.len() * COLUMNS).then_some((index / COLUMNS, index % COLUMNS))
}

/// What a screen reader says for a key.
pub fn spoken_name(key: Key) -> &'static str {
    match key {
        Key::Digit(d) => ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
            .get(d as usize)
            .copied()
            .unwrap_or(""),
        Key::Point => "Point",
        Key::Op(Op::Add) => "Add",
        Key::Op(Op::Sub) => "Subtract",
        Key::Op(Op::Mul) => "Multiply",
        Key::Op(Op::Div) => "Divide",
        Key::Op(Op::Pow) => "Power",
        Key::Equals => "Equals",
        Key::Percent => "Percent",
        Key::Sign => "Change sign",
        Key::Backspace => "Backspace",
        Key::Clear => "Clear",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_spoken_name() {
        let mut names = Vec::new();
        for (row, keys) in KEYS.iter().enumerate() {
            for col in 0..keys.len() {
                let name = spoken_name(ui::action((row, col)));
                // Words, not symbols: Orca reads "÷" unevenly.
                assert!(!name.is_empty());
                assert!(
                    name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' '),
                    "{name}"
                );
                names.push(name);
            }
        }
        for name in [
            "Divide",
            "Backspace",
            "Change sign",
            "Clear",
            "Equals",
            "Point",
        ] {
            assert!(names.contains(&name), "no {name}");
        }
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "names repeat");
    }

    #[test]
    fn buttons_map_to_keys_in_reading_order() {
        assert_eq!(cell_of_button(0), Some((0, 0)));
        assert_eq!(cell_of_button(7), Some((1, 3)));
        assert_eq!(cell_of_button(19), Some((4, 3)));
        assert_eq!(cell_of_button(20), None);
    }

    #[test]
    fn finds_orca_by_its_process_name() {
        assert!(is_screen_reader(b"orca\n"));
        assert!(!is_screen_reader(b"orcax\n"));
        assert!(!is_screen_reader(b"alias-calc\n"));
    }

    #[test]
    fn looks_beside_the_binary_first() {
        let paths = library_paths();
        assert_eq!(
            paths.last().unwrap(),
            &PathBuf::from("/usr/lib/alias-calc").join(LIBRARY)
        );
        assert!(paths[0].ends_with(LIBRARY));
    }
}

#[cfg(test)]
mod budget {
    #[test]
    #[ignore = "a measurement: run with --release -- --ignored budget_"]
    fn budget_screen_reader_check() {
        let start = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(super::screen_reader_running());
        }
        let each = start.elapsed() / 20;
        println!("screen reader check: {each:?}");
        // It runs after the first frame, before the first input is read.
        assert!(each.as_millis() < 5, "{each:?}");
    }
}
