#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! 1:1 port of i3bar/src/ipc.c (421 lines) — communicating with i3.
//!
//! C: i3bar/src/ipc.c
//!
//! The C code registers an ev_io watcher on the i3 socket and parses one
//! message per callback. The Rust port keeps got_data() as the per-message
//! entry point (main.rs polls the socket and calls it) and reproduces the
//! reply/event handler tables as matches.

use std::cell::{Cell, RefCell};

use libi3::ipc::{
    I3_IPC_EVENT_MASK, I3_IPC_MAGIC, I3_IPC_MESSAGE_TYPE_GET_OUTPUTS,
    I3_IPC_MESSAGE_TYPE_GET_WORKSPACES, I3_IPC_MESSAGE_TYPE_RUN_COMMAND,
    I3_IPC_MESSAGE_TYPE_SUBSCRIBE, I3_IPC_REPLY_TYPE_BAR_CONFIG, I3_IPC_REPLY_TYPE_COMMAND,
    I3_IPC_REPLY_TYPE_OUTPUTS, I3_IPC_REPLY_TYPE_SUBSCRIBE, I3_IPC_REPLY_TYPE_WORKSPACES,
};

use crate::child;
use crate::common::statusline;
use crate::config::{self, config, config_mut, free_colors};
use crate::outputs::{free_outputs, outputs};
use crate::xcb::{self, clean_xcb, DrawBackend, XConnLike};
use crate::{DLOG, ELOG};

/// C: i3bar/src/ipc.h — the i3 message types i3bar sends (from include/i3/ipc.h).
pub use libi3::ipc::I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG;
pub use libi3::ipc::I3_IPC_MESSAGE_TYPE_SYNC;
pub use libi3::ipc::I3_IPC_MESSAGE_TYPE_RUN_COMMAND as I3_IPC_RUN_COMMAND;

/// C: XCB_BUTTON_RELEASE (the response_type of a release event).
pub const XCB_BUTTON_RELEASE_EVENT: u8 = 5;

thread_local! {
    /// C: ipc.c:24 ev_io *i3_connection; — only the fd is kept.
    static I3_CONNECTION_FD: Cell<i32> = const { Cell::new(0) };
    /// C: ipc.c:26 const char *sock_path;
    static SOCK_PATH: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Rust-only: the messages the port "sent" when no socket is connected
    /// (unit tests assert on this instead of writing to a pipe).
    static SENT_LOG: RefCell<Vec<(u32, Option<String>)>> = const { RefCell::new(Vec::new()) };
}

/// C: ipc.c:32 static bool i3_provides_workspaces(void)
pub fn i3_provides_workspaces() -> bool {
    config(|c| !c.disable_ws && c.workspace_command.is_none())
}

/// The socket fd (0 when not connected).
pub fn i3_connection_fd() -> i32 {
    I3_CONNECTION_FD.with(|f| f.get())
}

/// Test helper: every message passed to i3_send_msg() while disconnected.
pub fn sent_messages() -> Vec<(u32, Option<String>)> {
    SENT_LOG.with(|l| l.borrow().clone())
}

/// Test helper: drop the recorded messages.
pub fn clear_sent_messages() {
    SENT_LOG.with(|l| l.borrow_mut().clear());
}

/// C: ipc.c:365 int i3_send_msg(uint32_t type, const char *payload)
pub fn i3_send_msg(message_type: u32, payload: Option<&str>) -> i32 {
    let fd = i3_connection_fd();
    if fd <= 0 {
        // Not connected (unit tests): record the message, exactly what the C
        // code would have written to the socket.
        SENT_LOG.with(|l| l.borrow_mut().push((message_type, payload.map(|p| p.to_string()))));
        return 1;
    }

    let len = payload.map(|p| p.len()).unwrap_or(0) as u32;
    libi3::ipc_send_message(fd, len, message_type, payload.map(|p| p.as_bytes()).unwrap_or(&[]))
}

