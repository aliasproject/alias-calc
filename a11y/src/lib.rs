//! Alias Calc's screen reader bridge: AccessKit, which speaks AT-SPI to
//! Orca, behind a small C interface. alias-calc loads this library with
//! dlopen only when a screen reader is running, so without one AccessKit
//! costs no memory, threads or binary size.
//!
//! The library knows nothing about the calculator: it's given a window
//! title, two texts (the expression and the result, read out when it
//! changes) and a grid of buttons with names and rectangles.
//!
//! AccessKit calls its handlers on its own thread. They queue requests and
//! write to the eventfd alias-calc passed in, and alias-calc takes them with
//! `alias_calc_a11y_next` on its own thread.
//!
//! The interface is versioned by [`ABI`]; alias-calc refuses a library with
//! another version.

use std::fs::File;
use std::io::Write;
use std::os::fd::BorrowedFd;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use accesskit::{
    Action, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Live, Node,
    NodeId, Rect, Role, TreeId, TreeInfo, TreeUpdate,
};

/// Bumped on any change to the functions or structs below.
pub const ABI: u32 = 2;

/// A borrowed UTF-8 string.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Str {
    pub ptr: *const u8,
    pub len: usize,
}

impl Str {
    /// # Safety
    /// `ptr` must point to `len` bytes that live while the result is used.
    unsafe fn get<'a>(self) -> &'a str {
        if self.ptr.is_null() {
            return "";
        }
        // SAFETY: the caller's promise.
        let bytes = unsafe { std::slice::from_raw_parts(self.ptr, self.len) };
        std::str::from_utf8(bytes).unwrap_or("")
    }
}

/// A button: its spoken name and its rectangle in logical pixels.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Button {
    pub name: Str,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// What's on screen.
#[repr(C)]
pub struct State {
    pub title: Str,
    pub expression: Str,
    pub result: Str,
    /// A short status ("Copied"), announced when it appears; empty for none.
    pub status: Str,
    pub buttons: *const Button,
    pub button_count: usize,
    /// The focused button's index, or -1 for the window.
    pub focus: i32,
    pub width: f32,
    pub height: f32,
}

/// What the screen reader asked for, as `alias_calc_a11y_next` returns it.
pub const REQUEST_NONE: u32 = 0;
/// It wants the whole tree: answer with `alias_calc_a11y_update`.
pub const REQUEST_TREE: u32 = 1;
/// Press the button.
pub const REQUEST_PRESS: u32 = 2;
/// Move keyboard focus to the button.
pub const REQUEST_FOCUS: u32 = 3;

#[derive(Debug, PartialEq)]
enum Request {
    Tree,
    Press(u32),
    Focus(u32),
}

pub struct Bridge {
    adapter: accesskit_unix::Adapter,
    requests: Receiver<Request>,
}

/// The interface version this library speaks.
#[unsafe(no_mangle)]
pub extern "C" fn alias_calc_a11y_abi() -> u32 {
    ABI
}

/// Starts the bridge. Requests are signalled by writing 1 to `wake` (an
/// eventfd, duplicated here). Null if `wake` can't be duplicated.
#[unsafe(no_mangle)]
pub extern "C" fn alias_calc_a11y_start(wake: i32) -> *mut Bridge {
    if wake < 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller passes an open fd, which stays open for this call.
    let borrowed = unsafe { BorrowedFd::borrow_raw(wake) };
    let Ok(wake) = borrowed.try_clone_to_owned() else {
        return std::ptr::null_mut();
    };
    let (send, requests) = channel();
    let notify = Notify {
        send: Arc::new(Mutex::new(send)),
        wake: Arc::new(File::from(wake)),
    };
    let adapter = accesskit_unix::Adapter::new(notify.clone(), notify, Inactive);
    Box::into_raw(Box::new(Bridge { adapter, requests }))
}

/// Takes the oldest request: its kind (REQUEST_*) and, for a press or focus,
/// the button's index in `button`. REQUEST_NONE when there's none.
///
/// # Safety
/// `bridge` comes from `alias_calc_a11y_start`; `button` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn alias_calc_a11y_next(bridge: *mut Bridge, button: *mut u32) -> u32 {
    // SAFETY: the caller's promise.
    let Some(bridge) = (unsafe { bridge.as_mut() }) else {
        return REQUEST_NONE;
    };
    let (kind, index) = match bridge.requests.try_recv() {
        Ok(Request::Tree) => (REQUEST_TREE, 0),
        Ok(Request::Press(i)) => (REQUEST_PRESS, i),
        Ok(Request::Focus(i)) => (REQUEST_FOCUS, i),
        Err(_) => (REQUEST_NONE, 0),
    };
    if !button.is_null() {
        // SAFETY: the caller's promise.
        unsafe { *button = index };
    }
    kind
}

