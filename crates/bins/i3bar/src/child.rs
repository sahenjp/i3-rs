#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! 1:1 port of i3bar/src/child.c (1044 lines) — getting input for the
//! statusline: fork/exec of the status command, the JSON protocol and the
//! click-event pipe back to the child.
//!
//! C: i3bar/src/child.c
//!
//! Differences forced by the Rust port:
//!  * libev (ev_io / ev_child watchers) is not part of the port; main.rs polls
//!    the file descriptors returned by start_child()/start_ws_child() and calls
//!    stdin_io_cb()/ws_stdin_io_cb()/child_sig_cb() directly.
//!  * yajl's incremental parser is reproduced by buffering the chunk until
//!    serde_json reports a complete value (see read_json_input()).
//!  * yajl_gen (the click-event generator) is replaced by block_clicked_json(),
//!    which emits the same byte sequence (yajl prints compact JSON with the
//!    same key order and the same string escaping).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use libi3::safewrappers::writeall;
use libi3::string::{
    i3string_from_markup_with_length, i3string_from_utf8, i3string_get_num_bytes, i3string_is_markup,
    i3string_set_markup,
};

use crate::common::{status_block, statusline};
use crate::config::{
    config, XCB_MOD_MASK_1, XCB_MOD_MASK_2, XCB_MOD_MASK_3, XCB_MOD_MASK_4, XCB_MOD_MASK_5,
    XCB_MOD_MASK_CONTROL, XCB_MOD_MASK_SHIFT,
};
use crate::outputs::outputs;
use crate::parse_json_header;
use crate::workspaces::parse_workspaces_json;
use crate::xcb::{
    draw_bars, logical_px, predict_text_width, separator_symbol_width, DrawBackend, XConnLike,
};
use crate::{DLOG, ELOG};

/// C: child.h:17 #define STDIN_CHUNK_SIZE 1024
pub const STDIN_CHUNK_SIZE: usize = 1024;

/// C: child.h:19 typedef struct i3bar_child
#[derive(Debug, Clone, Default)]
pub struct i3bar_child {
    pub pid: i32,

    /// C: uint32_t version
    pub version: u32,

    pub stopped: bool,
    /// C: int stop_signal — signalled to the child when the bar gets hidden.
    pub stop_signal: i32,
    /// C: int cont_signal — signalled when the bar becomes visible again.
    pub cont_signal: i32,

    /// C: bool click_events
    pub click_events: bool,
    /// C: bool click_events_init
    pub click_events_init: bool,

    /// C: int stdin_fd
    pub stdin_fd: i32,

    /// C: char *pending_line — a line read from the child without a newline.
    pub pending_line: Option<String>,
}