/// C: ipc.c:388 void init_connection(const char *socket_path)
pub fn init_connection(socket_path: &str) -> i32 {
    SOCK_PATH.with(|p| *p.borrow_mut() = Some(socket_path.to_string()));
    let sockfd = libi3::ipc_connect(Some(socket_path));
    I3_CONNECTION_FD.with(|f| f.set(sockfd));
    sockfd
}

/// C: ipc.c:398 void destroy_connection(void)
pub fn destroy_connection() {
    let fd = i3_connection_fd();
    if fd > 0 {
        unsafe { libc::close(fd) };
    }
    I3_CONNECTION_FD.with(|f| f.set(0));
}

/// C: ipc.c:405 void subscribe_events(void)
pub fn subscribe_events() {
    if i3_provides_workspaces() {
        i3_send_msg(
            I3_IPC_MESSAGE_TYPE_SUBSCRIBE,
            Some("[ \"workspace\", \"output\", \"mode\", \"barconfig_update\" ]"),
        );
    } else {
        i3_send_msg(
            I3_IPC_MESSAGE_TYPE_SUBSCRIBE,
            Some("[ \"output\", \"mode\", \"barconfig_update\" ]"),
        );
    }
}

// ---------------------------------------------------------------------------
// reply handlers (C: ipc.c:34-197)
// ---------------------------------------------------------------------------

/// C: ipc.c:40 static void got_command_reply(const unsigned char *reply, size_t size)
fn got_command_reply(_reply: &[u8], _size: usize) {
    // TODO: Error handling for command replies
}

/// C: ipc.c:48 static void got_workspace_reply(const unsigned char *reply, size_t size)
fn got_workspace_reply(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, reply: &[u8], size: usize) {
    DLOG!("Got workspace data!");
    crate::workspaces::parse_workspaces_json(&reply[..size]);
    xcb::draw_bars(x, draw, false);
}

/// C: ipc.c:59 static void got_subscribe_reply(const unsigned char *reply, size_t size)
fn got_subscribe_reply(reply: &[u8], _size: usize) {
    DLOG!("Got subscribe reply: {}", String::from_utf8_lossy(reply));
    // TODO: Error handling for subscribe commands
}

/// C: ipc.c:68 static void got_output_reply(const unsigned char *reply, size_t size)
fn got_output_reply(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, reply: &[u8], size: usize) {
    DLOG!("Clearing old output configuration...");
    free_outputs(x);

    DLOG!("Parsing outputs JSON...");
    crate::outputs::parse_outputs_json(&reply[..size]);
    DLOG!("Reconfiguring windows...");
    xcb::reconfig_windows(x, false);

    let count = outputs(|outputs| outputs.len());
    for idx in 0..count {
        xcb::kick_tray_clients(x, idx);
    }

    if i3_provides_workspaces() {
        i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_WORKSPACES, None);
    } else if config(|c| c.workspace_command.is_some()) {
        // Communication with the workspace child is one-way. Since we called
        // free_outputs() and free_workspaces() we have lost our workspace
        // information which will result in no workspace buttons. A
        // well-behaving client should be subscribed to output events as well and
        // re-send the output information to i3bar. Even in that case though
        // there is a race condition where the child can send the new workspace
        // information after the output change before i3bar receives the output
        // event from i3. For this reason, we re-parse the latest received JSON.
        child::repeat_last_ws_json();
    }

    xcb::draw_bars(x, draw, false);
}