/// Sends what's on screen, if a screen reader has asked for the tree.
///
/// # Safety
/// `bridge` comes from `alias_calc_a11y_start`; `state` and everything it
/// points to are valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn alias_calc_a11y_update(bridge: *mut Bridge, state: *const State) {
    // SAFETY: the caller's promise.
    let (Some(bridge), Some(state)) = (unsafe { bridge.as_mut() }, unsafe { state.as_ref() })
    else {
        return;
    };
    // SAFETY: the caller's promise.
    bridge.adapter.update_if_active(|| unsafe { tree(state) });
}

/// Says whether the window has keyboard focus.
///
/// # Safety
/// `bridge` comes from `alias_calc_a11y_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn alias_calc_a11y_window_focused(bridge: *mut Bridge, focused: bool) {
    // SAFETY: the caller's promise.
    if let Some(bridge) = unsafe { bridge.as_mut() } {
        bridge.adapter.update_window_focus_state(focused);
    }
}

/// Queues requests from AccessKit's thread and wakes alias-calc.
#[derive(Clone)]
struct Notify {
    send: Arc<Mutex<Sender<Request>>>,
    wake: Arc<File>,
}

impl Notify {
    fn send(&self, request: Request) {
        let sent = self
            .send
            .lock()
            .is_ok_and(|send| send.send(request).is_ok());
        if sent {
            // An eventfd adds the 8-byte count written to it.
            let _ = (&*self.wake).write(&1u64.to_ne_bytes());
        }
    }
}

impl ActivationHandler for Notify {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        // The state lives on alias-calc's thread: it answers with
        // alias_calc_a11y_update.
        self.send(Request::Tree);
        None
    }
}

impl ActionHandler for Notify {
    fn do_action(&mut self, request: ActionRequest) {
        let Some(index) = button_of(request.target_node) else {
            return;
        };
        match request.action {
            Action::Click => self.send(Request::Press(index)),
            Action::Focus => self.send(Request::Focus(index)),
            _ => {}
        }
    }
}

struct Inactive;

impl DeactivationHandler for Inactive {
    fn deactivate_accessibility(&mut self) {}
}

const WINDOW: NodeId = NodeId(1);
const EXPRESSION: NodeId = NodeId(2);
const RESULT: NodeId = NodeId(3);
const STATUS: NodeId = NodeId(4);
/// Buttons are numbered from here.
const FIRST_BUTTON: u64 = 10;
/// More than any keypad has; bounds what a request can name.
const MAX_BUTTONS: u64 = 256;

fn button_node(index: usize) -> NodeId {
    NodeId(FIRST_BUTTON + index as u64)
}

fn button_of(node: NodeId) -> Option<u32> {
    let index = node.0.checked_sub(FIRST_BUTTON)?;
    (index < MAX_BUTTONS).then_some(index as u32)
}