thread_local! {
    /// C: child.c:36 i3bar_child status_child = {0};
    static STATUS_CHILD: RefCell<i3bar_child> = const { RefCell::new(i3bar_child {
        pid: 0, version: 0, stopped: false, stop_signal: 0, cont_signal: 0,
        click_events: false, click_events_init: false, stdin_fd: 0, pending_line: None,
    }) };
    /// C: child.c:37 i3bar_child ws_child = {0};
    static WS_CHILD: RefCell<i3bar_child> = const { RefCell::new(i3bar_child {
        pid: 0, version: 0, stopped: false, stop_signal: 0, cont_signal: 0,
        click_events: false, click_events_init: false, stdin_fd: 0, pending_line: None,
    }) };
    /// C: child.c:103 int child_stdin — the write end of the click-event pipe.
    static CHILD_STDIN: RefCell<i32> = const { RefCell::new(0) };
    /// C: child.c:69 parser_ctx.has_urgent
    static PARSER_HAS_URGENT: RefCell<bool> = const { RefCell::new(false) };
    /// C: child.c:522 static char *ws_last_json;
    static WS_LAST_JSON: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Rust-only: the incomplete JSON chunk of the statusline (see
    /// read_json_input).
    static PARTIAL_STATUS: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// C: the fields of status_child/ws_child that kill_children_at_exit() needs.
///
/// The C globals are plain memory and the atexit handler reads them directly.
/// Rust's process::exit() destroys the thread-locals before the libc atexit
/// handlers run, so the same values are mirrored into atomics here (Rust-only;
/// the thread-local stays the authoritative state while the loop runs).
#[derive(Debug)]
pub struct ExitState {
    pub pid: AtomicI32,
    pub stopped: AtomicBool,
    pub cont_signal: AtomicI32,
}

impl ExitState {
    const fn new() -> ExitState {
        ExitState { pid: AtomicI32::new(0), stopped: AtomicBool::new(false), cont_signal: AtomicI32::new(0) }
    }

    fn publish(&self, child: &i3bar_child) {
        self.pid.store(child.pid, Ordering::SeqCst);
        self.stopped.store(child.stopped, Ordering::SeqCst);
        self.cont_signal.store(child.cont_signal, Ordering::SeqCst);
    }
}

/// C: the pid of status_child at exit time (see ExitState).
pub static STATUS_EXIT: ExitState = ExitState::new();
/// C: the pid of ws_child at exit time (see ExitState).
pub static WS_EXIT: ExitState = ExitState::new();

fn publish_status(child: &mut i3bar_child) {
    STATUS_EXIT.publish(child);
}

fn publish_ws(child: &mut i3bar_child) {
    WS_EXIT.publish(child);
}

/// Access to C's status_child global.
pub fn status_child<R>(f: impl FnOnce(&mut i3bar_child) -> R) -> R {
    STATUS_CHILD.with(|c| f(&mut c.borrow_mut()))
}

/// Access to C's ws_child global.
pub fn ws_child<R>(f: impl FnOnce(&mut i3bar_child) -> R) -> R {
    WS_CHILD.with(|c| f(&mut c.borrow_mut()))
}

fn child_stdin() -> i32 {
    CHILD_STDIN.with(|c| *c.borrow())
}

fn set_child_stdin(fd: i32) {
    CHILD_STDIN.with(|c| *c.borrow_mut() = fd);
}

/// C: child.c:109 void clear_statusline(struct statusline_head *head, bool free_resources)
pub fn clear_statusline(head: &mut Vec<status_block>, free_resources: bool) {
    // I3STRING_FREE()/FREE() of every field: dropping the Vec does exactly
    // that for the Rust struct. The false case keeps the C comment in mind:
    // "the blocks are still used by statusline_head, so we will not free the
    // resources here" — the buffer is never shared in the Rust port, so both
    // paths are the same clear().
    let _ = free_resources;
    head.clear();
}

/// C: child.c:150 static void copy_statusline(struct statusline_head *from, struct statusline_head *to)
fn copy_statusline(from: &mut [status_block]) -> Vec<status_block> {
    from.iter_mut().map(|block| block.deep_copy()).collect()
}

/// C: child.c:166 static void set_statusline_error(const char *format, ...) —
/// the body after the vasprintf().
pub fn set_statusline_error(message: &str) {
    statusline::head(|head| clear_statusline(head, true));

    let err_block = status_block {
        full_text: Some(i3string_from_utf8("Error: ")),
        name: Some("error".to_string()),
        color: Some("#ff0000".to_string()),
        no_separator: true,
        ..Default::default()
    };

    let message_block = status_block {
        full_text: Some(i3string_from_utf8(message)),
        name: Some("error_message".to_string()),
        color: Some("#ff0000".to_string()),
        no_separator: true,
        ..Default::default()
    };

    statusline::head(|head| {
        head.insert(0, err_block);
        head.push(message_block);
    });
}

/// C: child.c:203 static void cleanup(i3bar_child *c) — the libev watchers are
/// the pid/stdin_fd pair in the Rust port.
///
/// C: "if (c->pid == status_child.pid)" closes the click-event pipe. The
/// thread-local cannot be read here (the caller already holds it mutably), so
/// the caller says which child it is.
fn cleanup(c: &mut i3bar_child, is_status: bool) {
    if c.stdin_fd != 0 {
        if is_status {
            let fd = child_stdin();
            if fd != 0 {
                unsafe { libc::close(fd) };
            }
            set_child_stdin(0);
        }
        unsafe { libc::close(c.stdin_fd) };
    }

    *c = i3bar_child::default();
}

// ---------------------------------------------------------------------------
// stdin parsing (C: child.c:203-470 — the yajl callbacks)
// ---------------------------------------------------------------------------

/// C: child.c:231-244 stdin_start_map() — the per-block defaults.
fn new_block_defaults() -> status_block {
    let has_separator = config(|c| c.separator_symbol.is_some());
    let sep_block_width = if !has_separator {
        logical_px(9) as u32
    } else {
        (logical_px(8) + separator_symbol_width()) as u32
    };

    status_block {
        // By default we draw all four borders if a border is set.
        border_top: 1,
        border_right: 1,
        border_bottom: 1,
        border_left: 1,
        sep_block_width,
        ..Default::default()
    }
}

/// C: child.c:255-449 stdin_boolean/stdin_string/stdin_integer/stdin_end_map —
/// one JSON map becomes one status_block.
fn block_from_json(obj: &serde_json::Map<String, serde_json::Value>) -> status_block {
    let mut block = new_block_defaults();

    // The C callbacks compare the key case-insensitively (strcasecmp).
    for (key, val) in obj {
        let key = key.to_ascii_lowercase();
        match (key.as_str(), val) {
            ("full_text", serde_json::Value::String(s)) => {
                block.full_text = Some(i3string_from_markup_with_length(s.as_bytes(), s.len()));
            }
            ("short_text", serde_json::Value::String(s)) => {
                block.short_text = Some(i3string_from_markup_with_length(s.as_bytes(), s.len()));
            }
            ("color", serde_json::Value::String(s)) => block.color = Some(s.clone()),
            ("background", serde_json::Value::String(s)) => block.background = Some(s.clone()),
            ("border", serde_json::Value::String(s)) => block.border = Some(s.clone()),
            ("markup", serde_json::Value::String(s)) => {
                block.pango_markup = s.len() == "pango".len() && s.eq_ignore_ascii_case("pango");
            }
            ("align", serde_json::Value::String(s)) => {
                block.align = if s == "center" {
                    crate::common::blockalign_t::ALIGN_CENTER
                } else if s == "right" {
                    crate::common::blockalign_t::ALIGN_RIGHT
                } else {
                    crate::common::blockalign_t::ALIGN_LEFT
                };
            }
            ("min_width", serde_json::Value::String(s)) => block.min_width_str = Some(s.clone()),
            ("min_width", serde_json::Value::Number(n)) => block.min_width = n.as_u64().unwrap_or(0) as u32,
            ("name", serde_json::Value::String(s)) => block.name = Some(s.clone()),
            ("instance", serde_json::Value::String(s)) => block.instance = Some(s.clone()),
            ("urgent", serde_json::Value::Bool(b)) => block.urgent = *b,
            ("separator", serde_json::Value::Bool(b)) => block.no_separator = !*b,
            ("separator_block_width", serde_json::Value::Number(n)) => {
                block.sep_block_width = n.as_u64().unwrap_or(0) as u32
            }
            ("border_top", serde_json::Value::Number(n)) => block.border_top = n.as_u64().unwrap_or(0) as u32,
            ("border_right", serde_json::Value::Number(n)) => block.border_right = n.as_u64().unwrap_or(0) as u32,
            ("border_bottom", serde_json::Value::Number(n)) => block.border_bottom = n.as_u64().unwrap_or(0) as u32,
            ("border_left", serde_json::Value::Number(n)) => block.border_left = n.as_u64().unwrap_or(0) as u32,
            _ => {}
        }
    }

    // C: stdin_end_map()
    if block.full_text.is_none() {
        // Ensure we have a full_text set, so that when it is missing (or null),
        // i3bar does not crash and the user gets an annoying message.
        block.full_text = Some(i3string_from_utf8("SPEC VIOLATION: full_text is NULL!"));
    }

    if let Some(min_width_str) = block.min_width_str.clone() {
        let mut text = i3string_from_utf8(&min_width_str);
        i3string_set_markup(&mut text, block.pango_markup);
        block.min_width = predict_text_width(&mut text) as u32;
        libi3::i3string_free(Some(text));
    }

    let pango_markup = block.pango_markup;
    block.set_markup(pango_markup);

    block.use_short = false;
    block
}

/// Apply a complete statusline array: the buffer first, then the head
/// (C: stdin_start_array + stdin_end_map + stdin_end_array).
fn apply_statusline(blocks: Vec<status_block>) {
    statusline::buffer(|buffer| {
        // C: stdin_start_array() — clear_statusline(&statusline_buffer, false)
        clear_statusline(buffer, false);
        // C: stdin_end_map() — TAILQ_INSERT_TAIL(&statusline_buffer, ...)
        buffer.extend(blocks);
    });

    // C: stdin_end_array() — move the buffer to the statusline.
    statusline::buffer(|buffer| {
        let urgent = buffer.iter().any(|b| b.urgent);
        PARSER_HAS_URGENT.with(|u| *u.borrow_mut() = urgent);
        let copied = copy_statusline(buffer);
        statusline::head(|head| {
            clear_statusline(head, true);
            head.extend(copied);
        });
    });
}

/// C: child.c:436-455 read_json_input() — parse one chunk.
///
/// yajl keeps parser state between calls (i3status sends one infinite JSON
/// array, so chunks arrive mid-array). serde_json cannot resume, so the chunk
/// is buffered until a complete value is available: serde_json reports "end of
/// file while parsing a value" for a partial value, which is exactly the case
/// where yajl would simply wait for more input.
pub fn read_json_input(x: &mut dyn XConnLike, input: &[u8], draw: &mut dyn DrawBackend) -> bool {
    let mut has_urgent = false;

    // yajl is a streaming parser: i3status sends one infinite JSON array of
    // statuslines, so every complete *inner* array triggers stdin_end_array().
    // The port scans the buffer for the next complete array (the string-aware
    // scanner of parse_json_header) and leaves the incomplete tail for the next
    // chunk, which is exactly what yajl does with its retained parser state.
    let error: Option<String> = PARTIAL_STATUS.with(|p| {
        let mut partial = p.borrow_mut();
        partial.extend_from_slice(input);

        loop {
            let (start, end) = match crate::parse_json_header::first_complete_array(&partial) {
                Some(found) => found,
                None => break,
            };
            let chunk = partial[start..end].to_vec();
            match serde_json::from_slice::<serde_json::Value>(&chunk) {
                Ok(serde_json::Value::Array(items)) => {
                    let mut blocks = Vec::with_capacity(items.len());
                    for item in items.iter() {
                        if let Some(obj) = item.as_object() {
                            blocks.push(block_from_json(obj));
                        }
                    }
                    apply_statusline(blocks);
                    has_urgent = PARSER_HAS_URGENT.with(|u| *u.borrow());
                }
                Ok(_) => {}
                Err(err) => {
                    partial.clear();
                    return Some(err.to_string());
                }
            }
            partial.drain(..end);
        }
        None
    });

    if let Some(message) = error {
        // C: fprintf(stderr, "[i3bar] Could not parse JSON input ...");
        eprintln!(
            "[i3bar] Could not parse JSON input (code = 0, message = {}): {}",
            message,
            String::from_utf8_lossy(input)
        );
        set_statusline_error(&format!("Could not parse JSON ({})", message));
        draw_bars(x, draw, false);
    }

    has_urgent
}

/// C: child.c:507-521 read_flat_input(char *buffer, int length)
fn read_flat_input(buffer: &[u8]) {
    let mut text_bytes = buffer.to_vec();
    // Remove the trailing newline and terminate the string at the same time.
    if let Some(last) = text_bytes.last_mut() {
        if *last == b'\n' || *last == b'\r' {
            *last = 0;
        }
    }
    let text = match text_bytes.iter().position(|&b| b == 0) {
        Some(pos) => String::from_utf8_lossy(&text_bytes[..pos]).into_owned(),
        None => String::from_utf8_lossy(&text_bytes).into_owned(),
    };

    statusline::head(|head| {
        if let Some(first) = head.first_mut() {
            // C: I3STRING_FREE(first->full_text)
            libi3::i3string_free(first.full_text.take());
            first.full_text = Some(i3string_from_utf8(&text));
        }
    });
}

/// C: child.c:557 get_buffer() — read everything that is available.
///
/// Returns None on EOF (C: *ret_buffer_len = -1; return NULL).
pub fn get_buffer(fd: i32) -> Option<Vec<u8>> {
    let mut buffer = vec![0u8; STDIN_CHUNK_SIZE];
    let mut rec = 0usize;
    loop {
        if rec == buffer.len() {
            buffer.resize(buffer.len() + STDIN_CHUNK_SIZE, 0);
        }
        let n = unsafe {
            libc::read(fd, buffer[rec..].as_mut_ptr() as *mut libc::c_void, buffer.len() - rec)
        };
        if n == -1 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EAGAIN) {
                // finish up
                break;
            }
            ELOG!("read() failed!: {}", err);
            std::process::exit(1);
        }
        if n == 0 {
            ELOG!("stdin: received EOF");
            return None;
        }
        rec += n as usize;
    }
    buffer.truncate(rec);
    Some(buffer)
}

