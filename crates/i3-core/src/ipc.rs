//! 1:1 port of src/ipc.c — UNIX domain socket IPC (server side):
//! initialization, client handling, protocol.
//!
//! C: src/ipc.c (1740 lines)
//!
//! The C code wires the clients into libev (ev_io read/write callbacks and
//! an ev_timer with a 10 second kill timeout). The event loop integration is
//! a later phase; this module provides the [IpcServer] struct plus the
//! step-wise entry points (accept_clients / handle_readable /
//! ipc_client_timeout) the event loop will drive, and all reply/event
//! payload generation 1:1 (yajl output reproduced byte-exactly by
//! [crate::dump::JsonGen]).
//!
//! The C globals this module reads (current_configpath, included_files,
//! barconfigs, modes, current_binding_mode, i3_version) live in the config /
//! bindings / version layers in C; they are passed in via [IpcState] until
//! those layers exist. TODO(config/bindings): source these values from the
//! real globals once their phases are ported.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};

use crate::backend::ConBackend;
use crate::commands::CommandRunner;
use crate::data::{Binding, InputType};
// Only the test module below names Rect explicitly.
#[cfg(test)]
use crate::data::Rect;
use crate::dump::{
    dump_rect, marshal_workspace_event, tree_payload, window_event_payload, JsonGen,
};
use crate::tree::Tree;

use slotmap::Key as _;

/// C: include/i3/ipc.h:31 — Never change this, only on major IPC breakage.
pub const I3_IPC_MAGIC: &str = "i3-ipc";

/// C: include/i3/ipc.h:82 — the high bit of the message type marks events.
pub const I3_IPC_EVENT_MASK: u32 = 1 << 31;

// Replies from i3 to clients (C: include/i3/ipc.h:79-105)
/// C: include/i3/ipc.h:82
pub const I3_IPC_REPLY_TYPE_COMMAND: u32 = 0;
pub const I3_IPC_REPLY_TYPE_WORKSPACES: u32 = 1;
pub const I3_IPC_REPLY_TYPE_SUBSCRIBE: u32 = 2;
pub const I3_IPC_REPLY_TYPE_OUTPUTS: u32 = 3;
pub const I3_IPC_REPLY_TYPE_TREE: u32 = 4;
pub const I3_IPC_REPLY_TYPE_MARKS: u32 = 5;
pub const I3_IPC_REPLY_TYPE_BAR_CONFIG: u32 = 6;
pub const I3_IPC_REPLY_TYPE_VERSION: u32 = 7;
pub const I3_IPC_REPLY_TYPE_BINDING_MODES: u32 = 8;
pub const I3_IPC_REPLY_TYPE_CONFIG: u32 = 9;
pub const I3_IPC_REPLY_TYPE_TICK: u32 = 10;
pub const I3_IPC_REPLY_TYPE_SYNC: u32 = 11;
pub const I3_IPC_REPLY_TYPE_GET_BINDING_STATE: u32 = 12;

// Events (C: include/i3/ipc.h:106-133)
pub const I3_IPC_EVENT_WORKSPACE: u32 = I3_IPC_EVENT_MASK | 0;
pub const I3_IPC_EVENT_OUTPUT: u32 = I3_IPC_EVENT_MASK | 1;
pub const I3_IPC_EVENT_MODE: u32 = I3_IPC_EVENT_MASK | 2;
pub const I3_IPC_EVENT_WINDOW: u32 = I3_IPC_EVENT_MASK | 3;
pub const I3_IPC_EVENT_BARCONFIG_UPDATE: u32 = I3_IPC_EVENT_MASK | 4;
pub const I3_IPC_EVENT_BINDING: u32 = I3_IPC_EVENT_MASK | 5;
pub const I3_IPC_EVENT_SHUTDOWN: u32 = I3_IPC_EVENT_MASK | 6;
pub const I3_IPC_EVENT_TICK: u32 = I3_IPC_EVENT_MASK | 7;

// Messages from clients to i3 (C: include/i3/ipc.h:33-75)
pub const I3_IPC_MESSAGE_TYPE_COMMAND: u32 = 0;
pub const I3_IPC_MESSAGE_TYPE_RUN_COMMAND: u32 = 0;
pub const I3_IPC_MESSAGE_TYPE_GET_WORKSPACES: u32 = 1;
pub const I3_IPC_MESSAGE_TYPE_SUBSCRIBE: u32 = 2;
pub const I3_IPC_MESSAGE_TYPE_GET_OUTPUTS: u32 = 3;
pub const I3_IPC_MESSAGE_TYPE_GET_TREE: u32 = 4;
pub const I3_IPC_MESSAGE_TYPE_GET_MARKS: u32 = 5;
pub const I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG: u32 = 6;
pub const I3_IPC_MESSAGE_TYPE_GET_VERSION: u32 = 7;
pub const I3_IPC_MESSAGE_TYPE_GET_BINDING_MODES: u32 = 8;
pub const I3_IPC_MESSAGE_TYPE_GET_CONFIG: u32 = 9;
pub const I3_IPC_MESSAGE_TYPE_SEND_TICK: u32 = 10;
pub const I3_IPC_MESSAGE_TYPE_SYNC: u32 = 11;
pub const I3_IPC_MESSAGE_TYPE_GET_BINDING_STATE: u32 = 12;

// C: xcb/xcb.h — the KEY_BUT_MASK bits named in dump_event_state_mask.
pub const XCB_KEY_BUT_MASK_SHIFT: u32 = 1 << 0;
pub const XCB_KEY_BUT_MASK_LOCK: u32 = 1 << 1;
pub const XCB_KEY_BUT_MASK_CONTROL: u32 = 1 << 2;
pub const XCB_KEY_BUT_MASK_MOD_1: u32 = 1 << 3;
pub const XCB_KEY_BUT_MASK_MOD_2: u32 = 1 << 4;
pub const XCB_KEY_BUT_MASK_MOD_3: u32 = 1 << 5;
pub const XCB_KEY_BUT_MASK_MOD_4: u32 = 1 << 6;
pub const XCB_KEY_BUT_MASK_MOD_5: u32 = 1 << 7;
pub const XCB_KEY_BUT_MASK_BUTTON_1: u32 = 1 << 8;
pub const XCB_KEY_BUT_MASK_BUTTON_2: u32 = 1 << 9;
pub const XCB_KEY_BUT_MASK_BUTTON_3: u32 = 1 << 10;
pub const XCB_KEY_BUT_MASK_BUTTON_4: u32 = 1 << 11;
pub const XCB_KEY_BUT_MASK_BUTTON_5: u32 = 1 << 12;

// C: include/data.h:104-107 I3_XKB_GROUP_MASK_1..4 (bit-shifted into the
// upper half by dump_event_state_mask).
pub const I3_XKB_GROUP_MASK_1: u32 = 1 << 0;
pub const I3_XKB_GROUP_MASK_2: u32 = 1 << 1;
pub const I3_XKB_GROUP_MASK_3: u32 = 1 << 2;
pub const I3_XKB_GROUP_MASK_4: u32 = 1 << 3;

// C: dump_event_state_mask matches (I3_XKB_GROUP_MASK_n << 16) — Rust match
// patterns cannot contain arbitrary expressions, so the shifted values are
// precomputed.
pub const I3_XKB_GROUP_MASK_1_SHIFTED: u32 = I3_XKB_GROUP_MASK_1 << 16;
pub const I3_XKB_GROUP_MASK_2_SHIFTED: u32 = I3_XKB_GROUP_MASK_2 << 16;
pub const I3_XKB_GROUP_MASK_3_SHIFTED: u32 = I3_XKB_GROUP_MASK_3 << 16;
pub const I3_XKB_GROUP_MASK_4_SHIFTED: u32 = I3_XKB_GROUP_MASK_4 << 16;

/// C: src/ipc.c:33 'static ev_tstamp kill_timeout = 10.0'
/// C: src/ipc.c:33 `static ev_tstamp kill_timeout = 10.0` — the initial
/// value, overwritten by `ipc_kill_timeout` (C: ipc.c:35
/// ipc_set_kill_timeout, called from config_directives.c:707).
pub const DEFAULT_KILL_TIMEOUT: f64 = 10.0;

/// C: include/data.h shutdown_reason_t (SHUTDOWN_REASON_*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum shutdown_reason_t {
    SHUTDOWN_REASON_RESTART = 0,
    SHUTDOWN_REASON_EXIT = 1,
}

// C: include/configuration.h:79 'struct IncludedFile',
// include/configuration.h:284 'struct Barconfig' (with its bar_mode_t /
// bar_hidden_state_t / bar_position_t enums at :315/:320/:329),
// include/configuration.h:379 'struct bar_colors',
// include/configuration.h:417 'struct Barbinding' and
// include/configuration.h:430 'struct tray_output_t'.
//
// In C there is exactly one definition of each of these structs and both the
// config layer and the IPC handlers work on it (the config layer fills the
// global barconfigs list, ipc.c:758 dump_bar_config reads it). The port
// therefore re-exports the config layer's structs instead of keeping a second
// copy of the same C struct.
pub use crate::config::{
    bar_colors, bar_hidden_state_t, bar_mode_t, bar_position_t, Barbinding, Barconfig,
    IncludedFile, tray_output_t,
};

/// The C globals from src/config.c / src/config_directives.c /
/// src/bindings.c that the IPC handlers read.
/// TODO(config/bindings): replace with the real globals of their phases.
#[derive(Debug, Clone, Default)]
pub struct IpcState {
    /// C: src/config.c 'char *current_configpath'
    pub current_configpath: String,
    /// C: src/config.c 'TAILQ_HEAD(included_files_head, IncludedFile)
    /// included_files'
    pub included_files: VecDeque<IncludedFile>,
    /// C: src/bindings.c 'SLIST_HEAD(modes_head, Mode) modes' (names only;
    /// the payload of GET_BINDING_MODES is exactly the mode names)
    pub modes: VecDeque<String>,
    /// C: src/bindings.c 'char *current_binding_mode'
    pub current_binding_mode: String,
}

/// C: include/i3/ipc.h ipc_client — one connected IPC client.
/// (The ev_io/ev_timer members and the write buffer bookkeeping are handled
/// by the event loop; the events list replaces the C events/num_events
/// pair, with num_events == events.len().)
pub struct IpcClient {
    /// C: int fd (wrapped in the stream for read/write)
    pub stream: UnixStream,
    /// C: uint8_t *buffer + size_t buffer_size — the pending write buffer.
    pub buffer: Vec<u8>,
    /// C: int num_events + char **events (subscriptions)
    pub events: Vec<String>,
    /// C: bool first_tick_sent
    pub first_tick_sent: bool,
    /// C: src/ipc.c:65 ipc_client->timeout (struct ev_timer *) — once the
    /// pending write buffer could not be handed to the socket completely,
    /// ipc_push_pending() arms a timer for kill_timeout seconds and
    /// ipc_client_timeout() frees the client when it fires. i3-core has no
    /// libev, so the instant the timer would fire is recorded here and
    /// [IpcServer::expire_write_timeouts] (called from
    /// [IpcServer::drain_pending_events], i.e. once per loop iteration)
    /// performs the expiry. None == C's "no client->timeout".
    pub write_deadline: Option<std::time::Instant>,
    /// C: src/ipc.c:33 'static ev_tstamp kill_timeout' — the value of the
    /// global at the moment the timer is (re-)armed. ipc_set_kill_timeout()
    /// mirrors it into every client, like C reads the global when arming.
    pub kill_timeout: f64,
}

