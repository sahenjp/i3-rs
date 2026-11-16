//! C: src/sync.c (i3 4.25, 44 lines) — the i3 sync protocol.
//!
//! The testsuite sends a ClientMessage with the I3_SYNC atom to the root
//! window; i3 answers with the same random value to the window named in
//! data32[0]. Because i3 processes its event queue strictly in order, a client
//! that received the reply knows that every earlier request was handled.
//!
//! https://i3wm.org/docs/testsuite.html#i3_sync

use crate::backend::XConn;
use crate::xcb_util::Atoms;

use std::sync::Mutex;
use std::sync::OnceLock;

/// C: src/sync.c:13 void sync_respond(xcb_window_t window, uint32_t rnd)
pub fn sync_respond(x: &mut dyn XConn, a: &Atoms, window: u32, rnd: u32) {
    // C: DLOG("[i3 sync protocol] Sending random value %d back to X11
    //         window 0x%08x\n", rnd, window);
    crate::log::debuglog_at(
        file!(),
        line!(),
        &format!(
            "[i3 sync protocol] Sending random value {} back to X11 window {:#010x}\n",
            rnd, window
        ),
    );

    // C: void *reply = scalloc(32, 1) with response_type = XCB_CLIENT_MESSAGE,
    //    window = window, type = A_I3_SYNC, format = 32,
    //    data.data32[0] = window, data.data32[1] = rnd;
    //    xcb_send_event(conn, false, window, XCB_EVENT_MASK_NO_EVENT, reply);
    //    xcb_flush(conn);
    x.send_client_message(
        window,
        false,
        // C: XCB_EVENT_MASK_NO_EVENT (0): deliver only to the window owner
        0,
        a.A_I3_SYNC,
        [window, rnd, 0, 0, 0],
    );
    x.flush();
}

/// The IPC "sync" request (C: src/ipc.c:1369 IPC_HANDLER(sync)) runs in the IPC
/// layer, which has no X connection. C calls sync_respond() right there; the
/// port records the request and the event loop performs it in the same
/// iteration (before any later key/mouse event is dispatched).
fn pending() -> &'static Mutex<Vec<(u32, u32)>> {
    static PENDING: OnceLock<Mutex<Vec<(u32, u32)>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// C: src/ipc.c:1446 sync_respond(state.window, state.rnd) — queued.
pub fn queue_sync_response(window: u32, rnd: u32) {
    if let Ok(mut p) = pending().lock() {
        p.push((window, rnd));
    }
}

/// Drains the queued IPC sync responses (the event loop calls this).
pub fn take_queued_sync_responses() -> Vec<(u32, u32)> {
    match pending().lock() {
        Ok(mut p) => std::mem::take(&mut *p),
        Err(_) => Vec::new(),
    }
}