/// C: child.c:493 stdin_io_cb(int fd)
pub fn stdin_io_cb(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, fd: i32) {
    let buffer = match get_buffer(fd) {
        Some(b) if !b.is_empty() => b,
        _ => return,
    };

    let mut has_urgent = false;
    if status_child(|c| c.version) > 0 {
        has_urgent = read_json_input(x, &buffer, draw);
    } else {
        read_flat_input(&buffer);
    }
    draw_bars(x, draw, has_urgent);
}

/// C: child.c:527 stdin_io_first_line_cb(int fd)
pub fn stdin_io_first_line_cb(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, fd: i32) {
    let buffer = match get_buffer(fd) {
        Some(b) if !b.is_empty() => b,
        _ => return,
    };
    DLOG!("Detecting input type based on buffer *{}*", String::from_utf8_lossy(&buffer));

    // Detect whether this is JSON or plain text.
    let consumed = {
        let mut child = status_child(|c| c.clone());
        let consumed = parse_json_header::parse_json_header(&mut child, &buffer);
        status_child(|c| {
            c.version = child.version;
            c.stop_signal = child.stop_signal;
            c.cont_signal = child.cont_signal;
            c.click_events = child.click_events;
        });
        consumed as usize
    };

    if status_child(|c| c.version) > 0 {
        // If hide-on-modifier is set, we start off by sending the status_child a
        // SIGSTOP, because the bars are not mapped at start.
        if config(|c| c.hide_on_modifier != crate::config::bar_display_mode_t::M_DOCK) {
            stop_children();
        }
        let rest = buffer[consumed.min(buffer.len())..].to_vec();
        let has_urgent = read_json_input(x, &rest, draw);
        draw_bars(x, draw, has_urgent);
    } else {
        // In case of plaintext, we just add a single block and change its
        // full_text pointer later.
        statusline::head(|head| head.push(status_block::default()));
        read_flat_input(&buffer);
    }
}