impl IpcClient {
    /// C: src/ipc.c:98 ipc_send_client_message — create the header, merge
    /// with the payload and append to the client's output buffer; send when
    /// the buffer was empty.
    pub fn send_message(&mut self, payload: &[u8], message_type: u32) {
        let mut message: Vec<u8> = Vec::with_capacity(14 + payload.len());
        message.extend_from_slice(I3_IPC_MAGIC.as_bytes());
        message.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        message.extend_from_slice(&message_type.to_ne_bytes());
        message.extend_from_slice(payload);

        let push_now = self.buffer.is_empty();
        self.buffer.extend_from_slice(&message);

        if push_now {
            self.push_pending();
        }
    }

    /// C: src/ipc.c:45 ipc_push_pending — try to write the pending buffer.
    ///
    /// Mirrors the buffer/timeout bookkeeping of the C function: a complete
    /// write clears the timer, an incomplete write keeps the rest and
    /// (re-)arms the kill timer, and a write error leaves everything to the
    /// next attempt (C returns before touching the timer as well). The ev_io
    /// write callback itself is [IpcServer::flush_pending_writes].
    pub fn push_pending(&mut self) {
        let result = write_all_nonblock(&mut self.stream, &self.buffer);
        let written = match result {
            Ok(written) => written,
            Err(_) => {
                /* C: if (result < 0) { return; } */
                return;
            }
        };

        if written == self.buffer.len() {
            /* Everything was written successfully: clear the timer and stop
             * the io callback. */
            self.buffer.clear();
            self.write_deadline = None;
            return;
        }

        /* Otherwise, make sure that the io callback is enabled and create a
         * new timer if needed. */
        let now = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs_f64(self.kill_timeout.max(0.0));
        if self.write_deadline.is_none() {
            /* C: ev_timer_init(timeout, ipc_client_timeout, kill_timeout, 0.) */
            self.write_deadline = Some(now + timeout);
        } else if written > 0 {
            /* Keep the old timeout when nothing is written. Otherwise, we
             * would keep a dead connection by continuously renewing its
             * timeouts. */
            self.write_deadline = Some(now + timeout);
        }

        /* Shift the buffer to the left (C: memmove + srealloc). A write of 0
         * bytes keeps the whole buffer. */
        self.buffer.drain(..written);
    }
}

/// C: libi3/safewrappers.c:90 writeall_nonblock — writes as much as
/// possible without blocking; returns the number of bytes written (or an
/// error). i3-core does not depend on libi3, so the function is mirrored
/// here for the server side.
fn write_all_nonblock(
    stream: &mut UnixStream,
    buf: &[u8],
) -> std::io::Result<usize> {
    let mut written = 0usize;
    while written < buf.len() {
        match stream.write(&buf[written..]) {
            Ok(0) => break,
            Ok(n) => written += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(written)
}

/// C: libi3/ipc_recv_message.c:29 ipc_recv_message — reads a message from
/// the socket. Returns -1 on read error (EAGAIN keeps errno), -2 on EOF,
/// -3 on protocol violations; 0 on success. i3-core does not depend on
/// libi3, so the fd based function is mirrored on the stream.
/// (Note the 1:1 quirk: the header loop does NOT retry on EAGAIN — a
/// partial header read aborts, exactly like the C callback.)
fn ipc_recv_message(
    stream: &mut UnixStream,
    message_type: Option<&mut u32>,
    reply: &mut Vec<u8>,
) -> i32 {
    // C: const uint32_t to_read = strlen(I3_IPC_MAGIC) + 2 * sizeof(uint32_t);
    let to_read = I3_IPC_MAGIC.len() + 4 + 4;
    let mut msg = [0u8; 14];

    let mut read_bytes = 0usize;
    while read_bytes < to_read {
        match stream.read(&mut msg[read_bytes..to_read]) {
            Ok(0) => {
                if read_bytes == 0 {
                    return -2;
                }
                // C: ELOG("IPC: unexpected EOF while reading header …")
                return -3;
            }
            Ok(n) => read_bytes += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => return -1,
            Err(_) => return -1,
        }
    }

    // C: if (memcmp(walk, I3_IPC_MAGIC, strlen(I3_IPC_MAGIC)) != 0)
    if &msg[0..I3_IPC_MAGIC.len()] != I3_IPC_MAGIC.as_bytes() {
        // C: ELOG("IPC: invalid magic in header …")
        return -3;
    }

    let reply_length =
        u32::from_ne_bytes(msg[I3_IPC_MAGIC.len()..I3_IPC_MAGIC.len() + 4].try_into().unwrap());
    if let Some(mt) = message_type {
        *mt = u32::from_ne_bytes(
            msg[I3_IPC_MAGIC.len() + 4..I3_IPC_MAGIC.len() + 8]
                .try_into()
                .unwrap(),
        );
    }

    reply.clear();
    reply.resize(reply_length as usize, 0);

    let mut read_bytes = 0usize;
    while read_bytes < reply.len() {
        match stream.read(&mut reply[read_bytes..]) {
            Ok(0) => {
                // C: ELOG("IPC: unexpected EOF while reading payload …")
                return -3;
            }
            Ok(n) => read_bytes += n,
            Err(ref e)
                if e.kind() == std::io::ErrorKind::Interrupted
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                continue;
            }
            Err(_) => return -1,
        }
    }

    0
}

/******************************************************************************
 * The event queue for the tree / X layer.
 *
 * C: src/ipc.c:1626 ipc_send_workspace_event and :1642 ipc_send_window_event
 * marshal the payload and hand it to ipc_send_event() right away; the socket
 * and the client list are globals in C, so nothing has to be passed in.
 * In the port the one IpcServer belongs to the event loop, while the tree
 * layer (through ConBackend) and x_push_changes() are the callers. Those
 * callers enqueue the marshalled payload here — the C calls only push bytes
 * towards the subscribers, so queueing and forwarding the same payload in
 * the same order is behaviourally identical — and the loop drains the queue
 * with [IpcServer::drain_pending_events] after every event batch.
 *****************************************************************************/

/// One marshalled IPC event waiting to be forwarded to the subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEvent {
    /// C: the subscription name passed to ipc_send_event ("window" /
    /// "workspace" / "mode" / "binding")
    pub subscription: &'static str,
    /// C: the I3_IPC_EVENT_* message type
    pub message_type: u32,
    /// C: the yajl-generated payload
    pub payload: String,
}