/// C: ipc.c:100 static void got_bar_config(const unsigned char *reply, size_t size)
fn got_bar_config(x: &mut dyn XConnLike, reply: &[u8], size: usize) {
    if config(|c| c.bar_id.is_none()) {
        DLOG!("Received bar list \"{}\"", String::from_utf8_lossy(reply));
        config::parse_get_first_i3bar_config(&reply[..size]);

        let bar_id = config(|c| c.bar_id.clone());
        if bar_id.is_none() {
            ELOG!("No bar configuration found, please configure a bar block in your i3 config file.");
            std::process::exit(1);
        }

        let bar_id = bar_id.unwrap();
        DLOG!("Using first bar config: {}. Use --bar_id to manually select a different bar configuration.", bar_id);
        i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG, Some(&bar_id));
        return;
    }

    DLOG!("Received bar config \"{}\"", String::from_utf8_lossy(reply));
    // We initiate the main function by requesting infos about the outputs and
    // workspaces. Everything else (creating the bars, showing the right
    // workspace-buttons and more) is taken care of by the event-drivenness of
    // the code.
    i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_OUTPUTS, None);

    config_mut(|c| free_colors(&mut c.colors));
    config::parse_config_json(&reply[..size]);

    // Now we can actually use 'config', so let's subscribe to the appropriate
    // events and request the workspaces if necessary.
    subscribe_events();
    if i3_provides_workspaces() {
        i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_WORKSPACES, None);
    }

    // Initialize the rest of XCB
    let fontname = config(|c| c.fontname.clone());
    xcb::init_xcb_late(fontname.as_deref(), x);

    // Resolve color strings to colorpixels and save them, then free the
    // strings.
    let colors = config(|c| c.colors.clone());
    xcb::init_colors(&colors, x);
    config_mut(|c| free_colors(&mut c.colors));

    let command = config(|c| c.command.clone());
    child::start_child(command.as_deref());
    let workspace_command = config(|c| c.workspace_command.clone());
    child::start_ws_child(workspace_command.as_deref());
}

// ---------------------------------------------------------------------------
// event handlers (C: ipc.c:216-260)
// ---------------------------------------------------------------------------

/// C: ipc.c:222 static void got_workspace_event(const unsigned char *event, size_t size)
fn got_workspace_event(_event: &[u8], _size: usize) {
    DLOG!("Got workspace event!");
    i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_WORKSPACES, None);
}

/// C: ipc.c:232 static void got_output_event(const unsigned char *event, size_t size)
fn got_output_event(_event: &[u8], _size: usize) {
    DLOG!("Got output event!");
    i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_OUTPUTS, None);
}

/// C: ipc.c:242 static void got_mode_event(const unsigned char *event, size_t size)
fn got_mode_event(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, event: &[u8], size: usize) {
    DLOG!("Got mode event!");
    crate::mode::parse_mode_json(&event[..size]);
    xcb::draw_bars(x, draw, false);
}

/// C: ipc.c:252 static bool strings_differ(char *a, char *b)
fn strings_differ(a: Option<&str>, b: Option<&str>) -> bool {
    let a_null = a.is_none();
    let b_null = b.is_none();
    if a_null != b_null {
        return true;
    }
    if a_null && b_null {
        return false;
    }
    a != b
}

/// C: ipc.c:264 static void got_bar_config_update(const unsigned char *event, size_t size)
fn got_bar_config_update(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, event: &[u8], size: usize) {
    // check whether this affect this bar instance by checking the bar_id
    let bar_id = config(|c| c.bar_id.clone()).unwrap_or_default();
    let expected_id = format!("\"id\":\"{}\"", bar_id);
    let haystack = String::from_utf8_lossy(&event[..size]).into_owned();
    if !haystack.contains(&expected_id) {
        return;
    }

    // reconfigure the bar based on the current outputs
    i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_OUTPUTS, None);

    config_mut(|c| free_colors(&mut c.colors));

    // update the configuration with the received settings
    DLOG!("Received bar config update \"{}\"", haystack);

    let old_command = config(|c| c.command.clone());
    let old_workspace_command = config(|c| c.workspace_command.clone());
    config_mut(|c| {
        c.command = None;
        c.workspace_command = None;
    });
    let old_mode = config(|c| c.hide_on_modifier);

    config::parse_config_json(&event[..size]);
    if old_mode != config(|c| c.hide_on_modifier) {
        xcb::reconfig_windows(x, true);
    }

    // update fonts and colors
    let fontname = config(|c| c.fontname.clone());
    xcb::init_xcb_late(fontname.as_deref(), x);
    let colors = config(|c| c.colors.clone());
    xcb::init_colors(&colors, x);

    // restart status command process
    let command = config(|c| c.command.clone());
    if !child::status_child_is_alive() || strings_differ(old_command.as_deref(), command.as_deref()) {
        child::kill_child();
        statusline::head(|head| child::clear_statusline(head, true));
        child::start_child(command.as_deref());
    }

    // restart workspace command process
    let workspace_command = config(|c| c.workspace_command.clone());
    if !child::ws_child_is_alive()
        || strings_differ(old_workspace_command.as_deref(), workspace_command.as_deref())
    {
        crate::workspaces::free_workspaces();
        child::kill_ws_child();
        child::start_ws_child(workspace_command.as_deref());
    }

    xcb::draw_bars(x, draw, false);
}