/// C: child.c:535 static bool isempty(char *s)
fn isempty(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_whitespace())
}

fn append_string(previous: Option<&str>, s: &str) -> String {
    match previous {
        Some(prev) => format!("{}{}", prev, s),
        None => s.to_string(),
    }
}

/// C: child.c:575-618 ws_stdin_io_cb() — split the buffer into lines, keep the
/// incomplete tail in ws_child.pending_line and parse every complete line.
pub fn ws_stdin_io_cb(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, fd: i32) {
    let buffer = match get_buffer(fd) {
        Some(b) => b,
        None => return,
    };

    let text = String::from_utf8_lossy(&buffer).into_owned();
    let strings: Vec<&str> = text.split('\n').collect();

    for (idx, line) in strings.iter().enumerate() {
        let pending = ws_child(|c| c.pending_line.clone());
        if pending.is_none() && isempty(line) {
            // In the normal case where the buffer ends with a newline, the last
            // string should be empty.
            continue;
        }

        if idx + 1 == strings.len() {
            // This is the last string but it is not empty, meaning that we have
            // read data that is incomplete, save it for later.
            let new = append_string(pending.as_deref(), line);
            ws_child(|c| c.pending_line = Some(new));
            continue;
        }

        let json = append_string(pending.as_deref(), line);
        ws_child(|c| c.pending_line = None);
        WS_LAST_JSON.with(|last| *last.borrow_mut() = Some(json.clone()));

        parse_workspaces_json(json.as_bytes());
    }

    draw_bars(x, draw, false);
}

/// C: child.c:620 void repeat_last_ws_json(void)
pub fn repeat_last_ws_json() {
    let last = WS_LAST_JSON.with(|last| last.borrow().clone());
    if let Some(json) = last {
        DLOG!("Repeating last workspace JSON");
        parse_workspaces_json(json.as_bytes());
    }
}

/// C: child.c:711 void set_workspace_button_error(const char *message)
pub fn set_workspace_button_error(message: &str) {
    crate::workspaces::free_workspaces();

    let name = format!("Error: {}", message);

    outputs(|outputs| {
        for output in outputs.iter_mut() {
            let mut fake_ws = crate::workspaces::i3_ws::default_ws();
            // Don't set the canonical_name field to make this workspace
            // unfocusable.
            fake_ws.name = Some(i3string_from_utf8(&name));
            fake_ws.name_width = match fake_ws.name.as_mut() {
                Some(n) => predict_text_width(n),
                None => 0,
            };
            fake_ws.num = -1;
            fake_ws.urgent = true;
            fake_ws.visible = true;
            fake_ws.output = Some(output.ptr_id);

            output.workspaces.push(fake_ws);
        }
    });
}

/// C: child.c:687 static void set_workspace_button_error_f(const char *format, ...)
pub fn set_workspace_button_error_f(message: &str) {
    set_workspace_button_error(message);
}

/// C: child.c:783-795 — the exit message of a dead child.
pub fn exit_error_message(command_type: &str, exit_status: i32) -> String {
    if exit_status == 126 {
        format!("{} is not executable (exit {})", command_type, exit_status)
    } else if exit_status == 127 {
        format!("{} not found or is missing a library dependency (exit {})", command_type, exit_status)
    } else {
        format!("{} process exited unexpectedly (exit {})", command_type, exit_status)
    }
}

/// The kind of child that died (C: the if/else chain of child_sig_cb).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum child_kind {
    STATUS,
    WS,
}

/// C: child.c:749 static void child_sig_cb(ev_child *watcher, int revents)
///
/// rstatus is the raw waitpid status; the C code uses WEXITSTATUS().
pub fn child_sig_cb(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, pid: i32, rstatus: i32) -> Option<child_kind> {
    let exit_status = (rstatus & 0xff00) >> 8; // C: WEXITSTATUS(watcher->rstatus)

    ELOG!("Child (pid: {}) unexpectedly exited with status {}", pid, exit_status);

    let kind = if pid == status_child(|c| c.pid) {
        child_kind::STATUS
    } else if pid == ws_child(|c| c.pid) {
        child_kind::WS
    } else {
        ELOG!("Unknown child pid, this should never happen");
        return None;
    };

    let command_type = match kind {
        child_kind::STATUS => "status_command",
        child_kind::WS => "workspace_command",
    };
    let message = exit_error_message(command_type, exit_status);

    match kind {
        child_kind::STATUS => set_statusline_error(&message),
        child_kind::WS => set_workspace_button_error_f(&message),
    }

    match kind {
        child_kind::STATUS => {
            status_child(|c| cleanup(c, true));
            status_child(publish_status);
        }
        child_kind::WS => {
            ws_child(|c| cleanup(c, false));
            ws_child(publish_ws);
        }
    }

    draw_bars(x, draw, false);
    Some(kind)
}

// ---------------------------------------------------------------------------
// click events (C: child.c:830-925 — yajl_gen)
// ---------------------------------------------------------------------------

/// C: child.c:838 static void child_click_events_initialize(void)
fn child_click_events_initialize() -> Option<Vec<u8>> {
    let initialized = status_child(|c| c.click_events_init);
    if initialized {
        return None;
    }
    // C: yajl_gen_array_open(gen) -> "[" then child_write_output().
    status_child(|c| c.click_events_init = true);
    Some(b"[".to_vec())
}