thread_local! {
    /// C: there is no equivalent list — the C call writes to the clients immediately.
    static PENDING_EVENTS: std::cell::RefCell<Vec<PendingEvent>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

/// C: src/ipc.c:1642 ipc_send_window_event — the payload of a window event is
/// marshalled now (it needs the tree) and forwarded by the event loop.
pub fn queue_window_event(t: &Tree, property: &str, con: crate::ConId) {
    let payload = window_event_payload(t, property, con);
    PENDING_EVENTS.with(|q| {
        q.borrow_mut().push(PendingEvent {
            subscription: "window",
            message_type: I3_IPC_EVENT_WINDOW,
            payload,
        })
    });
}

/// C: src/ipc.c:1626 ipc_send_workspace_event — queued like
/// [queue_window_event].
///
/// `current` is the `Con` in the event's "current" field and `old` is the
/// previously focused workspace (C: `Con *current, Con *old`), which lands in
/// the "old" field.
pub fn queue_workspace_event(
    t: &Tree,
    change: &str,
    current: Option<crate::ConId>,
    old: Option<crate::ConId>,
) {
    let payload = marshal_workspace_event(t, change, current, old);
    PENDING_EVENTS.with(|q| {
        q.borrow_mut().push(PendingEvent {
            subscription: "workspace",
            message_type: I3_IPC_EVENT_WORKSPACE,
            payload,
        })
    });
}

/// C: src/ipc.c:1694 ipc_send_binding_event("run", …) — the binding event is
/// queued like the window/workspace events, because bindings.c cannot reach
/// the IpcServer that the event loop owns.
pub fn queue_binding_event(event_type: &str, bind: &Binding, modename: Option<&str>) {
    let payload = binding_event_payload(event_type, bind, modename);
    PENDING_EVENTS.with(|q| {
        q.borrow_mut().push(PendingEvent {
            subscription: "binding",
            message_type: I3_IPC_EVENT_BINDING,
            payload,
        })
    });
}

/// C: src/bindings.c:665-672 — the mode event that switch_mode() sends right
/// after the new mode's bindings are grabbed (ipc_send_event("mode",
/// I3_IPC_EVENT_MODE, event_msg)).
pub fn queue_mode_event(mode: &crate::config::Mode) {
    let payload = mode_event_payload(mode);
    PENDING_EVENTS.with(|q| {
        q.borrow_mut().push(PendingEvent {
            subscription: "mode",
            message_type: I3_IPC_EVENT_MODE,
            payload,
        })
    });
}

/// C: src/ipc.c:1673 ipc_send_barconfig_update_event — the dumped barconfig is
/// queued like the window/workspace events, because the config/command layer
/// (src/commands.c:1690 cmd_reload, cmd_bar_mode, cmd_bar_hidden_state) cannot
/// reach the IpcServer that the event loop owns.
pub fn queue_barconfig_update_event(t: &Tree, barconfig: &Barconfig) {
    let mut gen = JsonGen::new();
    dump_bar_config(t, &mut gen, barconfig);
    let payload = gen.get_buf().to_string();
    PENDING_EVENTS.with(|q| {
        q.borrow_mut().push(PendingEvent {
            subscription: "barconfig_update",
            message_type: I3_IPC_EVENT_BARCONFIG_UPDATE,
            payload,
        })
    });
}

/// The bindings.c side effects that are really ipc.c calls.
///
/// C: src/bindings.c:669 ipc_send_mode_event() and src/bindings.c:902
/// ipc_send_binding_event() write to the global IPC clients directly; the port
/// queues the two marshalled events (see [queue_mode_event] /
/// [queue_binding_event]) and the event loop forwards them with
/// [IpcServer::drain_pending_events].
#[derive(Debug, Clone, Copy, Default)]
pub struct IpcHooks;

impl crate::bindings::BindingHooks for IpcHooks {
    fn ipc_send_mode_event(&mut self, mode: &crate::config::Mode) {
        queue_mode_event(mode);
    }

    fn ipc_send_binding_event(&mut self, bind: &Binding, modename: Option<&str>) {
        // C: src/bindings.c:902 ipc_send_binding_event("run", bind_cp, modename);
        queue_binding_event("run", bind, modename);
    }
}

/// Removes and returns the queued events in the order they were sent.
pub fn take_pending_events() -> Vec<PendingEvent> {
    PENDING_EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Drops the queued events without sending them (used by the tests).
pub fn clear_pending_events() {
    PENDING_EVENTS.with(|q| q.borrow_mut().clear());
}

/// C: src/ipc.c:1694-1730 — the yajl payload of a "binding" event.
fn binding_event_payload(event_type: &str, bind: &Binding, modename: Option<&str>) -> String {
    let mut gen = JsonGen::new();

    gen.map_open();

    gen.string("change");
    gen.string(event_type);

    gen.string("mode");
    match modename {
        None => gen.string("default"),
        Some(modename) => gen.string(modename),
    }

    gen.string("binding");
    dump_binding(&mut gen, bind);

    gen.map_close();

    gen.take_buf()
}

/// C: src/bindings.c:665 — sasprintf(&event_msg, "{"change":"%s",
/// "pango_markup":%s}", mode->name, mode->pango_markup ? "true" : "false");
///
/// The C string is rebuilt verbatim (including the space after the comma and
/// without escaping the mode name, exactly like sasprintf).
fn mode_event_payload(mode: &crate::config::Mode) -> String {
    format!(
        "{{\"change\":\"{}\", \"pango_markup\":{}}}",
        mode.name,
        if mode.pango_markup { "true" } else { "false" }
    )
}

/// The IPC server: the listening socket plus the TAILQ of all clients
/// (C: src/ipc.c:28 'TAILQ_HEAD(ipc_client_head, ipc_client) all_clients')
/// and the C globals the handlers need.
pub struct IpcServer {
    /// C: src/ipc.c the listening socket (ev_io in C)
    pub listener: UnixListener,
    /// C: src/main.c:1030-1053 — additional listening sockets that systemd
    /// style socket activation passed in fd 3..3+LISTEN_FDS. C registers one
    /// ev_io per fd; the port keeps them in this Vec and accepts from all of
    /// them in accept_client().
    pub activation_listeners: Vec<UnixListener>,
    /// C: src/ipc.c:26 'char *current_socketpath'
    pub socketpath: Option<String>,
    /// C: src/ipc.c:33 kill_timeout
    pub kill_timeout: f64,
    /// C: TAILQ all_clients, insertion ordered (TAILQ_INSERT_TAIL)
    pub clients: Vec<IpcClient>,
    /// The C globals (config/bindings layers)
    pub state: IpcState,
}

impl IpcServer {
    /// C: src/main.c ipc_listen socket setup (create_socket + listen);
    /// the socket file handling of libi3/create_socket.c is mirrored here.
    pub fn bind(path: &str) -> std::io::Result<IpcServer> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        // C: the listening fd is registered with libev (ev_io) and therefore
        // never blocks; accept() is only called when it is readable. Rust's
        // event loop polls the same way, so the listener must be
        // non-blocking here as well — otherwise the "accept until no more"
        // loop below would park the whole WM in the second accept().
        listener.set_nonblocking(true)?;
        Ok(IpcServer {
            listener,
            activation_listeners: Vec::new(),
            socketpath: Some(path.to_string()),
            kill_timeout: DEFAULT_KILL_TIMEOUT,
            clients: Vec::new(),
            state: IpcState::default(),
        })
    }

    /// Mirrors the config/bindings globals the IPC handlers read into
    /// IpcState. In C these are process-wide variables that the handlers use
    /// directly, and load_configuration() writes them in place:
    ///
    /// * src/config.c:16 `char *current_configpath` — assigned at
    ///   src/config.c:235 (`current_configpath = get_config_path(...)`).
    /// * src/config.c:18 `struct modes_head modes` — SLIST_INIT at
    ///   src/config.c:170, filled through bindings.c:32 `mode_from_name`.
    /// * src/bindings.c `char *current_binding_mode` — assigned at
    ///   src/config.c:179 (the default mode) and bindings.c:651 (`mode`
    ///   command / configuration-time `mode` blocks).
    /// * src/config.c:20 `struct includedfiles_head included_files` — the
    ///   top-level file is inserted at src/config.c:259 and every included
    ///   file at src/config_directives.c:53.
    /// * src/config_directives.c `struct barconfigs_head barconfigs` — the
    ///   list the bar blocks are appended to.
    ///
    /// GET_VERSION (ipc.c:1081), GET_CONFIG (ipc.c:1283),
    /// GET_BINDING_MODES (ipc.c:1188), GET_BINDING_STATE (ipc.c:1398) and
    /// GET_BAR_CONFIG (ipc.c:1109) read exactly these globals. The port keeps
    /// them in ConfigDirectives, so the startup sequence copies them once
    /// after load_configuration() returns (src/main.c:687). A reload
    /// (commands.c:1698 `cmd_reload`) has to call this again, like the C
    /// globals are rewritten by the second load_configuration() call.
    pub fn sync_config_state(&mut self, d: &crate::config_directives::ConfigDirectives) {
        self.state.current_configpath = d.current_configpath.clone().unwrap_or_default();
        self.state.included_files = d.included_files.clone();
        self.state.modes = d.modes.iter().map(|mode| mode.name.clone()).collect();
        self.state.current_binding_mode = d.current_binding_mode.clone().unwrap_or_default();
    }

    /// C: src/ipc.c:35 ipc_set_kill_timeout
    pub fn ipc_set_kill_timeout(&mut self, new: f64) {
        self.kill_timeout = new;
        // C reads the global kill_timeout when ipc_push_pending() arms a
        // timer; the port hands the value to the clients up front so
        // IpcClient::push_pending (which never sees the IpcServer) arms the
        // same duration.
        for client in &mut self.clients {
            client.kill_timeout = new;
        }
    }

    /// C: src/main.c:1030-1053 — adopt a socket-activation listener. C only
    /// clears FD_CLOEXEC (so an in-place restart can keep the descriptor) and
    /// registers an ev_io; the port additionally switches it to non-blocking
    /// for the same reason the main listener is non-blocking.
    pub fn add_activation_listener(&mut self, fd: std::os::unix::io::RawFd) {
        // C: fcntl(fd, F_SETFD, flags & ~FD_CLOEXEC)
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                crate::log::errorlog(&format!(
                    "Could not disable FD_CLOEXEC on fd {}\n",
                    fd
                ));
            }
        }
        use std::os::unix::io::FromRawFd;
        let listener = unsafe { UnixListener::from_raw_fd(fd) };
        let _ = listener.set_nonblocking(true);
        self.activation_listeners.push(listener);
    }

    /// C: src/ipc.c:1540 ipc_new_client — accept()s a new client and
    /// registers it. Returns the index of the new client in clients.
    pub fn accept_client(&mut self) -> Option<usize> {
        match self.listener.accept() {
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => return None,
            Err(_) => {}
            Ok((stream, _peer)) => {
                // C: (void)fcntl(fd, F_SETFD, FD_CLOEXEC);
                return self.ipc_new_client_on_fd(stream);
            }
        }
        // C: one ev_io per socket-activation fd, all calling ipc_new_client.
        let mut accepted = None;
        for i in 0..self.activation_listeners.len() {
            if let Ok((stream, _peer)) = self.activation_listeners[i].accept() {
                accepted = self.ipc_new_client_on_fd(stream);
                break;
            }
        }
        accepted
    }

    /// C: src/ipc.c:1565 ipc_new_client_on_fd
    pub fn ipc_new_client_on_fd(&mut self, stream: UnixStream) -> Option<usize> {
        let _ = stream.set_nonblocking(true);

        let client = IpcClient {
            stream,
            buffer: Vec::new(),
            events: Vec::new(),
            first_tick_sent: false,
            write_deadline: None,
            kill_timeout: self.kill_timeout,
        };

        self.clients.push(client);
        Some(self.clients.len() - 1)
    }

    /// C: src/ipc.c:117 free_ipc_client — removes the client (exempt_fd is
    /// never closed; pass None to close all fds).
    pub fn free_ipc_client(&mut self, client_index: usize, exempt_fd: Option<i32>) {
        if let Some(client) = self.clients.get(client_index) {
            if Some(client.stream.as_raw_fd()) != exempt_fd {
                // C: close(client->fd) — dropping the stream closes it.
            }
        }
        if client_index < self.clients.len() {
            self.clients.remove(client_index);
        }
    }

    /// C: src/ipc.c:147 ipc_send_event — sends the event to all clients
    /// subscribed to it (strcasecmp match).
    pub fn ipc_send_event(&mut self, event: &str, message_type: u32, payload: &str) {
        for current in &mut self.clients {
            for subscribed in &current.events.clone() {
                if subscribed.eq_ignore_ascii_case(event) {
                    current.send_message(payload.as_bytes(), message_type);
                    break;
                }
            }
        }
    }

    /// C: src/ipc.c:162 ipc_send_shutdown_event
    pub fn ipc_send_shutdown_event(&mut self, reason: shutdown_reason_t) {
        let mut gen = JsonGen::new();
        gen.map_open();

        gen.string("change");

        if reason == shutdown_reason_t::SHUTDOWN_REASON_RESTART {
            gen.string("restart");
        } else if reason == shutdown_reason_t::SHUTDOWN_REASON_EXIT {
            gen.string("exit");
        }

        gen.map_close();

        self.ipc_send_event("shutdown", I3_IPC_EVENT_SHUTDOWN, gen.get_buf());
    }

    /// C: src/ipc.c:192 ipc_shutdown — calls shutdown() on each socket and
    /// closes it. To be called when exiting or restarting only!
    pub fn ipc_shutdown(&mut self, reason: shutdown_reason_t, exempt_fd: Option<i32>) {
        self.ipc_send_shutdown_event(reason);

        while !self.clients.is_empty() {
            if let Some(current) = self.clients.first() {
                if Some(current.stream.as_raw_fd()) != exempt_fd {
                    let _ = current.stream.shutdown(std::net::Shutdown::Both);
                }
            }
            self.free_ipc_client(0, exempt_fd);
        }
    }

    /// C: src/ipc.c:1626 ipc_send_workspace_event
    pub fn ipc_send_workspace_event(
        &mut self,
        t: &Tree,
        change: &str,
        current: Option<crate::ConId>,
        old: Option<crate::ConId>,
    ) {
        let payload = marshal_workspace_event(t, change, current, old);
        self.ipc_send_event("workspace", I3_IPC_EVENT_WORKSPACE, &payload);
    }

    /// C: src/ipc.c:1642 ipc_send_window_event
    pub fn ipc_send_window_event(
        &mut self,
        t: &Tree,
        property: &str,
        con: crate::ConId,
    ) {
        let payload = window_event_payload(t, property, con);
        self.ipc_send_event("window", I3_IPC_EVENT_WINDOW, &payload);
    }

    /// Sends every event the tree/X layer queued since the last call
    /// (see [queue_window_event] / [queue_workspace_event]).
    ///
    /// C: src/ipc.c:1626/1642 call ipc_send_event() directly because the IPC
    /// socket is a process-wide global. The port keeps the one IpcServer in
    /// the event loop, so the tree and X layers enqueue the marshalled
    /// payload and the loop forwards it here — the event reaches the
    /// subscribers with the same payload and at the same point in the
    /// iteration (the C calls also only reach the socket, they are never
    /// read back by i3 itself).
    pub fn drain_pending_events(&mut self) {
        for event in take_pending_events() {
            self.ipc_send_event(event.subscription, event.message_type, &event.payload);
        }

        /* C: once the handlers have returned, libev runs the fd/timer
         * callbacks of the iteration before the loop blocks again:
         * src/ipc.c:1526 ipc_socket_writeable_cb (retry the pending write)
         * and src/ipc.c:1477 ipc_client_timeout (kill a client that did not
         * accept its buffer within kill_timeout). The port has no libev, so
         * both run here — this function is the per-iteration point the event
         * loop calls (bins/i3/src/server.rs). */
        self.flush_pending_writes();
        self.expire_write_timeouts(std::time::Instant::now());
    }

    /// C: src/ipc.c:1526 ipc_socket_writeable_cb — libev calls it whenever a
    /// client's fd became writable while it still had a pending buffer; the
    /// port retries every non-empty buffer once per loop iteration.
    pub fn flush_pending_writes(&mut self) {
        for client in &mut self.clients {
            if !client.buffer.is_empty() {
                client.push_pending();
            }
        }
    }

    /// C: src/ipc.c:1477 ipc_client_timeout — frees every client whose
    /// pending write outlived kill_timeout (abi: the ev_timer the write path
    /// armed). Called from [drain_pending_events].
    pub fn expire_write_timeouts(&mut self, now: std::time::Instant) {
        let mut index = 0;
        while index < self.clients.len() {
            let expired = self.clients[index]
                .write_deadline
                .map_or(false, |deadline| deadline <= now);
            if expired {
                ipc_client_timeout(self, index);
            } else {
                index += 1;
            }
        }
    }

    /// C: src/ipc.c:1673 ipc_send_barconfig_update_event
    ///
    /// dump_bar_config canonicalizes the configured output names through
    /// get_output_by_name (C: src/ipc.c:749), which needs the output table of
    /// the tree; the C code reaches it through the outputs global.
    pub fn ipc_send_barconfig_update_event(&mut self, t: &Tree, barconfig: &Barconfig) {
        let mut gen = JsonGen::new();
        dump_bar_config(t, &mut gen, barconfig);
        self.ipc_send_event(
            "barconfig_update",
            I3_IPC_EVENT_BARCONFIG_UPDATE,
            gen.get_buf(),
        );
    }

    /// C: src/ipc.c:1694 ipc_send_binding_event
    pub fn ipc_send_binding_event(
        &mut self,
        event_type: &str,
        bind: &Binding,
        modename: Option<&str>,
    ) {
        let payload = binding_event_payload(event_type, bind, modename);
        self.ipc_send_event("binding", I3_IPC_EVENT_BINDING, &payload);
    }

    /// C: src/ipc.c:1733 ipc_confirm_restart
    pub fn ipc_confirm_restart(&mut self, client_index: usize) {
        let reply = r#"[{"success":true}]"#;
        if let Some(client) = self.clients.get_mut(client_index) {
            client.send_message(reply.as_bytes(), I3_IPC_REPLY_TYPE_COMMAND);
            client.push_pending();
        }
    }
}

