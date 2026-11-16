//! 1:1 port of src/drag.c — click and drag.
//!
//! C: src/drag.c
//!
//! The C implementation blocks the main loop: drag_pointer() disables the
//! xcb prepare watcher (main_set_x11_cb(false)) and runs a nested ev_loop with
//! its own prepare watcher. The port keeps the same structure without
//! replacing the process-wide event loop: the drag loop *is* a loop that reads
//! X events through the connection itself (XConn::poll_for_event /
//! XConn::wait_for_event) until the drag ends, dispatching every event it does
//! not own to the regular handler (handle_x11_event_ctx) — which is exactly
//! what C's handle_event() call in drain_drag_events() does.

use crate::backend::{ConBackend, XConn};
use crate::con::{con_by_window_id, con_exists, con_get_workspace};
use crate::data::{ConId, Rect};
use crate::handlers::{button_event_t, handle_x11_event_ctx, EventContext};
use crate::tree::Tree;
use crate::xcb_util::xcursor_cursor_t;

/// C: src/drag.c:13 struct drag_x11_cb (the mutable part of it)
pub struct DragLoop {
    /// C: drag_result_t result
    pub result: drag_result_t,
    /// C: Con *con — the container that is being dragged or resized, or None
    /// if this is a drag of the resize handle.
    pub con: Option<ConId>,
    /// C: Rect old_rect — the dimensions of con when the loop was started.
    pub old_rect: Rect,
    /// C: bool threshold_exceeded — dragging only starts once the pointer
    /// moved past the threshold. Without use_threshold it is always true.
    pub threshold_exceeded: bool,
    /// C: xcb_cursor_t xcursor — cursor to set after the threshold is met.
    pub xcursor: u32,
}

/// C: src/drag.c:16 enum drag_result_t
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum drag_result_t {
    DRAGGING = 0,
    DRAG_SUCCESS = 1,
    DRAG_REVERT = 2,
    DRAG_ABORT = 3,
}

/// The callback the drag loop invokes after every pointer movement.
///
/// C: include/drag.h DRAGGING_CB — the extra void pointer is replaced by the
/// closure's captures (Rust closures carry their own environment).
pub type DragCallback<'a> = dyn FnMut(
    &mut Tree,
    &mut dyn ConBackend,
    &mut dyn XConn,
    &crate::xcb_util::Atoms,
    Option<ConId>,
    &Rect,
    u32,
    u32,
    &button_event_t,
) + 'a;

/// C: src/drag.c:43 threshold_exceeded
///
/// The threshold is about the height of one window decoration.
pub fn threshold_exceeded(x1: u32, y1: u32, x2: u32, y2: u32) -> bool {
    let threshold = libi3::dpi::logical_px(15) as u32;
    let dx = x1.wrapping_sub(x2);
    let dy = y1.wrapping_sub(y2);
    dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy)) > threshold * threshold
}

/// C: XCB_NONE
fn xcb_none() -> u32 {
    0
}