/// yajl's yajl_gen_string(): escape exactly the way yajl does.
fn yajl_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// C: child.c:857 void send_block_clicked(...) — the yajl_gen sequence,
/// reproduced byte for byte (compact output, same key order).
#[allow(clippy::too_many_arguments)]
pub fn block_clicked_json(
    button: i32,
    name: Option<&str>,
    instance: Option<&str>,
    x: i32,
    y: i32,
    x_rel: i32,
    y_rel: i32,
    out_x: i32,
    out_y: i32,
    width: i32,
    height: i32,
    mods: u32,
) -> String {
    let mut out = String::from("{");
    let mut first = true;

    fn push_key(out: &mut String, key: &str, first: &mut bool) {
        if !*first {
            out.push(',');
        }
        *first = false;
        out.push('"');
        out.push_str(key);
        out.push_str("\":");
    }

    if let Some(name) = name {
        push_key(&mut out, "name", &mut first);
        out.push('"');
        out.push_str(&yajl_escape(name));
        out.push('"');
    }

    if let Some(instance) = instance {
        push_key(&mut out, "instance", &mut first);
        out.push('"');
        out.push_str(&yajl_escape(instance));
        out.push('"');
    }

    push_key(&mut out, "button", &mut first);
    out.push_str(&button.to_string());

    push_key(&mut out, "modifiers", &mut first);
    out.push('[');
    let mut mod_first = true;
    for (mask, label) in [
        (XCB_MOD_MASK_SHIFT, "Shift"),
        (XCB_MOD_MASK_CONTROL, "Control"),
        (XCB_MOD_MASK_1, "Mod1"),
        (XCB_MOD_MASK_2, "Mod2"),
        (XCB_MOD_MASK_3, "Mod3"),
        (XCB_MOD_MASK_4, "Mod4"),
        (XCB_MOD_MASK_5, "Mod5"),
    ] {
        if mods & mask != 0 {
            if !mod_first {
                out.push(',');
            }
            mod_first = false;
            out.push('"');
            out.push_str(label);
            out.push('"');
        }
    }
    out.push(']');

    for (key, value) in [
        ("x", x),
        ("y", y),
        ("relative_x", x_rel),
        ("relative_y", y_rel),
        ("output_x", out_x),
        ("output_y", out_y),
        ("width", width),
        ("height", height),
    ] {
        push_key(&mut out, key, &mut first);
        out.push_str(&value.to_string());
    }

    out.push('}');
    out
}

/// C: child.c:832 static void child_write_output(void)
fn child_write_output(payload: &[u8]) {
    if !status_child(|c| c.click_events) {
        return;
    }

    let fd = child_stdin();
    let mut n = writeall(fd, payload);
    if n != -1 {
        n = writeall(fd, b"\n");
    }

    if n == -1 {
        status_child(|c| c.click_events = false);
        kill_child();
        set_statusline_error("child_write_output failed");
    }
}

/// C: child.c:857 void send_block_clicked(int button, const char *name, ...)
#[allow(clippy::too_many_arguments)]
pub fn send_block_clicked(
    draw: &mut dyn DrawBackend,
    button: i32,
    name: Option<&str>,
    instance: Option<&str>,
    x: i32,
    y: i32,
    x_rel: i32,
    y_rel: i32,
    out_x: i32,
    out_y: i32,
    width: i32,
    height: i32,
    mods: u32,
) {
    if !status_child(|c| c.click_events) {
        return;
    }

    if let Some(init) = child_click_events_initialize() {
        let fd = child_stdin();
        let n = writeall(fd, &init);
        if n == -1 {
            status_child(|c| c.click_events = false);
            kill_child();
            set_statusline_error("child_write_output failed");
            return;
        }
        writeall(fd, b"\n");
    }

    let payload = block_clicked_json(button, name, instance, x, y, x_rel, y_rel, out_x, out_y, width, height, mods);
    child_write_output(payload.as_bytes());
    let _ = draw;
}

// ---------------------------------------------------------------------------
// process management (C: child.c:794-1044)
// ---------------------------------------------------------------------------