/// C: src/ipc.c:270 dump_event_state_mask — the XCB_KEY_BUT_MASK_* /
/// I3_XKB_GROUP_MASK_* bits become modifier names.
fn dump_event_state_mask(gen: &mut JsonGen, bind: &Binding) {
    gen.array_open();
    for i in 0..20 {
        if bind.event_state_mask & (1 << i) != 0 {
            match 1u32 << i {
                XCB_KEY_BUT_MASK_SHIFT => gen.string("shift"),
                XCB_KEY_BUT_MASK_LOCK => gen.string("lock"),
                XCB_KEY_BUT_MASK_CONTROL => gen.string("ctrl"),
                XCB_KEY_BUT_MASK_MOD_1 => gen.string("Mod1"),
                XCB_KEY_BUT_MASK_MOD_2 => gen.string("Mod2"),
                XCB_KEY_BUT_MASK_MOD_3 => gen.string("Mod3"),
                XCB_KEY_BUT_MASK_MOD_4 => gen.string("Mod4"),
                XCB_KEY_BUT_MASK_MOD_5 => gen.string("Mod5"),
                XCB_KEY_BUT_MASK_BUTTON_1 => gen.string("Button1"),
                XCB_KEY_BUT_MASK_BUTTON_2 => gen.string("Button2"),
                XCB_KEY_BUT_MASK_BUTTON_3 => gen.string("Button3"),
                XCB_KEY_BUT_MASK_BUTTON_4 => gen.string("Button4"),
                XCB_KEY_BUT_MASK_BUTTON_5 => gen.string("Button5"),
                I3_XKB_GROUP_MASK_1_SHIFTED => gen.string("Group1"),
                I3_XKB_GROUP_MASK_2_SHIFTED => gen.string("Group2"),
                I3_XKB_GROUP_MASK_3_SHIFTED => gen.string("Group3"),
                I3_XKB_GROUP_MASK_4_SHIFTED => gen.string("Group4"),
                _ => {}
            }
        }
    }
    gen.array_close();
}

/// C: src/ipc.c:332 dump_binding
fn dump_binding(gen: &mut JsonGen, bind: &Binding) {
    gen.map_open();
    gen.string("input_code");
    gen.integer(bind.keycode as i64);

    gen.string("input_type");
    gen.string(if bind.input_type == InputType::B_KEYBOARD {
        "keyboard"
    } else {
        "mouse"
    });

    gen.string("symbol");
    match &bind.symbol {
        None => gen.null(),
        Some(symbol) => gen.string(symbol),
    }

    gen.string("command");
    gen.string(&bind.command);

    // This key is only provided for compatibility, new programs should use
    // event_state_mask instead.
    gen.string("mods");
    dump_event_state_mask(gen, bind);

    gen.string("event_state_mask");
    dump_event_state_mask(gen, bind);

    gen.map_close();
}

/// C: src/ipc.c:724 dump_bar_bindings
fn dump_bar_bindings(gen: &mut JsonGen, config: &Barconfig) {
    if config.bar_bindings.is_empty() {
        return;
    }

    gen.string("bindings");
    gen.array_open();

    for current in &config.bar_bindings {
        gen.map_open();

        gen.string("input_code");
        gen.integer(current.input_code as i64);
        gen.string("command");
        gen.string(&current.command);
        gen.string("release");
        gen.boolean(current.release);

        gen.map_close();
    }

    gen.array_close();
}

/// C: src/ipc.c:749 canonicalize_output_name
fn canonicalize_output_name(t: &Tree, name: &str) -> String {
    /* Do not canonicalize special output names. */
    if name.eq_ignore_ascii_case("primary") || name.eq_ignore_ascii_case("nonprimary") {
        return name.to_string();
    }
    // C: Output *output = get_output_by_name(name, false);
    //    return output ? output_primary_name(output) : name;
    match crate::output::get_output_by_name(t, name, false) {
        Some(output) => crate::output::output_primary_name(t, output),
        None => name.to_string(),
    }
}

/// C: src/ipc.c:758 dump_bar_config
fn dump_bar_config(t: &Tree, gen: &mut JsonGen, config: &Barconfig) {
    gen.map_open();

    gen.string("id");
    // C: ystr(config->id) — a configured bar always has an id
    // (config_directives.c), the Option only covers the zeroed struct.
    gen.string(config.id.as_deref().unwrap_or(""));

    if config.num_outputs > 0 {
        gen.string("outputs");
        gen.array_open();
        for c in 0..config.num_outputs {
            /* Convert monitor names (RandR ≥ 1.5) or output names
             * (RandR < 1.5) into monitor names. This way, existing configs
             * which use output names transparently keep working. */
            let name = config.outputs.get(c as usize).map(|s| s.as_str()).unwrap_or("");
            gen.string(&canonicalize_output_name(t, name));
        }
        gen.array_close();
    }

    if !config.tray_outputs.is_empty() {
        gen.string("tray_outputs");
        gen.array_open();

        for tray_output in &config.tray_outputs {
            gen.string(&canonicalize_output_name(t, &tray_output.output));
        }

        gen.array_close();
    }

    gen.string("tray_padding");
    gen.integer(config.tray_padding as i64);

    if let Some(socket_path) = &config.socket_path {
        gen.string("socket_path");
        gen.string(socket_path);
    }

    gen.string("mode");
    match config.mode {
        bar_mode_t::M_HIDE => gen.string("hide"),
        bar_mode_t::M_INVISIBLE => gen.string("invisible"),
        _ => gen.string("dock"), // M_DOCK and default
    }

    gen.string("hidden_state");
    match config.hidden_state {
        bar_hidden_state_t::S_SHOW => gen.string("show"),
        _ => gen.string("hide"), // S_HIDE and default
    }

    gen.string("modifier");
    gen.integer(config.modifier as i64);

    dump_bar_bindings(gen, config);

    gen.string("position");
    if config.position == bar_position_t::P_BOTTOM {
        gen.string("bottom");
    } else {
        gen.string("top");
    }

    if let Some(status_command) = &config.status_command {
        gen.string("status_command");
        gen.string(status_command);
    }
    if let Some(workspace_command) = &config.workspace_command {
        gen.string("workspace_command");
        gen.string(workspace_command);
    }
    if let Some(font) = &config.font {
        gen.string("font");
        gen.string(font);
    }

    if config.bar_height != 0 {
        gen.string("bar_height");
        gen.integer(config.bar_height as i64);
    }

    dump_rect(gen, "padding", config.padding);

    if let Some(separator_symbol) = &config.separator_symbol {
        gen.string("separator_symbol");
        gen.string(separator_symbol);
    }

    gen.string("workspace_buttons");
    gen.boolean(!config.hide_workspace_buttons);

    gen.string("workspace_min_width");
    gen.integer(config.workspace_min_width as i64);

    gen.string("strip_workspace_numbers");
    gen.boolean(config.strip_workspace_numbers);

    gen.string("strip_workspace_name");
    gen.boolean(config.strip_workspace_name);

    gen.string("binding_mode_indicator");
    gen.boolean(!config.hide_binding_mode_indicator);

    gen.string("verbose");
    gen.boolean(config.verbose);

    gen.string("colors");
    gen.map_open();
    let colors = &config.colors;
    macro_rules! YSTR_IF_SET {
        ($name:ident) => {
            if let Some(v) = &colors.$name {
                gen.string(stringify!($name));
                gen.string(v);
            }
        };
    }
    YSTR_IF_SET!(background);
    YSTR_IF_SET!(statusline);
    YSTR_IF_SET!(separator);
    YSTR_IF_SET!(focused_background);
    YSTR_IF_SET!(focused_statusline);
    YSTR_IF_SET!(focused_separator);
    YSTR_IF_SET!(focused_workspace_border);
    YSTR_IF_SET!(focused_workspace_bg);
    YSTR_IF_SET!(focused_workspace_text);
    YSTR_IF_SET!(active_workspace_border);
    YSTR_IF_SET!(active_workspace_bg);
    YSTR_IF_SET!(active_workspace_text);
    YSTR_IF_SET!(inactive_workspace_border);
    YSTR_IF_SET!(inactive_workspace_bg);
    YSTR_IF_SET!(inactive_workspace_text);
    YSTR_IF_SET!(urgent_workspace_border);
    YSTR_IF_SET!(urgent_workspace_bg);
    YSTR_IF_SET!(urgent_workspace_text);
    YSTR_IF_SET!(binding_mode_border);
    YSTR_IF_SET!(binding_mode_bg);
    YSTR_IF_SET!(binding_mode_text);
    gen.map_close();

    gen.map_close();
}

// ---------------------------------------------------------------------------
// IPC_HANDLER table (C: src/ipc.c:1418 handlers[13])
// ---------------------------------------------------------------------------

/// C: src/ipc.c:208 IPC_HANDLER(run_command)
fn handle_run_command(
    server: &mut IpcServer,
    client_index: usize,
    t: &mut Tree,
    b: &mut dyn ConBackend,
    message: &[u8],
) {
    let command = String::from_utf8_lossy(message).into_owned();
    // C: LOG("IPC: received: *%.4000s*\n", command);
    let gen = JsonGen::new();

    let client_fd = server
        .clients
        .get(client_index)
        .map(|c| c.stream.as_raw_fd());
    let mut runner = CommandRunner::new(t, b, Some(gen), client_fd);
    let result = runner.run(&command);

    if result.needs_tree_render {
        // C: tree_render() — reborrows the runner's tree/backend.
        crate::tree::tree_render(runner.t, runner.b);
    }

    let mut gen = runner.take_gen();
    let reply = gen.as_mut().map(|g| g.take_buf()).unwrap_or_default();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(reply.as_bytes(), I3_IPC_REPLY_TYPE_COMMAND);
    }
}