/// C: src/drag.c:50 drain_drag_events
///
/// Returns whether there are no more queued events to drain right now (the C
/// ev_break / return true cases). The first parameter is an event the caller
/// already took out of the queue: the port's replacement for libev having
/// woken the loop up with an event C would still find in the queue.
#[allow(clippy::too_many_arguments)]
fn drain_drag_events(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    dragloop: &mut DragLoop,
    event: &button_event_t,
    first: Option<x11rb::protocol::Event>,
    callback: &mut DragCallback<'_>,
) -> bool {
    use x11rb::protocol::Event;

    let mut pending: std::collections::VecDeque<Event> = first.into_iter().collect();
    let mut last_motion_notify: Option<x11rb::protocol::xproto::MotionNotifyEvent> = None;

    loop {
        let ev = match pending.pop_front().or_else(|| x.poll_for_event()) {
            Some(ev) => ev,
            None => break,
        };

        // C: if (event->response_type == 0) { X11 error, probably harmless }
        // x11rb delivers errors out of band (connection error queue), so the
        // branch has nothing to do here.

        match &ev {
            Event::ButtonPress(_press) => {
                // C: ignoring button %d press during drag initiated by button %d
            }
            Event::ButtonRelease(release) => {
                if event.detail == crate::backend::XCB_BUTTON_INDEX_ANY
                    || release.detail == event.detail
                {
                    dragloop.result = drag_result_t::DRAG_SUCCESS;
                }
            }
            Event::KeyPress(_) => {
                dragloop.result = drag_result_t::DRAG_REVERT;
                handle_x11_event_ctx(t, b, x, a, &ev, ctx);
            }
            Event::UnmapNotify(unmap_event) => {
                if let Some(con) = con_by_window_id(t, unmap_event.window) {
                    let con_ws = con_get_workspace(t, con);
                    let focused_ws = t.focused.and_then(|f| con_get_workspace(t, f));
                    if con_ws.is_some() && con_ws == focused_ws {
                        dragloop.result = drag_result_t::DRAG_ABORT;
                    }
                }
                handle_x11_event_ctx(t, b, x, a, &ev, ctx);
            }
            Event::MotionNotify(motion) => {
                /* motion_notify events are saved for later */
                last_motion_notify = Some(*motion);
            }
            Event::ClientMessage(client_message) => {
                if client_message.type_ == a.A__NET_WM_MOVERESIZE {
                    /* Ignoring _NET_WM_MOVERESIZE request during active drag */
                } else {
                    handle_x11_event_ctx(t, b, x, a, &ev, ctx);
                }
            }
            _ => {
                handle_x11_event_ctx(t, b, x, a, &ev, ctx);
            }
        }

        if dragloop.result != drag_result_t::DRAGGING {
            // C: ev_break(EV_A_ EVBREAK_ONE)
            if dragloop.result == drag_result_t::DRAG_SUCCESS {
                /* Ensure motion notify events are handled. */
                break;
            } else {
                return true;
            }
        }
    }

    let last_motion_notify = match last_motion_notify {
        Some(motion) => motion,
        None => return true,
    };

    let root_x = last_motion_notify.root_x as i32 as u32;
    let root_y = last_motion_notify.root_y as i32 as u32;

    if !dragloop.threshold_exceeded
        && threshold_exceeded(root_x, root_y, event.root_x as u32, event.root_y as u32)
    {
        if dragloop.xcursor != xcb_none() {
            x.change_active_pointer_grab(
                dragloop.xcursor,
                crate::backend::XCB_CURRENT_TIME,
                crate::backend::XCB_EVENT_MASK_BUTTON_RELEASE
                    | crate::backend::XCB_EVENT_MASK_POINTER_MOTION,
            );
        }
        dragloop.threshold_exceeded = true;
    }

    /* Ensure that we are either dragging the resize handle (con is None) or
     * that the container still exists. The latter might not be true, e.g., if
     * the window closed for any reason while the user was dragging it. */
    let con_still_exists = match dragloop.con {
        None => true,
        Some(con) => con_exists(t, con),
    };
    if dragloop.threshold_exceeded && con_still_exists {
        callback(
            t,
            b,
            x,
            a,
            dragloop.con,
            &dragloop.old_rect,
            root_x,
            root_y,
            event,
        );
    }

    x.flush();
    dragloop.result != drag_result_t::DRAGGING
}