/// The whole tree: the window, the expression, the result and the buttons.
///
/// # Safety
/// Everything `state` points to is valid.
unsafe fn tree(state: &State) -> TreeUpdate {
    let buttons: &[Button] = if state.buttons.is_null() {
        &[]
    } else {
        // SAFETY: the caller's promise.
        unsafe { std::slice::from_raw_parts(state.buttons, state.button_count) }
    };
    let buttons = &buttons[..buttons.len().min(MAX_BUTTONS as usize)];
    let rect = |x: f32, y: f32, w: f32, h: f32| Rect {
        x0: x as f64,
        y0: y as f64,
        x1: (x + w) as f64,
        y1: (y + h) as f64,
    };
    let mut nodes = Vec::with_capacity(3 + buttons.len());
    let mut children = vec![EXPRESSION, RESULT, STATUS];

    // Labels speak their value. Both are polite live regions, read out
    // when they change: the expression as operators are typed, the result
    // as digits are.
    let mut expression = Node::new(Role::Label);
    // SAFETY: the caller's promise.
    expression.set_value(unsafe { state.expression.get() });
    expression.set_live(Live::Polite);
    nodes.push((EXPRESSION, expression));

    let mut result = Node::new(Role::Label);
    // SAFETY: the caller's promise.
    result.set_value(unsafe { state.result.get() });
    result.set_live(Live::Polite);
    nodes.push((RESULT, result));

    // A status message (WCAG 4.1.3): read when it appears, silent when it
    // empties again. A Label, like the others: AccessKit announces a live
    // node's name, and only a Label's name comes from its value.
    let mut status = Node::new(Role::Label);
    // SAFETY: the caller's promise.
    status.set_value(unsafe { state.status.get() });
    status.set_live(Live::Polite);
    nodes.push((STATUS, status));

    for (index, button) in buttons.iter().enumerate() {
        let mut node = Node::new(Role::Button);
        // SAFETY: the caller's promise.
        node.set_label(unsafe { button.name.get() });
        node.add_action(Action::Click);
        node.add_action(Action::Focus);
        node.set_bounds(rect(button.x, button.y, button.width, button.height));
        children.push(button_node(index));
        nodes.push((button_node(index), node));
    }

    let mut window = Node::new(Role::Window);
    // SAFETY: the caller's promise.
    window.set_label(unsafe { state.title.get() });
    window.set_bounds(rect(0.0, 0.0, state.width, state.height));
    window.set_children(children);
    nodes.push((WINDOW, window));

    let focus = usize::try_from(state.focus)
        .ok()
        .filter(|&i| i < buttons.len())
        .map_or(WINDOW, button_node);
    let mut tree = TreeInfo::new(WINDOW);
    tree.toolkit_name = Some("Alias Calc".into());
    tree.toolkit_version = Some(env!("CARGO_PKG_VERSION").into());
    TreeUpdate {
        nodes,
        tree: Some(tree),
        tree_id: TreeId::ROOT,
        focus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;

    fn s(text: &str) -> Str {
        Str {
            ptr: text.as_ptr(),
            len: text.len(),
        }
    }

    fn update(focus: i32) -> TreeUpdate {
        let names = ["Clear", "7", "Divide"];
        let buttons: Vec<Button> = names
            .iter()
            .enumerate()
            .map(|(i, n)| Button {
                name: s(n),
                x: i as f32 * 60.0,
                y: 100.0,
                width: 54.0,
                height: 54.0,
            })
            .collect();
        let state = State {
            title: s("Calculator"),
            expression: s("12 +"),
            result: s("7"),
            status: s("Copied"),
            buttons: buttons.as_ptr(),
            button_count: buttons.len(),
            focus,
            width: 320.0,
            height: 480.0,
        };
        // SAFETY: everything lives for the call.
        unsafe { tree(&state) }
    }

    #[test]
    fn buttons_are_named_and_pressable() {
        let update = update(-1);
        let buttons: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::Button)
            .collect();
        assert_eq!(buttons.len(), 3);
        for (i, (id, node)) in buttons.iter().enumerate() {
            assert_eq!(button_of(*id), Some(i as u32));
            assert!(node.supports_action(Action::Click));
            assert!(node.supports_action(Action::Focus));
        }
        assert_eq!(buttons[2].1.label(), Some("Divide"));
        let (_, window) = update.nodes.iter().find(|(id, _)| *id == WINDOW).unwrap();
        assert_eq!(window.label(), Some("Calculator"));
        assert_eq!(window.children().len(), 6);
        let (_, status) = update.nodes.iter().find(|(id, _)| *id == STATUS).unwrap();
        // Announced by name, which only a Label takes from its value.
        assert_eq!(status.role(), Role::Label);
        assert_eq!(status.value(), Some("Copied"));
        assert_eq!(status.live(), Some(Live::Polite));
    }

    #[test]
    fn result_is_live_and_focus_follows_the_buttons() {
        let update = update(1);
        let (_, result) = update.nodes.iter().find(|(id, _)| *id == RESULT).unwrap();
        assert_eq!(result.value(), Some("7"));
        assert_eq!(result.live(), Some(Live::Polite));
        let (_, expression) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == EXPRESSION)
            .unwrap();
        assert_eq!(expression.live(), Some(Live::Polite));
        assert_eq!(update.focus, button_node(1));
        assert_eq!(self::update(-1).focus, WINDOW);
        assert_eq!(self::update(7).focus, WINDOW, "out of range is the window");
        assert_eq!(button_of(WINDOW), None);
        assert_eq!(button_of(NodeId(FIRST_BUTTON + MAX_BUTTONS)), None);
    }

    #[test]
    fn requests_arrive_in_order_and_wake_the_loop() {
        let (send, requests) = channel();
        let (read, wake) = std::io::pipe().unwrap();
        let notify = Notify {
            send: Arc::new(Mutex::new(send)),
            wake: Arc::new(File::from(OwnedFd::from(wake))),
        };
        let mut handler = notify.clone();
        handler.do_action(ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: button_node(3),
            data: None,
        });
        handler.do_action(ActionRequest {
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: button_node(5),
            data: None,
        });
        // Not a button: ignored.
        handler.do_action(ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: WINDOW,
            data: None,
        });
        let got: Vec<_> = requests.try_iter().collect();
        assert_eq!(got, [Request::Press(3), Request::Focus(5)]);
        let mut bytes = [0u8; 16];
        let n = std::io::Read::read(&mut &read, &mut bytes).unwrap();
        assert_eq!(n, 16, "one wake per request");
    }
}