/// C: src/ipc.c:932 IPC_HANDLER(get_workspaces)
fn handle_get_workspaces(server: &mut IpcServer, client_index: usize, t: &Tree) {
    let mut gen = JsonGen::new();
    gen.array_open();

    let focused_ws = t
        .focused
        .and_then(|f| crate::con::con_get_workspace(t, f));

    let croot = t.croot.expect("get_workspaces: no croot");
    let outputs: Vec<crate::ConId> = t.cons[croot].nodes_head.iter().copied().collect();
    for output in outputs {
        if crate::con::con_is_internal(t, output) {
            continue;
        }
        let content = crate::output::output_get_content(t, output)
            .expect("get_workspaces: no content container");
        let workspaces: Vec<crate::ConId> = t.cons[content].nodes_head.iter().copied().collect();
        for ws in workspaces {
            debug_assert!(t.cons[ws].r#type == crate::data::ConType::CT_WORKSPACE);
            gen.map_open();

            gen.string("id");
            gen.integer(ws.data().as_ffi() as i64);

            gen.string("num");
            gen.integer(t.cons[ws].num as i64);

            gen.string("name");
            gen.string(t.cons[ws].name.as_deref().unwrap_or(""));

            gen.string("visible");
            gen.boolean(crate::workspace::workspace_is_visible(t, ws));

            gen.string("focused");
            gen.boolean(Some(ws) == focused_ws);

            gen.string("rect");
            gen.map_open();
            gen.string("x");
            gen.integer(t.cons[ws].rect.x as i32 as i64);
            gen.string("y");
            gen.integer(t.cons[ws].rect.y as i32 as i64);
            gen.string("width");
            gen.integer(t.cons[ws].rect.width as i64);
            gen.string("height");
            gen.integer(t.cons[ws].rect.height as i64);
            gen.map_close();

            gen.string("output");
            gen.string(t.cons[output].name.as_deref().unwrap_or(""));

            gen.string("urgent");
            gen.boolean(t.cons[ws].urgent);

            gen.map_close();
        }
    }

    gen.array_close();

    let payload = gen.take_buf();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_WORKSPACES);
    }
}

/// C: src/ipc.c:1000 IPC_HANDLER(get_outputs)
fn handle_get_outputs(server: &mut IpcServer, client_index: usize, t: &Tree) {
    let mut gen = JsonGen::new();
    gen.array_open();

    for (idx, output) in t.outputs.iter().enumerate() {
        gen.map_open();

        gen.string("name");
        gen.string(&crate::output::output_primary_name(t, idx));

        gen.string("active");
        gen.boolean(output.active);

        gen.string("primary");
        gen.boolean(output.primary);

        gen.string("rect");
        gen.map_open();
        gen.string("x");
        gen.integer(output.rect.x as i32 as i64);
        gen.string("y");
        gen.integer(output.rect.y as i32 as i64);
        gen.string("width");
        gen.integer(output.rect.width as i64);
        gen.string("height");
        gen.integer(output.rect.height as i64);
        gen.map_close();

        gen.string("current_workspace");
        let ws = match output.con {
            Some(con) => crate::con::con_get_fullscreen_con(t, con, crate::data::FullscreenMode::CF_OUTPUT),
            None => None,
        };
        match ws {
            Some(ws) => gen.string(t.cons[ws].name.as_deref().unwrap_or("")),
            None => gen.null(),
        }

        gen.map_close();
    }

    gen.array_close();

    let payload = gen.take_buf();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_OUTPUTS);
    }
}

/// C: src/ipc.c:1055 IPC_HANDLER(get_marks)
fn handle_get_marks(server: &mut IpcServer, client_index: usize, t: &Tree) {
    let mut gen = JsonGen::new();
    gen.array_open();

    for con in &t.all_cons {
        for mark in &t.cons[*con].marks_head {
            gen.string(&mark.name);
        }
    }

    gen.array_close();

    let payload = gen.take_buf();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_MARKS);
    }
}

/// C: version.c MAJOR_VERSION/MINOR_VERSION/PATCH_VERSION (i3 4.25).
pub const MAJOR_VERSION: i64 = 4;
pub const MINOR_VERSION: i64 = 25;
pub const PATCH_VERSION: i64 = 0;

/// C: config.h I3_VERSION — the libi3 port mirrors the meson vcs_tag
/// fallback "4.25-non-git" and honours $I3RS_VERSION for byte-exact
/// baseline comparisons. (libi3 is not a dependency of i3-core, so the
/// function is mirrored.)
fn i3_version() -> String {
    std::env::var("I3RS_VERSION").unwrap_or_else(|_| "4.25-non-git".to_string())
}

/// C: src/ipc.c:1081 IPC_HANDLER(get_version)
fn handle_get_version(server: &mut IpcServer, client_index: usize) {
    let mut gen = JsonGen::new();
    gen.map_open();

    gen.string("major");
    gen.integer(MAJOR_VERSION);

    gen.string("minor");
    gen.integer(MINOR_VERSION);

    gen.string("patch");
    gen.integer(PATCH_VERSION);

    gen.string("human_readable");
    gen.string(&i3_version());

    gen.string("loaded_config_file_name");
    gen.string(&server.state.current_configpath);

    gen.string("included_config_file_names");
    gen.array_open();
    for (i, file) in server.state.included_files.iter().enumerate() {
        if i == 0 {
            /* Skip the first file, which is current_configpath. */
            continue;
        }
        gen.string(&file.path);
    }
    gen.array_close();
    gen.map_close();

    let payload = gen.take_buf();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_VERSION);
    }
}

/// C: src/ipc.c:1126 IPC_HANDLER(get_bar_config)
fn handle_get_bar_config(
    server: &mut IpcServer,
    client_index: usize,
    t: &Tree,
    message: &[u8],
) {
    let mut gen = JsonGen::new();

    /* If no ID was passed, we return a JSON array with all IDs */
    if message.is_empty() {
        gen.array_open();
        for current in &t.barconfigs {
            gen.string(current.id.as_deref().unwrap_or(""));
        }
        gen.array_close();

        let payload = gen.take_buf();
        if let Some(client) = server.clients.get_mut(client_index) {
            client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_BAR_CONFIG);
        }
        return;
    }

    /* To get a properly terminated buffer, we copy message_size bytes out
     * of the buffer */
    let bar_id = String::from_utf8_lossy(message).into_owned();
    // C: LOG("IPC: looking for config for bar ID \"%s\"\n", bar_id);

    let config = t
        .barconfigs
        .iter()
        .find(|current| current.id.as_deref() == Some(bar_id.as_str()));

    match config {
        None => {
            /* If we did not find a config for the given ID, the reply will
             * contain a null 'id' field. */
            gen.map_open();

            gen.string("id");
            gen.null();

            gen.map_close();
        }
        Some(config) => {
            // dump_bar_config cannot borrow server while we hold the
            // reference into server.state — clone the barconfig (the C code
            // operates on the pointer; the clone is invisible in the
            // output).
            let config = config.clone();
            dump_bar_config(t, &mut gen, &config);
        }
    }

    let payload = gen.take_buf();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_BAR_CONFIG);
    }
}

/// C: src/ipc.c:1188 IPC_HANDLER(get_binding_modes)
fn handle_get_binding_modes(server: &mut IpcServer, client_index: usize) {
    let mut gen = JsonGen::new();

    gen.array_open();
    for mode in &server.state.modes {
        gen.string(mode);
    }
    gen.array_close();

    let payload = gen.take_buf();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_BINDING_MODES);
    }
}

/// C: src/ipc.c:1283 IPC_HANDLER(get_config)
fn handle_get_config(server: &mut IpcServer, client_index: usize) {
    let mut gen = JsonGen::new();

    gen.map_open();

    gen.string("config");
    let first = server.state.included_files.front().cloned().unwrap_or_default();
    gen.string(&first.raw_contents);

    gen.string("included_configs");
    gen.array_open();
    for file in &server.state.included_files {
        gen.map_open();
        gen.string("path");
        gen.string(&file.path);
        gen.string("raw_contents");
        gen.string(&file.raw_contents);
        gen.string("variable_replaced_contents");
        gen.string(&file.variable_replaced_contents);
        gen.map_close();
    }
    gen.array_close();

    gen.map_close();

    let payload = gen.take_buf();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_CONFIG);
    }
}

/// C: src/ipc.c:1320 IPC_HANDLER(send_tick)
fn handle_send_tick(server: &mut IpcServer, client_index: usize, message: &[u8]) {
    let mut gen = JsonGen::new();

    gen.map_open();

    gen.string("first");
    gen.boolean(false);

    gen.string("payload");
    // C: yajl_gen_string(gen, (unsigned char *)message, message_size) — the
    // raw bytes go through the yajl string encoder.
    gen.string_bytes(message);

    gen.map_close();

    let payload = gen.take_buf();
    self_ipc_send_event(server, "tick", I3_IPC_EVENT_TICK, &payload);

    let reply = r#"{"success":true}"#;
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(reply.as_bytes(), I3_IPC_REPLY_TYPE_TICK);
    }
    // C: DLOG("Sent tick event\n");
}

/// ipc_send_event needs &mut self on IpcServer; the free handler functions
/// take &mut IpcServer, so this is just a wrapper used above.
fn self_ipc_send_event(server: &mut IpcServer, event: &str, message_type: u32, payload: &str) {
    server.ipc_send_event(event, message_type, payload);
}

/// C: src/ipc.c:1369 IPC_HANDLER(sync) — parses {"window": …, "rnd": …} and
/// replies. sync_respond (src/sync.c) writes the X11 reply; the X layer is
/// not available here.
/// TODO(X11 port): sync_respond.
fn handle_sync(server: &mut IpcServer, client_index: usize, message: &[u8]) {
    struct sync_state {
        last_key: String,
        rnd: u32,
        window: u32,
    }

    // C: yajl_parse(p, message, message_size) — a malformed document must
    // fail (yajl_status != yajl_status_ok), so the grammar is checked first.
    if !json_valid(message) {
        // C: ELOG("YAJL parse error: %s\n", err);
        let reply = r#"{"success":false}"#;
        if let Some(client) = server.clients.get_mut(client_index) {
            client.send_message(reply.as_bytes(), I3_IPC_REPLY_TYPE_SYNC);
        }
        return;
    }

    // The yajl callbacks (map_key + integer) become a tiny scanner over the
    // (now known to be valid) message.
    let mut state = sync_state {
        last_key: String::new(),
        rnd: 0,
        window: 0,
    };
    let bytes = message;
    let mut walk = 0usize;
    while walk < bytes.len() {
        // C: _sync_json_key — map keys and strings.
        if bytes[walk] == b'"' {
            let start = walk + 1;
            walk += 1;
            while walk < bytes.len() && bytes[walk] != b'"' {
                if bytes[walk] == b'\\' {
                    walk += 1;
                }
                walk += 1;
            }
            let s = String::from_utf8_lossy(&bytes[start..walk]).into_owned();
            // C: _sync_json_int fires on the following integer; remember the
            // last key (a string value never sets the key in C).
            if is_map_key(bytes, walk) {
                state.last_key = s;
            }
            walk += 1;
            continue;
        }
        // C: yajl_integer — an optional minus sign, then the digits.
        if bytes[walk].is_ascii_digit() || bytes[walk] == b'-' {
            let start = walk;
            walk += 1;
            while walk < bytes.len() && bytes[walk].is_ascii_digit() {
                walk += 1;
            }
            let value: i64 = std::str::from_utf8(&bytes[start..walk])
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            // C: strcasecmp(state.last_key, "rnd") == 0 — the values are
            // truncated to the C types (uint32_t / xcb_window_t).
            if state.last_key.eq_ignore_ascii_case("rnd") {
                state.rnd = value as u32;
            } else if state.last_key.eq_ignore_ascii_case("window") {
                state.window = value as u32;
            }
            continue;
        }
        walk += 1;
    }

    // C: sync_respond(state.window, state.rnd); TODO(X11 port)
    let reply = r#"{"success":true}"#;
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(reply.as_bytes(), I3_IPC_REPLY_TYPE_SYNC);
    }
}