// ---------------------------------------------------------------------------
// the message loop body
// ---------------------------------------------------------------------------

fn read_exact_fd(fd: i32, buf: &mut [u8]) -> Result<(), i32> {
    let mut rec = 0usize;
    while rec < buf.len() {
        let n = unsafe {
            libc::read(fd, buf[rec..].as_mut_ptr() as *mut libc::c_void, buf.len() - rec)
        };
        if n == -1 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            ELOG!("read() failed: {}", err);
            return Err(-1);
        }
        if n == 0 {
            // EOF received.
            return Err(0);
        }
        rec += n as usize;
    }
    Ok(())
}

/// C: ipc.c:288 static void got_data() — reads and dispatches one message.
pub fn got_data(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend) {
    DLOG!("Got data!");
    let fd = i3_connection_fd();
    if fd <= 0 {
        return;
    }

    // First we only read the header, because we know its length.
    let header_len = I3_IPC_MAGIC.len() + std::mem::size_of::<u32>() * 2;
    let mut header = vec![0u8; header_len];

    if let Err(code) = read_exact_fd(fd, &mut header) {
        if code == 0 {
            // EOF received. Since i3 will restart i3bar instances as
            // appropriate, we exit here.
            DLOG!("EOF received, exiting...");
            clean_xcb(x);
            std::process::exit(0);
        }
        std::process::exit(1);
    }

    if !header.starts_with(I3_IPC_MAGIC.as_bytes()) {
        ELOG!(
            "Wrong magic code: {} Expected: {}",
            String::from_utf8_lossy(&header[..I3_IPC_MAGIC.len()]),
            I3_IPC_MAGIC
        );
        std::process::exit(1);
    }

    let mut walk = I3_IPC_MAGIC.len();
    let size = u32::from_ne_bytes(header[walk..walk + 4].try_into().unwrap());
    walk += 4;
    let message_type = u32::from_ne_bytes(header[walk..walk + 4].try_into().unwrap());

    // Now that we know, what to expect, we can start read()ing the rest of the
    // message.
    let mut buffer = vec![0u8; size as usize];
    if size > 0 {
        if read_exact_fd(fd, &mut buffer).is_err() {
            ELOG!("Nothing to read!");
            std::process::exit(1);
        }
    }

    // And call the callback (indexed by the type).
    if message_type & I3_IPC_EVENT_MASK != 0 {
        let event_type = message_type ^ I3_IPC_EVENT_MASK;
        // C: event_handlers[] = { workspace, output, mode, NULL, barconfig_update }
        match event_type {
            0 => got_workspace_event(&buffer, size as usize),
            1 => got_output_event(&buffer, size as usize),
            2 => got_mode_event(x, draw, &buffer, size as usize),
            3 => {}
            4 => got_bar_config_update(x, draw, &buffer, size as usize),
            _ => {}
        }
    } else {
        // C: reply_handlers[] — indexed by the reply type.
        match message_type {
            I3_IPC_REPLY_TYPE_COMMAND => got_command_reply(&buffer, size as usize),
            I3_IPC_REPLY_TYPE_WORKSPACES => got_workspace_reply(x, draw, &buffer, size as usize),
            I3_IPC_REPLY_TYPE_SUBSCRIBE => got_subscribe_reply(&buffer, size as usize),
            I3_IPC_REPLY_TYPE_OUTPUTS => got_output_reply(x, draw, &buffer, size as usize),
            I3_IPC_REPLY_TYPE_BAR_CONFIG => got_bar_config(x, &buffer, size as usize),
            _ => {}
        }
    }
}