/// C: src/drag.c:206 drag_pointer
///
/// This function grabs your pointer and keyboard and lets you drag stuff
/// around (borders). Every time you move your mouse, a MotionNotify event
/// will be received and the given callback will be called with the parameters
/// specified (client, the original event), the original rect of the client,
/// and the new coordinates (x, y).
///
/// If use_threshold is set, dragging only starts after the user moves the
/// pointer past a certain threshold. That is, the cursor will not be set and
/// the callback will not be called until then.
#[allow(clippy::too_many_arguments)]
pub fn drag_pointer(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    con: Option<ConId>,
    event: &button_event_t,
    confine_to: u32,
    cursor: Option<xcursor_cursor_t>,
    use_threshold: bool,
    callback: &mut DragCallback<'_>,
) -> drag_result_t {
    let xcursor = match cursor {
        Some(cursor) => crate::xcb_util::xcursor_get_cursor(x, cursor),
        None => xcb_none(),
    };

    /* Grab the pointer */
    let root = ctx.root;
    let status = x.grab_pointer(
        false, /* get all pointer events specified by the following mask */
        root,  /* grab the root window */
        crate::backend::XCB_EVENT_MASK_BUTTON_RELEASE
            | crate::backend::XCB_EVENT_MASK_POINTER_MOTION,
        crate::backend::XCB_GRAB_MODE_ASYNC, /* pointer events continue as normal */
        crate::backend::XCB_GRAB_MODE_ASYNC, /* keyboard mode */
        confine_to,                          /* in which window should the cursor stay */
        if use_threshold { xcb_none() } else { xcursor },
        crate::backend::XCB_CURRENT_TIME,
    );

    match status {
        None => {
            crate::log::errorlog("Could not grab pointer\n");
            return drag_result_t::DRAG_ABORT;
        }
        Some(status) => {
            if status != crate::backend::XCB_GRAB_STATUS_SUCCESS {
                crate::log::errorlog(&format!("Could not grab pointer (status = {})\n", status));
                return drag_result_t::DRAG_ABORT;
            }
        }
    }

    /* Grab the keyboard */
    let keyb_status = x.grab_keyboard(
        false, /* get all keyboard events */
        root,  /* grab the root window */
        crate::backend::XCB_CURRENT_TIME,
        crate::backend::XCB_GRAB_MODE_ASYNC, /* continue processing pointer events as normal */
        crate::backend::XCB_GRAB_MODE_ASYNC, /* keyboard mode */
    );

    match keyb_status {
        None => {
            crate::log::errorlog("Could not grab keyboard\n");
            x.ungrab_pointer(crate::backend::XCB_CURRENT_TIME);
            return drag_result_t::DRAG_ABORT;
        }
        Some(status) => {
            if status != crate::backend::XCB_GRAB_STATUS_SUCCESS {
                crate::log::errorlog(&format!("Could not grab keyboard (status = {})\n", status));
                x.ungrab_pointer(crate::backend::XCB_CURRENT_TIME);
                return drag_result_t::DRAG_ABORT;
            }
        }
    }

    /* Go into our own event loop */
    let mut dragloop = DragLoop {
        result: drag_result_t::DRAGGING,
        con,
        old_rect: match con {
            Some(con) => t.cons[con].rect,
            None => Rect::default(),
        },
        // C: .threshold_exceeded = !use_threshold
        threshold_exceeded: !use_threshold,
        xcursor,
    };

    loop {
        // C: xcb_drag_prepare_cb — while (!drain_drag_events()), i.e. keep
        // draining as long as events keep coming in.
        loop {
            let done = drain_drag_events(t, b, x, a, ctx, &mut dragloop, event, None, callback);
            if dragloop.result != drag_result_t::DRAGGING || done {
                break;
            }
        }
        if dragloop.result != drag_result_t::DRAGGING {
            break;
        }

        // C: libev blocks on the X file descriptor until the next event; the
        // event that woke it up is handled by the next drain pass.
        let woken = x.wait_for_event();
        if woken.is_none() {
            // Nothing can arrive any more (connection gone): abort like the
            // X error case of C.
            dragloop.result = drag_result_t::DRAG_ABORT;
            break;
        }
        let done = drain_drag_events(t, b, x, a, ctx, &mut dragloop, event, woken, callback);
        if dragloop.result != drag_result_t::DRAGGING {
            break;
        }
        let _ = done;
    }

    x.ungrab_keyboard(crate::backend::XCB_CURRENT_TIME);
    x.ungrab_pointer(crate::backend::XCB_CURRENT_TIME);
    x.flush();

    dragloop.result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::data::{BorderStyle, Rect};
    use crate::xcb_util::{Atoms, RecordingConn};

    fn ctx<'a>() -> EventContext<'a> {
        EventContext {
            wm_sn: 0,
            root: 0x100,
            randr_base: -1,
            xkb_base: -1,
            shape_base: -1,
            shape_supported: true,
            bindings: None,
            xkb_factory: None,
            randr: None,
            ipc: None,
            last_timestamp: 0,
            disable_focus_follows_mouse: false,
            force_xinerama: false,
        }
    }

    /// C: src/drag.c:43 — the threshold is 15 logical pixels, squared.
    #[test]
    fn threshold_is_fifteen_logical_px() {
        assert!(!threshold_exceeded(100, 100, 100, 100));
        assert!(!threshold_exceeded(100, 100, 105, 100));
        // 10 px diagonally is 14.14 px distance < 15
        assert!(!threshold_exceeded(0, 0, 10, 10));
        // 11 px diagonally is 15.56 px distance > 15
        assert!(threshold_exceeded(0, 0, 11, 11));
        // 16 px horizontally exceeds it
        assert!(threshold_exceeded(0, 0, 16, 0));
    }

    #[test]
    fn drag_pointer_aborts_when_the_pointer_grab_fails() {
        // C: src/drag.c:226-236 — a failed grab returns DRAG_ABORT.
        let mut t = Tree::new();
        let mut b = NullBackend;
        crate::tree::tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();
        let mut called = false;
        let mut callback = |_t: &mut Tree,
                            _b: &mut dyn ConBackend,
                            _x: &mut dyn XConn,
                            _a: &Atoms,
                            _con: Option<ConId>,
                            _old: &Rect,
                            _nx: u32,
                            _ny: u32,
                            _event: &button_event_t| {
            called = true;
        };
        let event = button_event_t {
            detail: 1,
            root_x: 10,
            root_y: 10,
            ..Default::default()
        };

        let result = drag_pointer(
            &mut t,
            &mut b,
            &mut x,
            &a,
            &mut c,
            None,
            &event,
            0,
            None,
            true,
            &mut callback,
        );
        assert_eq!(result, drag_result_t::DRAG_ABORT);
        assert!(!called, "the callback never runs without a grab");
        assert!(x.log.iter().any(|l| l.contains("grab_pointer")));
    }

    #[test]
    fn drag_pointer_passes_the_event_mask_and_confine_window_to_the_grab() {
        // C: src/drag.c:216-224 — BUTTON_RELEASE | POINTER_MOTION, ASYNC both,
        // confine_to, and the current time.
        let mut t = Tree::new();
        let mut b = NullBackend;
        crate::tree::tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();
        let mut callback = |_: &mut Tree,
                            _: &mut dyn ConBackend,
                            _: &mut dyn XConn,
                            _: &Atoms,
                            _: Option<ConId>,
                            _: &Rect,
                            _: u32,
                            _: u32,
                            _: &button_event_t| {};
        let event = button_event_t { detail: 1, ..Default::default() };
        let _ = drag_pointer(
            &mut t, &mut b, &mut x, &a, &mut c, None, &event, 0x42, None, true, &mut callback,
        );
        let line = x
            .log
            .iter()
            .find(|l| l.contains("grab_pointer"))
            .cloned()
            .expect("grab_pointer logged");
        let mask = crate::backend::XCB_EVENT_MASK_BUTTON_RELEASE | crate::backend::XCB_EVENT_MASK_POINTER_MOTION;
        assert!(line.contains(&format!("mask={mask:#x}")), "{line}");
        assert!(line.contains("confine=0x42"), "{line}");
        assert!(line.contains("ptr=1") && line.contains("kbd=1"), "{line}");
        let _ = BorderStyle::BS_NORMAL;
    }
}