/// C: yajl_parse semantics — a recursive descent check of the JSON grammar
/// (RFC 8259). yajl accepts any JSON value, requires the whole buffer to be
/// one document and rejects control characters inside strings and trailing
/// garbage, which is exactly what the sync handler reacts to.
fn json_valid(bytes: &[u8]) -> bool {
    struct P<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl<'a> P<'a> {
        fn ws(&mut self) {
            while self.i < self.b.len()
                && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r')
            {
                self.i += 1;
            }
        }
        fn lit(&mut self, word: &[u8]) -> bool {
            if self.b.len() >= self.i + word.len() && &self.b[self.i..self.i + word.len()] == word {
                self.i += word.len();
                true
            } else {
                false
            }
        }
        fn string(&mut self) -> bool {
            if self.b.get(self.i) != Some(&b'"') {
                return false;
            }
            self.i += 1;
            while let Some(&c) = self.b.get(self.i) {
                match c {
                    b'"' => {
                        self.i += 1;
                        return true;
                    }
                    b'\\' => {
                        self.i += 1;
                        match self.b.get(self.i) {
                            Some(b'"') | Some(b'\\') | Some(b'/') | Some(b'b') | Some(b'f')
                            | Some(b'n') | Some(b'r') | Some(b't') => self.i += 1,
                            Some(b'u') => {
                                self.i += 1;
                                for _ in 0..4 {
                                    match self.b.get(self.i) {
                                        Some(c) if c.is_ascii_hexdigit() => self.i += 1,
                                        _ => return false,
                                    }
                                }
                            }
                            _ => return false,
                        }
                    }
                    c if c < 0x20 => return false,
                    _ => {
                        self.i += 1;
                    }
                }
            }
            false
        }
        fn number(&mut self) -> bool {
            if self.b.get(self.i) == Some(&b'-') {
                self.i += 1;
            }
            match self.b.get(self.i) {
                Some(b'0') => self.i += 1,
                Some(c) if c.is_ascii_digit() => {
                    while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                        self.i += 1;
                    }
                }
                _ => return false,
            }
            if self.b.get(self.i) == Some(&b'.') {
                self.i += 1;
                if !matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                    return false;
                }
                while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            if matches!(self.b.get(self.i), Some(b'e') | Some(b'E')) {
                self.i += 1;
                if matches!(self.b.get(self.i), Some(b'+') | Some(b'-')) {
                    self.i += 1;
                }
                if !matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                    return false;
                }
                while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            true
        }
        fn value(&mut self) -> bool {
            self.ws();
            match self.b.get(self.i) {
                Some(b'{') => {
                    self.i += 1;
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return true;
                    }
                    loop {
                        self.ws();
                        if !self.string() {
                            return false;
                        }
                        self.ws();
                        if self.b.get(self.i) != Some(&b':') {
                            return false;
                        }
                        self.i += 1;
                        if !self.value() {
                            return false;
                        }
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                return true;
                            }
                            _ => return false,
                        }
                    }
                }
                Some(b'[') => {
                    self.i += 1;
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return true;
                    }
                    loop {
                        if !self.value() {
                            return false;
                        }
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                return true;
                            }
                            _ => return false,
                        }
                    }
                }
                Some(b'"') => self.string(),
                Some(b't') => self.lit(b"true"),
                Some(b'f') => self.lit(b"false"),
                Some(b'n') => self.lit(b"null"),
                Some(c) if c.is_ascii_digit() || *c == b'-' => self.number(),
                _ => false,
            }
        }
    }

    let mut p = P { b: bytes, i: 0 };
    if !p.value() {
        return false;
    }
    p.ws();
    p.i == bytes.len()
}

/// True when the string that ends at 'closing_quote_pos' is a map key (a ':'
/// follows the closing quote after optional whitespace). C: yajl only invokes
/// _sync_json_key for map keys, never for string values.
fn is_map_key(bytes: &[u8], closing_quote_pos: usize) -> bool {
    let mut i = closing_quote_pos + 1;
    while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    i < bytes.len() && bytes[i] == b':'
}

/// C: src/ipc.c:1398 IPC_HANDLER(get_binding_state)
fn handle_get_binding_state(server: &mut IpcServer, client_index: usize) {
    let mut gen = JsonGen::new();

    gen.map_open();

    gen.string("name");
    gen.string(&server.state.current_binding_mode);

    gen.map_close();

    let payload = gen.take_buf();
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_GET_BINDING_STATE);
    }
}

/// C: src/ipc.c:1210 add_subscription — the yajl string callback of the
/// SUBSCRIBE parser. Strings (and, like in yajl, map keys when no
/// map_key callback exists) are collected as subscriptions.
fn add_subscription(server: &mut IpcServer, client_index: usize, s: &str) {
    if let Some(client) = server.clients.get_mut(client_index) {
        client.events.push(s.to_string());
    }
}

/// Collects the JSON strings of the payload like yajl does for the
/// SUBSCRIBE handler (C: yajl callbacks with only .yajl_string set; map
/// keys are passed to yajl_string when no yajl_map_key callback exists).
fn parse_subscriptions(
    server: &mut IpcServer,
    client_index: usize,
    bytes: &[u8],
) -> bool {
    let mut ok = true;
    let mut walk = 0usize;
    let text = String::from_utf8_lossy(bytes).into_owned();
    while walk < bytes.len() {
        if bytes[walk] == b'"' {
            let start = walk + 1;
            walk += 1;
            while walk < bytes.len() && bytes[walk] != b'"' {
                if bytes[walk] == b'\\' && walk + 1 < bytes.len() {
                    walk += 1;
                }
                walk += 1;
            }
            if walk >= bytes.len() {
                ok = false;
                break;
            }
            let s = &text[start..walk];
            let unescaped = unescape_json_string(s);
            add_subscription(server, client_index, &unescaped);
            walk += 1;
            continue;
        }
        walk += 1;
    }
    ok
}