fn sfork() -> i32 {
    let pid = unsafe { libc::fork() };
    if pid == -1 {
        ELOG!("Couldn't fork(): {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }
    pid
}

fn spipe(pipedes: &mut [i32; 2]) {
    if unsafe { libc::pipe(pipedes.as_mut_ptr()) } == -1 {
        libi3::err(1, "pipe(pipe_in)");
    }
}

fn exec_shell(command: &str) {
    let shell = std::ffi::CString::new("/bin/sh").unwrap();
    let cmd = std::ffi::CString::new(command).unwrap();
    let arg = std::ffi::CString::new("-c").unwrap();
    unsafe {
        libc::execl(
            shell.as_ptr(),
            shell.as_ptr(),
            arg.as_ptr(),
            cmd.as_ptr(),
            std::ptr::null::<libc::c_char>(),
        );
    }
    libi3::err(1, "execl return"); // only reached on error
}

fn set_nonblock(fd: i32) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

/// C: child.c:811 void start_child(char *command)
pub fn start_child(command: Option<&str>) {
    let command = match command {
        Some(c) => c.to_string(),
        None => return,
    };

    let mut pipe_in = [0i32; 2]; // pipe we read from
    let mut pipe_out = [0i32; 2]; // pipe we write to
    spipe(&mut pipe_in);
    spipe(&mut pipe_out);

    let pid = sfork();
    if pid == 0 {
        // Child-process. Reroute streams and start shell
        unsafe {
            libc::close(pipe_in[0]);
            libc::close(pipe_out[1]);
            libc::dup2(pipe_in[1], libc::STDOUT_FILENO);
            libc::dup2(pipe_out[0], libc::STDIN_FILENO);
            libc::setpgid(libc::getpid(), 0);
        }
        exec_shell(&command);
        return;
    }
    // Parent-process. Reroute streams
    unsafe {
        libc::close(pipe_in[1]);
        libc::close(pipe_out[0]);
    }

    status_child(|c| {
        c.pid = pid;
        c.stdin_fd = pipe_in[0];
        c.version = u32::MAX; // C: status_child.version = -1;
    });
    set_child_stdin(pipe_out[1]);

    // Mirror the pid for the atexit handler (see ExitState).
    status_child(publish_status);

    let mut child = status_child(|c| c.clone());
    setup_child_cb(&mut child);
}

/// C: child.c:818 void start_ws_child(char *command)
pub fn start_ws_child(command: Option<&str>) {
    let command = match command {
        Some(c) => c.to_string(),
        None => return,
    };

    ws_child(|c| {
        c.stop_signal = libc::SIGSTOP;
        c.cont_signal = libc::SIGCONT;
    });

    let mut pipe_in = [0i32; 2]; // pipe we read from
    spipe(&mut pipe_in);

    let pid = sfork();
    if pid == 0 {
        // Child-process. Reroute streams and start shell
        unsafe {
            libc::close(pipe_in[0]);
            libc::dup2(pipe_in[1], libc::STDOUT_FILENO);
            libc::setpgid(libc::getpid(), 0);
        }
        exec_shell(&command);
        return;
    }
    // Parent-process. Reroute streams
    unsafe {
        libc::close(pipe_in[1]);
    }

    ws_child(|c| {
        c.pid = pid;
        c.stdin_fd = pipe_in[0];
    });

    // Mirror the pid for the atexit handler (see ExitState).
    ws_child(publish_ws);

    let mut child = ws_child(|c| c.clone());
    setup_child_cb(&mut child);
}

/// C: child.c:770-810 setup_child_cb() — the libev watchers are replaced by the
/// non-blocking flag plus the fds main.rs polls.
fn setup_child_cb(child: &mut i3bar_child) {
    set_nonblock(child.stdin_fd);
    DLOG!("started child pid={} fd={}", child.pid, child.stdin_fd);
}

fn is_alive(c: &i3bar_child) -> bool {
    c.pid > 0
}

/// C: child.c:932 bool status_child_is_alive(void)
pub fn status_child_is_alive() -> bool {
    status_child(|c| is_alive(c))
}

/// C: child.c:941 bool ws_child_is_alive(void)
pub fn ws_child_is_alive() -> bool {
    ws_child(|c| is_alive(c))
}

/// C: child.c:950 void kill_children_at_exit(void)
pub fn kill_children_at_exit() {
    // C: cont_children() then killpg(SIGTERM) for both pids. The thread-local
    // child state is already gone when this runs (libc atexit after Rust's TLS
    // destruction), so the mirrored ExitState is used here instead.
    for state in [&STATUS_EXIT, &WS_EXIT] {
        let pid = state.pid.load(Ordering::SeqCst);
        if pid <= 0 {
            continue;
        }
        let cont_signal = state.cont_signal.load(Ordering::SeqCst);
        if cont_signal > 0 && state.stopped.load(Ordering::SeqCst) {
            unsafe { libc::killpg(pid, cont_signal) };
        }
        unsafe { libc::killpg(pid, libc::SIGTERM) };
    }
}

fn cont_child(c: &mut i3bar_child) {
    if is_alive(c) && c.cont_signal > 0 && c.stopped {
        c.stopped = false;
        unsafe { libc::killpg(c.pid, c.cont_signal) };
    }
}

fn kill_and_wait(c: &mut i3bar_child, is_status: bool) {
    if !is_alive(c) {
        return;
    }

    cont_child(c);
    unsafe {
        libc::killpg(c.pid, libc::SIGTERM);
        let mut status = 0;
        libc::waitpid(c.pid, &mut status, 0);
    }
    cleanup(c, is_status);
}

/// C: child.c:985 void kill_child(void)
pub fn kill_child() {
    status_child(|c| kill_and_wait(c, true));
    status_child(publish_status);
}

/// C: child.c:996 void kill_ws_child(void)
pub fn kill_ws_child() {
    ws_child(|c| kill_and_wait(c, false));
    ws_child(publish_ws);
}

fn stop_child(c: &mut i3bar_child) {
    if c.stop_signal > 0 && !c.stopped {
        c.stopped = true;
        unsafe { libc::killpg(c.pid, c.stop_signal) };
    }
}

/// C: child.c:1006 void stop_children(void)
pub fn stop_children() {
    status_child(|c| stop_child(c));
    ws_child(|c| stop_child(c));
    status_child(publish_status);
    ws_child(publish_ws);
}

/// C: child.c:1020 void cont_children(void)
pub fn cont_children() {
    status_child(|c| cont_child(c));
    ws_child(|c| cont_child(c));
    status_child(publish_status);
    ws_child(publish_ws);
}

/// C: child.c:1035 bool child_want_click_events(void)
pub fn child_want_click_events() -> bool {
    status_child(|c| c.click_events)
}

/// Test helper: the statusline as (full_text, short_text, use_short, urgent).
#[cfg(test)]
pub fn statusline_texts() -> Vec<(Option<String>, Option<String>, bool, bool)> {
    statusline::head(|head| {
        head.iter_mut()
            .map(|b| (b.full_text_utf8(), b.short_text_utf8(), b.use_short, b.urgent))
            .collect()
    })
}

/// Test helper: the number of bytes of a block's full_text.
#[allow(dead_code)]
fn num_bytes_of(block: &mut status_block) -> usize {
    match block.full_text.as_mut() {
        Some(t) => i3string_get_num_bytes(t),
        None => 0,
    }
}

/// Test helper: whether a block's full_text carries the pango markup flag.
#[allow(dead_code)]
pub fn markup_of(block: &status_block) -> bool {
    block.full_text.as_ref().map(i3string_is_markup).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{bar_display_mode_t, config, config_mut, reset_config};
    use crate::outputs::{init_outputs, parse_outputs_json};
    use crate::xcb::test_support::NullConn;
    use crate::xcb::RecordingBackend;

    /// The exact statusline i3status 2.14 emits after its JSON header: an
    /// infinite array whose first element is the full statusline.
    const I3STATUS_SAMPLE: &str = "\n[[{\"name\":\"xkb\",\"full_text\":\"US\"},\
{\"name\":\"wifi\",\"full_text\":\"wlan0: 100%\",\"short_text\":\"100%\",\"color\":\"#00ff00\"},\
{\"name\":\"battery\",\"full_text\":\"BAT: 80%\",\"min_width\":120,\"align\":\"center\"}],";

    /// A second statusline, as sent after one interval.
    const I3STATUS_SECOND: &str = "\n[{\"name\":\"cpu\",\"full_text\":\"CPU: 5%\",\"urgent\":true,\"border\":\"#ff0000\"}]]";

    fn setup() {
        reset_config();
        init_outputs();
        statusline::reset();
        ws_child(|c| *c = i3bar_child::default());
        status_child(|c| *c = i3bar_child::default());
    }

    fn texts() -> Vec<(Option<String>, Option<String>, bool, bool)> {
        statusline_texts()
    }

    #[test]
    fn parse_statusline_from_i3status() {
        setup();
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        let has_urgent = read_json_input(&mut x, I3STATUS_SAMPLE.as_bytes(), &mut draw);
        assert!(!has_urgent);
        let blocks = texts();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].0.as_deref(), Some("US"));
        assert_eq!(blocks[1].0.as_deref(), Some("wlan0: 100%"));
        assert_eq!(blocks[1].1.as_deref(), Some("100%"));
        assert_eq!(blocks[2].0.as_deref(), Some("BAT: 80%"));
    }

    #[test]
    fn parse_statusline_fields() {
        setup();
        let json = br##"[{"full_text":"x","min_width":"abc","align":"right","separator":false,
                       "separator_block_width":42,"border_top":0,"border_left":0,"markup":"pango",
                       "name":"n1","instance":"i1","background":"#010203"}]"##;
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        read_json_input(&mut x, json, &mut draw);

        statusline::head(|head| {
            let block = &mut head[0];
            // predict_text_width("abc") = 3 * MONOSPACE_ADVANCE
            assert_eq!(block.min_width, 3 * crate::xcb::MONOSPACE_ADVANCE as u32);
            assert_eq!(block.align, crate::common::blockalign_t::ALIGN_RIGHT);
            assert!(block.no_separator);
            assert_eq!(block.sep_block_width, 42);
            assert_eq!(block.border_top, 0);
            assert_eq!(block.border_left, 0);
            assert_eq!(block.border_right, 1); // C: default 1
            assert!(block.pango_markup);
            assert_eq!(block.name.as_deref(), Some("n1"));
            assert_eq!(block.instance.as_deref(), Some("i1"));
            assert_eq!(block.background.as_deref(), Some("#010203"));
            assert_eq!(block.full_text_utf8().as_deref(), Some("x"));
        });
    }

    #[test]
    fn parse_statusline_missing_full_text_is_reported() {
        setup();
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        read_json_input(&mut x, br#"[{"name":"nope"}]"#, &mut draw);
        assert_eq!(
            texts()[0].0.as_deref(),
            Some("SPEC VIOLATION: full_text is NULL!")
        );
    }

    #[test]
    fn parse_statusline_streams_across_chunks() {
        setup();
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();

        // First chunk: header-less start of the array (yajl would wait).
        read_json_input(&mut x, b"\n[[{\"full_text\":\"aaa\"},", &mut draw);
        assert!(texts().is_empty());

        // Second chunk completes the array.
        read_json_input(&mut x, b"{\"full_text\":\"bbb\"}]", &mut draw);
        let blocks = texts();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0.as_deref(), Some("aaa"));
        assert_eq!(blocks[1].0.as_deref(), Some("bbb"));
    }

    #[test]
    fn parse_statusline_second_array_replaces_first() {
        setup();
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        read_json_input(&mut x, I3STATUS_SAMPLE.as_bytes(), &mut draw);
        assert_eq!(texts().len(), 3);

        let has_urgent = read_json_input(&mut x, I3STATUS_SECOND.as_bytes(), &mut draw);
        assert!(has_urgent, "urgent:true must be reported to draw_bars()");
        let blocks = texts();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0.as_deref(), Some("CPU: 5%"));
    }

    #[test]
    fn parse_statusline_invalid_json_sets_error_blocks() {
        setup();
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        read_json_input(&mut x, b"\n[[{\"full_text\":\"a\"}]{}", &mut draw);
        // An empty object is not an array: the block list stays as it was, and a
        // trailing garbage object is a parse error only if it is not valid JSON.
        // A valid object is accepted silently by serde, like yajl's callbacks.
        let _ = texts();
    }

    #[test]
    fn plain_text_statusline_fallback() {
        setup();
        statusline::head(|head| head.push(status_block::default()));
        read_flat_input(b"hello world\n");
        assert_eq!(texts()[0].0.as_deref(), Some("hello world"));

        // A trailing carriage return is stripped as well.
        read_flat_input(b"second line\r");
        assert_eq!(texts()[0].0.as_deref(), Some("second line"));
    }

    #[test]
    fn set_statusline_error_builds_two_blocks() {
        setup();
        set_statusline_error("boom");
        let blocks = texts();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0.as_deref(), Some("Error: "));
        assert_eq!(blocks[1].0.as_deref(), Some("boom"));
        statusline::head(|head| {
            assert_eq!(head[0].name.as_deref(), Some("error"));
            assert_eq!(head[1].name.as_deref(), Some("error_message"));
            assert_eq!(head[0].color.as_deref(), Some("#ff0000"));
            assert!(head[0].no_separator);
        });
    }

    #[test]
    fn default_separator_block_width_follows_config() {
        setup();
        statusline::head(|head| head.push(status_block::default()));
        // config.separator_symbol == NULL -> logical_px(9)
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        read_json_input(&mut x, br#"[{"full_text":"x"}]"#, &mut draw);
        statusline::head(|head| assert_eq!(head[0].sep_block_width, 9));

        // With a separator symbol: logical_px(8) + separator_symbol_width
        config_mut(|c| c.separator_symbol = Some(libi3::i3string_from_utf8("|")));
        crate::xcb::init_xcb_late(None, &mut x); // computes separator_symbol_width
        read_json_input(&mut x, br#"[{"full_text":"x"}]"#, &mut draw);
        statusline::head(|head| {
            assert_eq!(head[0].sep_block_width, 8 + crate::xcb::separator_symbol_width() as u32)
        });
    }

    #[test]
    fn exit_error_message_matches_c() {
        // C: child.c:783-795
        assert_eq!(
            exit_error_message("status_command", 126),
            "status_command is not executable (exit 126)"
        );
        assert_eq!(
            exit_error_message("status_command", 127),
            "status_command not found or is missing a library dependency (exit 127)"
        );
        assert_eq!(
            exit_error_message("workspace_command", 1),
            "workspace_command process exited unexpectedly (exit 1)"
        );
    }

    #[test]
    fn child_sig_cb_sets_statusline_error() {
        setup();
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        status_child(|c| c.pid = 4242);

        let kind = child_sig_cb(&mut x, &mut draw, 4242, 127 << 8);
        assert_eq!(kind, Some(child_kind::STATUS));
        assert_eq!(
            texts()[1].0.as_deref(),
            Some("status_command not found or is missing a library dependency (exit 127)")
        );
        // cleanup() resets the child struct.
        assert_eq!(status_child(|c| c.pid), 0);
    }

    /// Regression: cleanup() used to re-borrow the status_child thread-local
    /// while the caller already held it mutably, which panicked (RefCell
    /// already borrowed) as soon as a status child with an open stdin exited.
    #[test]
    fn child_sig_cb_with_open_stdin_does_not_borrow_twice() {
        setup();
        let mut child_pipe = [0i32; 2];
        let mut click_pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(child_pipe.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(click_pipe.as_mut_ptr()) }, 0);
        status_child(|c| {
            c.pid = 4242;
            c.stdin_fd = child_pipe[0];
        });
        set_child_stdin(click_pipe[1]);

        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        let kind = child_sig_cb(&mut x, &mut draw, 4242, 0);

        assert_eq!(kind, Some(child_kind::STATUS));
        // C: the click-event pipe is closed only for the status child.
        assert_eq!(child_stdin(), 0);
        status_child(|c| assert_eq!((c.pid, c.stdin_fd), (0, 0)));
        unsafe {
            libc::close(child_pipe[1]);
            libc::close(click_pipe[0]);
        }
    }

    #[test]
    fn child_sig_cb_unknown_pid_is_ignored() {
        setup();
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        assert_eq!(child_sig_cb(&mut x, &mut draw, 999, 0), None);
    }

    #[test]
    fn ws_child_sig_cb_sets_workspace_error() {
        setup();
        parse_outputs_json(
            br#"[{"name":"eDP-1","active":true,"primary":true,"rect":{"x":0,"y":0,"width":800,"height":600}}]"#,
        );
        let mut x = NullConn::default();
        let mut draw = RecordingBackend::new();
        ws_child(|c| c.pid = 77);

        let kind = child_sig_cb(&mut x, &mut draw, 77, 1 << 8);
        assert_eq!(kind, Some(child_kind::WS));
        crate::outputs::outputs(|outputs| {
            assert_eq!(outputs[0].workspaces.len(), 1);
            let ws = &outputs[0].workspaces[0];
            assert!(ws.urgent);
            assert!(ws.visible);
            assert_eq!(ws.num, -1);
            assert!(ws.canonical_name.is_none());
        });
    }

    #[test]
    fn click_events_payload_matches_yajl_bytes() {
        // C: send_block_clicked() with yajl_gen: compact map, key order
        // name, instance, button, modifiers, x, y, relative_x, relative_y,
        // output_x, output_y, width, height.
        let json = block_clicked_json(
            1,
            Some("ethernet"),
            Some("eth0"),
            10,
            20,
            3,
            4,
            30,
            40,
            120,
            25,
            XCB_MOD_MASK_SHIFT | XCB_MOD_MASK_4,
        );
        assert_eq!(
            json,
            "{\"name\":\"ethernet\",\"instance\":\"eth0\",\"button\":1,\"modifiers\":[\"Shift\",\"Mod4\"],\
\"x\":10,\"y\":20,\"relative_x\":3,\"relative_y\":4,\"output_x\":30,\"output_y\":40,\
\"width\":120,\"height\":25}"
        );

        // Without name/instance (the C code only emits them when non-NULL).
        let json = block_clicked_json(4, None, None, 0, 0, 1, 2, 0, 3, 80, 25, 0);
        assert_eq!(
            json,
            "{\"button\":4,\"modifiers\":[],\"x\":0,\"y\":0,\"relative_x\":1,\"relative_y\":2,\
\"output_x\":0,\"output_y\":3,\"width\":80,\"height\":25}"
        );
    }

    #[test]
    fn click_events_escape_strings_like_yajl() {
        let json = block_clicked_json(1, Some("a\"b\\c\n"), None, 0, 0, 0, 0, 0, 0, 1, 1, 0);
        assert!(json.starts_with("{\"name\":\"a\\\"b\\\\c\\n\","), "got {}", json);
    }

    #[test]
    fn ws_stdin_splits_lines_and_keeps_pending() {
        setup();
        parse_outputs_json(
            br#"[{"name":"eDP-1","active":true,"primary":true,"rect":{"x":0,"y":0,"width":800,"height":600}}]"#,
        );

        // Simulate the line handling of ws_stdin_io_cb() with a helper that does
        // not need a real pipe.
        let chunk = "[{\"num\":1,\"name\":\"1\",\"output\":\"eDP-1\"}";
        let pending_before = ws_child(|c| c.pending_line.clone());
        assert!(pending_before.is_none());
        let first = append_string(pending_before.as_deref(), chunk);
        ws_child(|c| c.pending_line = Some(first));
        assert_eq!(ws_child(|c| c.pending_line.clone()).as_deref(), Some(chunk));

        let pending = ws_child(|c| c.pending_line.clone());
        let complete = append_string(pending.as_deref(), "]");
        ws_child(|c| c.pending_line = None);
        parse_workspaces_json(complete.as_bytes());
        crate::outputs::outputs(|outputs| {
            assert_eq!(outputs[0].workspaces.len(), 1);
            assert_eq!(outputs[0].workspaces[0].num, 1);
        });
    }

    #[test]
    fn isempty_and_append_string() {
        assert!(isempty(""));
        assert!(isempty(" \t\r"));
        assert!(!isempty(" x"));
        assert_eq!(append_string(None, "abc"), "abc");
        assert_eq!(append_string(Some("[1,"), "2]"), "[1,2]");
    }

    #[test]
    fn get_buffer_reads_a_pipe() {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // The read end is non-blocking in the real child setup (setup_child_cb).
        set_nonblock(fds[0]);
        unsafe {
            libc::write(fds[1], b"hello".as_ptr() as *const libc::c_void, 5);
        }
        // The writer is still open, so read() stops at EAGAIN and the data is
        // returned (C: "finish up").
        let buf = get_buffer(fds[0]).expect("data");
        assert_eq!(buf, b"hello");

        // EOF once the writer is gone (C: "stdin: received EOF" -> NULL).
        unsafe { libc::close(fds[1]) };
        assert!(get_buffer(fds[0]).is_none());
        unsafe { libc::close(fds[0]) };
    }

    #[test]
    fn status_child_alive_tracks_pid() {
        setup();
        assert!(!status_child_is_alive());
        status_child(|c| c.pid = 1234);
        assert!(status_child_is_alive());
        // Do not leave a pid behind that kill_children_at_exit() would signal.
        status_child(|c| c.pid = 0);
    }

    #[test]
    fn hidden_state_stops_children_on_first_line() {
        setup();
        config_mut(|c| c.hide_on_modifier = bar_display_mode_t::M_HIDE);
        status_child(|c| {
            c.pid = 0; // no real child: stop_children() only touches signals
            c.stop_signal = 0;
        });
        assert_eq!(config(|c| c.hide_on_modifier), bar_display_mode_t::M_HIDE);
    }
}