/// Unused re-export kept so call sites in other modules can name the constant.
pub const _MESSAGE_TYPE_UNUSED: u32 = I3_IPC_MESSAGE_TYPE_RUN_COMMAND;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{config_mut, reset_config};
    use crate::outputs::init_outputs;
    use crate::xcb::test_support::NullConn;
    use crate::xcb::RecordingBackend;
    use libi3::ipc::I3_IPC_MESSAGE_TYPE_GET_OUTPUTS;

    fn setup() {
        reset_config();
        init_outputs();
        clear_sent_messages();
        crate::common::statusline::reset();
    }

    #[test]
    fn i3_provides_workspaces_matrix() {
        setup();
        assert!(i3_provides_workspaces());

        config_mut(|c| c.disable_ws = true);
        assert!(!i3_provides_workspaces());

        setup();
        config_mut(|c| c.workspace_command = Some("ws-client".to_string()));
        assert!(!i3_provides_workspaces());

        // Both set: still no workspaces from i3.
        setup();
        config_mut(|c| {
            c.disable_ws = true;
            c.workspace_command = Some("ws-client".to_string());
        });
        assert!(!i3_provides_workspaces());
    }

    #[test]
    fn subscribe_events_payloads() {
        setup();
        subscribe_events();
        assert_eq!(
            sent_messages(),
            vec![(
                I3_IPC_MESSAGE_TYPE_SUBSCRIBE,
                Some("[ \"workspace\", \"output\", \"mode\", \"barconfig_update\" ]".to_string())
            )]
        );

        setup();
        config_mut(|c| c.workspace_command = Some("ws-client".to_string()));
        subscribe_events();
        assert_eq!(
            sent_messages(),
            vec![(
                I3_IPC_MESSAGE_TYPE_SUBSCRIBE,
                Some("[ \"output\", \"mode\", \"barconfig_update\" ]".to_string())
            )]
        );
    }

    #[test]
    fn i3_send_msg_records_without_a_socket() {
        setup();
        assert_eq!(i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_WORKSPACES, None), 1);
        assert_eq!(
            sent_messages(),
            vec![(I3_IPC_MESSAGE_TYPE_GET_WORKSPACES, None)]
        );

        clear_sent_messages();
        i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG, Some("bar-0"));
        assert_eq!(
            sent_messages(),
            vec![(I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG, Some("bar-0".to_string()))]
        );
    }

    #[test]
    fn bar_config_update_for_another_bar_is_ignored() {
        setup();
        config_mut(|c| c.bar_id = Some("bar-0".to_string()));
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();

        // The event names a different bar id -> not our business, no GET_OUTPUTS.
        let event = br#"{"id":"bar-1","hidden_state":"show"}"#;
        got_bar_config_update(&mut x, &mut draw, event, event.len());
        assert!(sent_messages().is_empty());

        // Our own id matches the C strstr() check, so the bar re-requests the
        // outputs.
        let event = br#"{"id":"bar-0","hidden_state":"show"}"#;
        got_bar_config_update(&mut x, &mut draw, event, event.len());
        assert_eq!(sent_messages()[0], (I3_IPC_MESSAGE_TYPE_GET_OUTPUTS, None));
        assert!(config(|c| c.hidden_state) == crate::config::hidden_state_t::S_SHOW);
    }

    #[test]
    fn strings_differ_matches_c() {
        assert!(!strings_differ(None, None));
        assert!(strings_differ(Some("a"), None));
        assert!(strings_differ(None, Some("a")));
        assert!(!strings_differ(Some("a"), Some("a")));
        assert!(strings_differ(Some("a"), Some("b")));
    }

    #[test]
    fn workspace_and_output_events_request_fresh_data() {
        setup();
        got_workspace_event(&[], 0);
        got_output_event(&[], 0);
        assert_eq!(
            sent_messages(),
            vec![
                (I3_IPC_MESSAGE_TYPE_GET_WORKSPACES, None),
                (I3_IPC_MESSAGE_TYPE_GET_OUTPUTS, None),
            ]
        );
    }
}