/// Unescapes the JSON string escapes that matter for subscription names
/// (yajl yields the decoded string).
fn unescape_json_string(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{0008}'),
                Some('f') => out.push('\u{000C}'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// C: src/ipc.c:1238 IPC_HANDLER(subscribe)
fn handle_subscribe(server: &mut IpcServer, client_index: usize, message: &[u8]) {
    let parse_ok = parse_subscriptions(server, client_index, message);

    if !parse_ok {
        // C: ELOG("YAJL parse error: %s\n", err);
        let reply = r#"{"success":false}"#;
        if let Some(client) = server.clients.get_mut(client_index) {
            client.send_message(reply.as_bytes(), I3_IPC_REPLY_TYPE_SUBSCRIBE);
        }
        return;
    }
    let reply = r#"{"success":true}"#;
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(reply.as_bytes(), I3_IPC_REPLY_TYPE_SUBSCRIBE);
    }

    let client = &server.clients[client_index];
    if client.first_tick_sent {
        return;
    }

    let is_tick = client
        .events
        .iter()
        .any(|e| e == "tick");
    if !is_tick {
        return;
    }

    server.clients[client_index].first_tick_sent = true;
    let payload = r#"{"first":true,"payload":""}"#;
    if let Some(client) = server.clients.get_mut(client_index) {
        client.send_message(payload.as_bytes(), I3_IPC_EVENT_TICK);
    }
}

/// C: src/ipc.c:1444 ipc_receive_message — reads one message from the
/// client and dispatches it. Returns false when the client was
/// disconnected (C: free_ipc_client).
pub fn handle_readable(
    server: &mut IpcServer,
    client_index: usize,
    t: &mut Tree,
    b: &mut dyn ConBackend,
) -> bool {
    let mut message_type = 0u32;
    let mut message: Vec<u8> = Vec::new();

    let mut stream = {
        let client = &mut server.clients[client_index];
        client.stream.try_clone().expect("handle_readable: cannot clone stream")
    };
    let ret = ipc_recv_message(&mut stream, Some(&mut message_type), &mut message);
    drop(stream);
    /* EOF or other error */
    if ret < 0 {
        /* Was this a spurious read? See ev(3) */
        if ret == -1 {
            return true;
        }

        /* If not, there was some kind of error. We don't bother and close
         * the connection. Delete the client from the list of clients. */
        server.free_ipc_client(client_index, None);
        return false;
    }

    if message_type >= 13 {
        // C: DLOG("Unhandled message type: %d\n", message_type);
    } else {
        dispatch_message(server, client_index, t, b, message_type, &message);
    }

    true
}

/// C: src/ipc.c:1418 handlers[13] — the index is the message type.
fn dispatch_message(
    server: &mut IpcServer,
    client_index: usize,
    t: &mut Tree,
    b: &mut dyn ConBackend,
    message_type: u32,
    message: &[u8],
) {
    match message_type {
        0 => handle_run_command(server, client_index, t, b, message),
        1 => handle_get_workspaces(server, client_index, t),
        2 => handle_subscribe(server, client_index, message),
        3 => handle_get_outputs(server, client_index, t),
        4 => {
            // C: IPC_HANDLER(tree) (ipc.c:911) — the LC_NUMERIC dance is
            // unnecessary in Rust (formatting is locale independent).
            let payload = tree_payload(t);
            if let Some(client) = server.clients.get_mut(client_index) {
                client.send_message(payload.as_bytes(), I3_IPC_REPLY_TYPE_TREE);
            }
        }
        5 => handle_get_marks(server, client_index, t),
        6 => handle_get_bar_config(server, client_index, t, message),
        7 => handle_get_version(server, client_index),
        8 => handle_get_binding_modes(server, client_index),
        9 => handle_get_config(server, client_index),
        10 => handle_send_tick(server, client_index, message),
        11 => handle_sync(server, client_index, message),
        12 => handle_get_binding_state(server, client_index),
        _ => {}
    }
}

/// C: src/ipc.c:1477 ipc_client_timeout — called by the event loop when a
/// client did not accept its pending write within kill_timeout.
/// TODO(event loop): the SO_PEERCRED based cmdline logging
/// (C: ipc.c:1483-1512) needs libc; the plain log line is emitted today.
pub fn ipc_client_timeout(server: &mut IpcServer, client_index: usize) {
    // C: ELOG("client %p on fd %d timed out, killing\n", client, client->fd)
    // (the SO_PEERCRED cmdline lookup above it needs /proc and libc; it is
    // still the TODO noted on the module, the plain line is what C logs when
    // that lookup fails as well).
    let fd = server
        .clients
        .get(client_index)
        .map(|c| c.stream.as_raw_fd())
        .unwrap_or(-1);
    crate::log::elog(&format!("client fd {} timed out, killing\n", fd));
    server.free_ipc_client(client_index, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::commands::CommandRunner;
    use crate::data::ConType;
    use crate::dump::tree_payload as _tree_payload;
    use crate::tree::tree_init;
    use crate::workspace::create_workspace_on_output;
    use std::io::Write as _;
    use std::os::unix::io::AsRawFd as _;
    use std::time::Duration;

    fn test_tree() -> Tree {
        let mut t = Tree::new();
        let mut b = NullBackend;
        tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));

        let croot = t.croot;
        let output = crate::con::con_new(&mut t, &mut b, croot, None);
        t.cons[output].r#type = ConType::CT_OUTPUT;
        t.cons[output].name = Some("fake-0".to_string());
        t.cons[output].rect = Rect::new(0, 0, 1280, 800);
        t.focus_insert_tail(t.croot.unwrap(), output);

        let output_id = t.outputs.len();
        let mut output_data = crate::data::Output::default();
        output_data.active = true;
        output_data.primary = true;
        output_data.rect = Rect::new(0, 0, 1280, 800);
        output_data.con = Some(output);
        output_data.names_head.push_back(crate::data::output_name {
            name: "fake-0".to_string(),
        });
        t.outputs.push_back(output_data);

        let content = crate::con::con_new(&mut t, &mut b, Some(output), None);
        t.cons[content].r#type = ConType::CT_CON;
        t.cons[content].layout = crate::data::Layout::L_SPLITH;
        t.cons[content].rect = Rect::new(0, 0, 1280, 800);
        t.nodes_insert_tail(output, content);

        let ws = create_workspace_on_output(&mut t, &mut b, output_id, content);
        t.focused = Some(t.cons[ws].nodes_head.front().copied().unwrap_or(ws));
        t
    }

    /// Writes an i3-ipc frame on the client side.
    fn write_frame(stream: &mut UnixStream, message_type: u32, payload: &[u8]) {
        let mut msg = Vec::new();
        msg.extend_from_slice(b"i3-ipc");
        msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        msg.extend_from_slice(&message_type.to_ne_bytes());
        msg.extend_from_slice(payload);
        stream.write_all(&msg).unwrap();
    }

    /// Reads one i3-ipc frame reply (blocking client side).
    fn read_frame(stream: &mut UnixStream) -> (u32, Vec<u8>) {
        let mut header = [0u8; 14];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(&header[0..6], b"i3-ipc");
        let size = u32::from_ne_bytes(header[6..10].try_into().unwrap());
        let type_ = u32::from_ne_bytes(header[10..14].try_into().unwrap());
        let mut payload = vec![0u8; size as usize];
        if size > 0 {
            stream.read_exact(&mut payload).unwrap();
        }
        (type_, payload)
    }

    /// C: frame parsing + GET_VERSION roundtrip over a real socket
    /// (C: ipc.c:1081 IPC_HANDLER(get_version) through
    /// ipc_receive_message/ipc_new_client_on_fd).
    #[test]
    fn get_version_roundtrip() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);

        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();
        server.state.current_configpath = "/home/user/.config/i3/config".to_string();

        let (client_stream, _accepted) = {
            let mut c = UnixStream::connect(&sockpath).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            write_frame(&mut c, I3_IPC_MESSAGE_TYPE_GET_VERSION, b"");
            let s = server.accept_client().unwrap();
            (c, s)
        };
        let mut client_stream = client_stream;

        // Server side: read the message and dispatch it.
        let mut t = test_tree();
        let mut b = NullBackend;
        assert!(handle_readable(&mut server, 0, &mut t, &mut b));

        let (reply_type, payload) = read_frame(&mut client_stream);
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_VERSION);
        let text = String::from_utf8(payload).unwrap();
        assert!(text.starts_with(r#"{"major":4,"minor":25,"patch":0,"human_readable":"#));
        assert!(text.contains(r#""loaded_config_file_name":"/home/user/.config/i3/config""#));

        let _ = std::fs::remove_file(&sockpath);
    }

    /// C: an invalid magic frame disconnects the client
    /// (C: ipc_recv_message returns -3 → free_ipc_client).
    #[test]
    fn invalid_magic_disconnects() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-test2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);

        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();
        {
            let mut c = UnixStream::connect(&sockpath).unwrap();
            // 14 bytes of garbage
            c.write_all(b"XXXXXXXXXXXXXX").unwrap();
            let _ = server.accept_client().unwrap();
        }

        let mut t = test_tree();
        let mut b = NullBackend;
        assert!(!handle_readable(&mut server, 0, &mut t, &mut b));
        assert!(server.clients.is_empty());

        let _ = std::fs::remove_file(&sockpath);
    }

    /// C: subscribe + first tick event (ipc.c:1238 IPC_HANDLER(subscribe)
    /// and ipc.c:1320 IPC_HANDLER(send_tick)).
    #[test]
    fn subscribe_and_tick() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-test3-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);

        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();

        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_SUBSCRIBE, br#"["tick","window"]"#);
        server.accept_client().unwrap();

        let mut t = test_tree();
        let mut b = NullBackend;
        handle_readable(&mut server, 0, &mut t, &mut b);

        // Reply {"success":true} followed by the first tick event.
        let (t1, p1) = read_frame(&mut client);
        assert_eq!(t1, I3_IPC_REPLY_TYPE_SUBSCRIBE);
        assert_eq!(p1, br#"{"success":true}"#);
        let (t2, p2) = read_frame(&mut client);
        assert_eq!(t2, I3_IPC_EVENT_TICK);
        assert_eq!(p2, br#"{"first":true,"payload":""}"#);

        // C: send_tick broadcasts {"first":false,"payload":"…"} and replies.
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_SEND_TICK, b"hello");
        handle_readable(&mut server, 0, &mut t, &mut b);
        let (t3, p3) = read_frame(&mut client);
        assert_eq!(t3, I3_IPC_EVENT_TICK);
        assert_eq!(p3, br#"{"first":false,"payload":"hello"}"#);
        let (t4, p4) = read_frame(&mut client);
        assert_eq!(t4, I3_IPC_REPLY_TYPE_TICK);
        assert_eq!(p4, br#"{"success":true}"#);

        // The event goes only to tick subscribers (this client subscribed).
        assert_eq!(server.clients[0].events, vec!["tick", "window"]);

        let _ = std::fs::remove_file(&sockpath);
    }

    /// C: GET_MARKS / GET_WORKSPACES payloads (ipc.c:1055/932).
    #[test]
    fn get_marks_and_workspaces() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-test4-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);

        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();

        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_GET_MARKS, b"");
        server.accept_client().unwrap();

        let mut t = test_tree();
        // Give the workspace a mark so the reply is not empty.
        let ws = crate::con::con_get_workspace(&t, t.focused.unwrap()).unwrap();
        t.cons[ws].marks_head.push_back(crate::data::mark_t {
            name: "urgent".to_string(),
        });
        let mut b = NullBackend;
        handle_readable(&mut server, 0, &mut t, &mut b);

        let (reply_type, payload) = read_frame(&mut client);
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_MARKS);
        assert_eq!(String::from_utf8(payload).unwrap(), r#"["urgent"]"#);

        let _ = std::fs::remove_file(&sockpath);
    }

    /// C: run_command executes through CommandRunner and replies
    /// (ipc.c:208 IPC_HANDLER(run_command)).
    #[test]
    fn run_command_roundtrip() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-test5-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);

        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();
        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_RUN_COMMAND, b"nop");
        server.accept_client().unwrap();

        let mut t = test_tree();
        let mut b = NullBackend;
        handle_readable(&mut server, 0, &mut t, &mut b);

        let (reply_type, payload) = read_frame(&mut client);
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_COMMAND);
        assert_eq!(String::from_utf8(payload).unwrap(), r#"[{"success":true}]"#);

        let _ = std::fs::remove_file(&sockpath);
    }

    /// Binds a server, connects one client and answers exactly one message.
    /// Returns the (reply type, payload) of the reply frame.
    fn roundtrip(tag: &str, message_type: u32, payload: &[u8]) -> (u32, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-{}-{}", tag, std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);

        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();
        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, message_type, payload);
        server.accept_client().unwrap();

        let mut t = test_tree();
        let mut b = NullBackend;
        handle_readable(&mut server, 0, &mut t, &mut b);

        let frame = read_frame(&mut client);
        let _ = std::fs::remove_file(&sockpath);
        frame
    }

    /// C: src/ipc.c:1000 IPC_HANDLER(get_outputs) — every output of the i3
    /// tree (including the internal __i3 output), in the C key order.
    #[test]
    fn get_outputs_roundtrip() {
        let (reply_type, payload) = roundtrip("outputs", I3_IPC_MESSAGE_TYPE_GET_OUTPUTS, b"");
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_OUTPUTS);
        let text = String::from_utf8(payload).unwrap();
        assert!(text.starts_with(r#"[{"name":"#), "{}", text);
        // The fake output of the fixture: active, primary and its rect.
        assert!(
            text.contains(r#""name":"fake-0","active":true,"primary":true,"rect":{"x":0,"y":0,"width":1280,"height":800}"#),
            "{}",
            text
        );
        // C: src/ipc.c:1044 — current_workspace is the CF_OUTPUT fullscreen
        // container of the output; workspaces are created with
        // fullscreen_mode = CF_OUTPUT (src/workspace.c
        // create_workspace_on_output), so it is the workspace "1" here.
        assert!(text.contains(r#""current_workspace":"1"}"#), "{}", text);
    }

    /// C: src/ipc.c:1126 IPC_HANDLER(get_bar_config) — without an ID the reply
    /// is the list of IDs, with an ID the dumped Barconfig (outputs and tray
    /// outputs canonicalized via get_output_by_name), unknown IDs yield
    /// {"id":null}.
    #[test]
    fn get_bar_config_roundtrip() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-bar-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);
        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();

        let mut config = Barconfig {
            id: Some("bar-0".to_string()),
            // Uppercase on purpose: canonicalize_output_name matches with
            // strcasecmp and replies with the primary name of the output.
            num_outputs: 1,
            outputs: vec!["FAKE-0".to_string()],
            mode: crate::config::bar_mode_t::M_DOCK,
            hidden_state: crate::config::bar_hidden_state_t::S_HIDE,
            position: crate::config::bar_position_t::P_TOP,
            ..Default::default()
        };
        config.tray_outputs.push_back(tray_output_t {
            output: "primary".to_string(),
        });
        config.bar_bindings.push_back(Barbinding {
            input_code: 24,
            command: "workspace 1".to_string(),
            release: false,
        });
        // C: the barconfigs list is a global that the config layer fills; the
        // tree holds it next to the other C globals (see Tree::barconfigs).
        let mut t = test_tree();
        let mut b = NullBackend;
        t.barconfigs.push_back(config);

        // (1) No payload: the list of bar IDs.
        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG, b"");
        server.accept_client().unwrap();
        handle_readable(&mut server, 0, &mut t, &mut b);
        let (reply_type, payload) = read_frame(&mut client);
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_BAR_CONFIG);
        assert_eq!(String::from_utf8(payload).unwrap(), r#"["bar-0"]"#);
        server.free_ipc_client(0, None);

        // (2) A known ID: the dumped config with the canonicalized output.
        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG, b"bar-0");
        server.accept_client().unwrap();
        handle_readable(&mut server, 0, &mut t, &mut b);
        let (reply_type, payload) = read_frame(&mut client);
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_BAR_CONFIG);
        let text = String::from_utf8(payload).unwrap();
        assert!(text.starts_with(r#"{"id":"bar-0""#), "{}", text);
        assert!(text.contains(r#""outputs":["fake-0"]"#), "{}", text);
        assert!(text.contains(r#""tray_outputs":["primary"]"#), "{}", text);
        assert!(text.contains(r#""mode":"dock""#), "{}", text);
        assert!(text.contains(r#""hidden_state":"hide""#), "{}", text);
        assert!(text.contains(r#""position":"top""#), "{}", text);
        assert!(
            text.contains(
                r#""bindings":[{"input_code":24,"command":"workspace 1","release":false}]"#
            ),
            "{}",
            text
        );
        server.free_ipc_client(0, None);

        // (3) An unknown ID: {"id":null}.
        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG, b"nope");
        server.accept_client().unwrap();
        handle_readable(&mut server, 0, &mut t, &mut b);
        let (_, payload) = read_frame(&mut client);
        assert_eq!(String::from_utf8(payload).unwrap(), r#"{"id":null}"#);

        let _ = std::fs::remove_file(&sockpath);
    }

    /// C: src/ipc.c:1188 IPC_HANDLER(get_binding_modes) and
    /// src/ipc.c:1283 IPC_HANDLER(get_config).
    #[test]
    fn get_binding_modes_and_config() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-modes-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);
        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();
        server.state.modes.push_back("default".to_string());
        server.state.modes.push_back("resize".to_string());
        server.state.included_files.push_back(IncludedFile {
            path: "/etc/i3/config".to_string(),
            raw_contents: "font pango:x\n".to_string(),
            variable_replaced_contents: "font pango:x\n".to_string(),
        });
        server.state.included_files.push_back(IncludedFile {
            path: "/etc/i3/bar.conf".to_string(),
            raw_contents: "bar {\n}\n".to_string(),
            variable_replaced_contents: "bar {\n}\n".to_string(),
        });

        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_GET_BINDING_MODES, b"");
        server.accept_client().unwrap();
        let mut t = test_tree();
        let mut b = NullBackend;
        handle_readable(&mut server, 0, &mut t, &mut b);
        let (reply_type, payload) = read_frame(&mut client);
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_BINDING_MODES);
        assert_eq!(
            String::from_utf8(payload).unwrap(),
            r#"["default","resize"]"#
        );
        server.free_ipc_client(0, None);

        // GET_CONFIG: the raw contents of the first file plus every included
        // file with path / raw_contents / variable_replaced_contents.
        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_GET_CONFIG, b"");
        server.accept_client().unwrap();
        handle_readable(&mut server, 0, &mut t, &mut b);
        let (reply_type, payload) = read_frame(&mut client);
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_CONFIG);
        let text = String::from_utf8(payload).unwrap();
        assert!(text.starts_with(r#"{"config":"font pango:x\n","#), "{}", text);
        assert!(text.contains(r#"{"path":"/etc/i3/bar.conf""#), "{}", text);
        assert!(
            text.contains(r#""variable_replaced_contents":"bar {\n}\n""#),
            "{}",
            text
        );

        let _ = std::fs::remove_file(&sockpath);
    }

    /// C: src/ipc.c:1398 IPC_HANDLER(get_binding_state)
    #[test]
    fn get_binding_state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-bstate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);
        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();
        server.state.current_binding_mode = "resize".to_string();

        let mut client = UnixStream::connect(&sockpath).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut client, I3_IPC_MESSAGE_TYPE_GET_BINDING_STATE, b"");
        server.accept_client().unwrap();
        let mut t = test_tree();
        let mut b = NullBackend;
        handle_readable(&mut server, 0, &mut t, &mut b);
        let (reply_type, payload) = read_frame(&mut client);
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_GET_BINDING_STATE);
        assert_eq!(String::from_utf8(payload).unwrap(), r#"{"name":"resize"}"#);

        let _ = std::fs::remove_file(&sockpath);
    }

    /// C: src/ipc.c:1369 IPC_HANDLER(sync) — {"window":…,"rnd":…} is answered
    /// with success (sync_respond is X11-only), malformed JSON with
    /// {"success":false}.
    #[test]
    fn sync_roundtrip() {
        let (reply_type, payload) = roundtrip(
            "sync",
            I3_IPC_MESSAGE_TYPE_SYNC,
            br#"{"window":123,"rnd":42}"#,
        );
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_SYNC);
        assert_eq!(String::from_utf8(payload).unwrap(), r#"{"success":true}"#);

        // An unterminated string is a yajl parse error in C.
        let (reply_type, payload) = roundtrip(
            "sync-bad",
            I3_IPC_MESSAGE_TYPE_SYNC,
            br#"{"window":"oops}"#,
        );
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_SYNC);
        assert_eq!(String::from_utf8(payload).unwrap(), r#"{"success":false}"#);
    }

    /// C: yajl_parse rejects anything that is not exactly one JSON document;
    /// the sync handler answers {"success":false} in that case.
    #[test]
    fn sync_json_validity() {
        // Valid documents (yajl accepts any JSON value).
        assert!(json_valid(br#"{"window":123,"rnd":42}"#));
        assert!(json_valid(br#"{"rnd": -7}"#));
        assert!(json_valid(b"{}"));
        assert!(json_valid(b" \r\n{ \"window\" : 1 } \t"));
        assert!(json_valid(br#"["window", 1]"#));
        assert!(json_valid(b"42"));
        assert!(json_valid(br#""a\u00e9b""#));
        // Malformed: trailing garbage, missing value, bad number, raw control
        // character in a string, unterminated string/object.
        assert!(!json_valid(b"not json"));
        assert!(!json_valid(br#"{"window":1} x"#));
        assert!(!json_valid(br#"{"window":}"#));
        assert!(!json_valid(b"{\"window\":1,}"));
        assert!(!json_valid(b"{\"window\":01}"));
        assert!(!json_valid(b"{\"window\":1.}"));
        assert!(!json_valid(b"{\"win\now\":1}"));
        assert!(!json_valid(b"{\"window\":\"oops}"));
        assert!(!json_valid(b"{"));
        assert!(!json_valid(b""));
        // A non-JSON body is answered with success:false.
        let (reply_type, payload) =
            roundtrip("sync-garbage", I3_IPC_MESSAGE_TYPE_SYNC, b"not json");
        assert_eq!(reply_type, I3_IPC_REPLY_TYPE_SYNC);
        assert_eq!(String::from_utf8(payload).unwrap(), r#"{"success":false}"#);
    }

    /// C: yajl calls _sync_json_key only for map keys (never for string
    /// values), so a string value does not become the current key.
    #[test]
    fn sync_map_key_detection() {
        // "window" followed by ':' is a key.
        let payload = br#"{"window":42}"#;
        assert!(is_map_key(payload, 8));
        // A string value ("42") followed by '}' is not a key.
        let payload = br#"{"window":"42"}"#;
        assert!(!is_map_key(payload, 12));
    }

    /// C: src/bindings.c:665 — the mode event payload is rebuilt verbatim:
    /// {"change":"<mode>", "pango_markup":<bool>} (with the space after the
    /// comma that sasprintf produces).
    #[test]
    fn mode_event_payload_matches_c() {
        let mut mode = crate::config::Mode {
            name: "resize".to_string(),
            pango_markup: false,
            bindings: VecDeque::new(),
        };
        assert_eq!(
            mode_event_payload(&mode),
            r#"{"change":"resize", "pango_markup":false}"#
        );

        mode.pango_markup = true;
        assert_eq!(
            mode_event_payload(&mode),
            r#"{"change":"resize", "pango_markup":true}"#
        );
    }

    /// C: src/ipc.c:1694 ipc_send_binding_event — "change" is the event type,
    /// "mode" is the mode the binding was run in (NULL becomes "default") and
    /// "binding" is the dump_binding object.
    #[test]
    fn binding_event_payload_matches_c() {
        let bind = Binding {
            command: "nop".to_string(),
            keycode: 24,
            symbol: Some("q".to_string()),
            ..Default::default()
        };
        assert_eq!(
            binding_event_payload("run", &bind, None),
            r#"{"change":"run","mode":"default","binding":{"input_code":24,"input_type":"keyboard","symbol":"q","command":"nop","mods":[],"event_state_mask":[]}}"#
        );
        assert!(binding_event_payload("run", &bind, Some("resize"))
            .contains(r#""mode":"resize""#));
    }

    /// C: src/bindings.c:669/902 — the two binding events go through the queue
    /// the event loop drains (IpcHooks is what bindings.c calls).
    #[test]
    fn ipc_hooks_queue_mode_and_binding_events() {
        use crate::bindings::BindingHooks as _;

        clear_pending_events();
        let bind = Binding {
            command: "nop".to_string(),
            ..Default::default()
        };
        let mut hooks = IpcHooks;
        hooks.ipc_send_mode_event(&crate::config::Mode {
            name: "resize".to_string(),
            pango_markup: false,
            bindings: VecDeque::new(),
        });
        hooks.ipc_send_binding_event(&bind, Some("resize"));

        let events = take_pending_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].subscription, "mode");
        assert_eq!(events[0].message_type, I3_IPC_EVENT_MODE);
        assert_eq!(
            events[0].payload,
            r#"{"change":"resize", "pango_markup":false}"#
        );
        assert_eq!(events[1].subscription, "binding");
        assert_eq!(events[1].message_type, I3_IPC_EVENT_BINDING);
        assert_eq!(
            events[1].payload,
            binding_event_payload("run", &bind, Some("resize"))
        );
    }

    /// C: src/ipc.c:1477 ipc_client_timeout — the timed out client is removed
    /// from the list and its socket closed.
    #[test]
    fn client_timeout_frees_client() {
        let dir = std::env::temp_dir().join(format!("i3rs-ipc-timeout-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);
        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();
        let mut client = UnixStream::connect(&sockpath).unwrap();
        server.accept_client().unwrap();
        assert_eq!(server.clients.len(), 1);
        ipc_client_timeout(&mut server, 0);
        assert!(server.clients.is_empty());
        // The peer sees EOF once the server side dropped the stream.
        let mut buf = [0u8; 1];
        assert_eq!(client.read(&mut buf).unwrap(), 0);
        let _ = std::fs::remove_file(&sockpath);
    }

    /// C: src/ipc.c:45-95 + :1477 — ipc_push_pending arms the kill timer when
    /// the pending buffer cannot be written out, and ipc_client_timeout frees
    /// the client once kill_timeout has passed (test 298-ipc-misbehaving-
    /// connection: a subscriber that stops reading must not be kept forever).
    #[test]
    fn pending_write_arms_and_expires_kill_timeout() {
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("i3rs-ipc-deadline-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sockpath = dir.join("sock");
        let _ = std::fs::remove_file(&sockpath);
        let mut server = IpcServer::bind(sockpath.to_str().unwrap()).unwrap();
        let _client = UnixStream::connect(&sockpath).unwrap();
        server.accept_client().unwrap();
        server.ipc_set_kill_timeout(0.2);
        assert!(server.clients[0].write_deadline.is_none());

        // The client never reads, so the socket buffer fills up and the write
        // of the message stays incomplete.
        let payload = vec![b'x'; 64 * 1024];
        for _ in 0..64 {
            server.clients[0].send_message(&payload, I3_IPC_EVENT_WINDOW);
        }
        let deadline = server.clients[0]
            .write_deadline
            .expect("an incomplete write arms the kill timer");
        assert!(deadline > Instant::now(), "the timer is kill_timeout in the future");
        assert!(!server.clients[0].buffer.is_empty());

        // Nothing has expired yet: the client stays connected.
        server.expire_write_timeouts(Instant::now());
        assert_eq!(server.clients.len(), 1);

        // C: ipc_client_timeout — the client is freed once the timer fires.
        server.expire_write_timeouts(deadline + Duration::from_millis(1));
        assert!(server.clients.is_empty());
        let _ = std::fs::remove_file(&sockpath);
    }
}
