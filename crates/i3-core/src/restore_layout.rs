//! 1:1 port of src/restore_layout.c (353 lines) plus the restart plumbing of
//! src/util.c:223-320 (store_restart_layout / i3_restart) and the
//! process-level half of src/commands.c:1698 (cmd_restart), which the C code
//! keeps in commands.c because everything there is one process with globals.
//!
//! restore_layout.c is "everything for restored containers that is not pure
//! state parsing": the X11 placeholder windows that show the swallow criteria
//! of a restored container until the real window appears.
//!
//! Difference to C: src/restore_layout.c opens a *second* X connection for the
//! placeholders (restore_conn). That is impossible here — the port has exactly
//! one Rc<RustConnection> and a second connection deadlocks in grab_server
//! (see PROGRESS.md). The placeholder windows are therefore created on the one
//! shared connection: i3 manages them like any other client window and the
//! temporary id-swallow match (restore_layout.c:231-235) makes them land in
//! the restored container, which is exactly the mechanism C relies on.

use std::cell::RefCell;
use std::rc::Rc;

use crate::xcb_util::{connect as xcb_connect, SharedConnection};

use crate::backend::{ConBackend, XConn, PropMode, XCB_CW_BACK_PIXEL, XCB_CW_EVENT_MASK};
use crate::con::{con_is_leaf, ConId};
use crate::data::{
    ConType, Match, MatchDock, MatchUrgency, MatchWindowMode, Rect, XCB_WINDOW_NONE,
};
use crate::tree::Tree;
use crate::xcb_util::{Atoms, RustXConn};

/// C: restore_layout.c:20-33 placeholder_state
#[derive(Debug, Clone, Copy)]
pub struct PlaceholderState {
    /// The X11 placeholder window.
    pub window: u32,
    /// The container to which this placeholder window belongs.
    pub con: ConId,
    /// Current size of the placeholder window (to detect size changes).
    pub rect: Rect,
}

/// The C file-statics (restore_layout.c:35-38): the TAILQ of placeholder
/// states and the (second) X connection.
struct RestoreState {
    /// C: static xcb_connection_t *restore_conn — here the shared connection.
    conn: Option<SharedConnection>,
    screen_idx: usize,
    atoms: Option<Atoms>,
    /// C: static TAILQ_HEAD(state_head, placeholder_state) state_head
    states: Vec<PlaceholderState>,
}

impl RestoreState {
    const fn new() -> RestoreState {
        RestoreState {
            conn: None,
            screen_idx: 0,
            atoms: None,
            states: Vec::new(),
        }
    }
}

thread_local! {
    /// C: the file-scope statics of restore_layout.c (the window manager is
    /// single-threaded in C, and the port's event loop is too).
    static RESTORE: RefCell<RestoreState> = const { RefCell::new(RestoreState::new()) };
}

/// C: src/restore_layout.c:85 restore_connect
///
/// Opens a separate connection to X11 for placeholder windows when restoring
/// layouts. This is done as a safety measure (users can xkill a placeholder
/// window without killing their window manager) and for better isolation, both
/// on the wire to X11 and thus also in the code.
///
/// The connection is opened lazily and only outside of the WM's grab_server
/// phases (placeholders are created during the tree restore before
/// manage_existing_windows(), and from cmd_append_layout while the event loop
/// is idle), which is why the single-connection deadlock noted in PROGRESS.md
/// does not apply here.
pub fn restore_connect(_main_conn: SharedConnection, _screen_idx: usize, atoms: Atoms) {
    RESTORE.with(|r| {
        let mut r = r.borrow_mut();
        if r.conn.is_some() {
            /* C: this is a reconnect (the X connection was killed); drop the
             * stale placeholder bookkeeping. */
            r.states.clear();
            r.conn = None;
        }
        /* C: restore_conn = xcb_connect(NULL, &screen); */
        match xcb_connect() {
            Ok((conn, screen)) => {
                r.conn = Some(conn);
                r.screen_idx = screen;
            }
            Err(e) => {
                /* C: errx(EXIT_FAILURE, "Cannot open display") — the WM keeps
                 * running without placeholder windows instead of dying. */
                crate::log::errorlog(&format!("Cannot open display for placeholder windows: {}
", e));
            }
        }
        r.atoms = Some(atoms);
    });
}

/// C: src/restore_layout.c:129 update_placeholder_contents
///
/// C draws the serialized swallow criteria and the watch symbol with
/// cairo/pango. Drawing is still stubbed in this port (x.rs NullDraw, see
/// PROGRESS.md "描画"), so only the serialization itself is kept — it is the
/// part that defines what a placeholder window represents.
pub fn placeholder_criteria(t: &Tree, con: ConId) -> Vec<String> {
    let mut out = Vec::new();
    for swallows in &t.cons[con].swallow_head {
        let mut serialized: Option<String> = None;
        macro_rules! append_regex {
            ($field:ident, $name:expr) => {
                if let Some(re) = &swallows.$field {
                    let old = serialized.take();
                    serialized = Some(format!(
                        "{}{}{}=\"{}\"",
                        old.clone().unwrap_or_else(|| "[".to_string()),
                        if old.is_some() { " " } else { "" },
                        $name,
                        re.pattern
                    ));
                }
            };
        }
        append_regex!(class, "class");
        append_regex!(instance, "instance");
        append_regex!(window_role, "window_role");
        append_regex!(title, "title");
        append_regex!(machine, "machine");

        match serialized {
            // C: DLOG("This swallows specification is not serializable?!\n")
            None => continue,
            Some(s) => out.push(format!("{}]", s)),
        }
    }
    out
}

/// C: src/restore_layout.c:188 open_placeholder_window
fn open_placeholder_window(t: &mut Tree, con: ConId) {
    let eligible = con_is_leaf(t, con)
        && t.cons[con].window.is_none()
        && !t.cons[con].swallow_head.is_empty()
        && t.cons[con].r#type == ConType::CT_CON;

    if eligible {
        let rect = t.cons[con].rect;
        let name = t.cons[con].name.clone().unwrap_or_default();

        let (window, atoms) = RESTORE.with(|r| {
            let r = r.borrow();
            let conn = match &r.conn {
                Some(c) => Rc::clone(c),
                None => return (XCB_WINDOW_NONE, None),
            };
            let mut x = RustXConn::wrap(Rc::clone(&conn), r.screen_idx);
            let root = x.root_screen().root;
            let window = x.generate_id();
            if window == XCB_WINDOW_NONE {
                return (XCB_WINDOW_NONE, None);
            }
            /* C: libi3 create_window(restore_conn, con->rect,
             *     XCB_COPY_FROM_PARENT, XCB_COPY_FROM_PARENT,
             *     XCB_WINDOW_CLASS_INPUT_OUTPUT, XCURSOR_CURSOR_POINTER, true,
             *     XCB_CW_BACK_PIXEL | XCB_CW_EVENT_MASK, ...)
             *
             * (width/height 0 would be an invalid X11 request, which is why
             * load_layout.c:154-164 fills in a size for floating containers.) */
            x.create_window(
                0, // C: XCB_COPY_FROM_PARENT
                window,
                root,
                rect.x as i16,
                rect.y as i16,
                rect.width.max(1) as u16,
                rect.height.max(1) as u16,
                0,
                1, // XCB_WINDOW_CLASS_INPUT_OUTPUT
                0, // C: XCB_COPY_FROM_PARENT
                &[
                    (XCB_CW_BACK_PIXEL, 0),
                    (
                        XCB_CW_EVENT_MASK,
                        /* XCB_EVENT_MASK_EXPOSURE | XCB_EVENT_MASK_STRUCTURE_NOTIFY */
                        (1 << 15) | (1 << 17),
                    ),
                ],
            );
            /* C: restore_layout.c:204-210 — "Make i3 not focus this window":
             * WM_HINTS with input = 0 (xcb_icccm_wm_hints_set_none +
             * xcb_icccm_wm_hints_set_input(hints, 0)). window_update_hints()
             * turns that into doesnt_accept_focus, so manage_window() does not
             * con_activate() the placeholder when it gets mapped (otherwise the
             * placeholder steals the focus and new windows are opened *inside*
             * it instead of next to it). */
            let hints: [u32; 9] = [
                1, /* flags: XCB_ICCCM_WM_HINT_INPUT */
                0, /* input: False */
                0, 0, 0, 0, 0, 0, 0,
            ];
            let mut hint_bytes = Vec::with_capacity(hints.len() * 4);
            for word in hints {
                hint_bytes.extend_from_slice(&word.to_ne_bytes());
            }
            x.change_property(
                window,
                crate::data::XCB_ATOM_WM_HINTS,
                crate::data::XCB_ATOM_WM_HINTS,
                32,
                PropMode::REPLACE,
                &hint_bytes,
            );
            /* C: set the same name as was stored in the layout file. While
             * perhaps slightly confusing in the first instant, this brings
             * additional clarity to which placeholder is waiting for which
             * actual window. */
            if let Some(a) = &r.atoms {
                if !name.is_empty() {
                    x.change_property(
                        window,
                        a.A__NET_WM_NAME,
                        a.A_UTF8_STRING,
                        8,
                        PropMode::REPLACE,
                        name.as_bytes(),
                    );
                }
            }
            x.map_window(window);
            x.flush();
            (window, r.atoms.clone())
        });

        if window == XCB_WINDOW_NONE || atoms.is_none() {
            crate::log::debuglog("Could not create a placeholder window (no X connection)\n");
            return;
        }

        crate::log::debuglog(&format!(
            "Created placeholder window {:#010x} for leaf container {:?} / {}\n",
            window, con, name
        ));

        RESTORE.with(|r| {
            r.borrow_mut().states.push(PlaceholderState { window, con, rect });
        });

        /* create temporary id swallow to match the placeholder
         * (C: restore_layout.c:231-235) */
        let mut temp_id = match_init();
        temp_id.dock = MatchDock::M_DONTCHECK;
        temp_id.id = window;
        t.cons[con].swallow_head.push_front(temp_id);
    }

    let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in children {
        open_placeholder_window(t, child);
    }
    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for child in floating {
        open_placeholder_window(t, child);
    }
}

/// C: src/restore_layout.c:254 restore_open_placeholder_windows
///
/// Open placeholder windows for all children of parent. The placeholder window
/// will vanish as soon as a real window is swallowed by the container. Until
/// then, it exposes the criteria that must be fulfilled for a window to be
/// swallowed by this container.
pub fn restore_open_placeholder_windows(t: &mut Tree, parent: ConId) {
    let children: Vec<ConId> = t.cons[parent].nodes_head.iter().copied().collect();
    for child in children {
        open_placeholder_window(t, child);
    }
    let floating: Vec<ConId> = t.cons[parent].floating_head.iter().copied().collect();
    for child in floating {
        open_placeholder_window(t, child);
    }

    RESTORE.with(|r| {
        if let Some(conn) = &r.borrow().conn {
            RustXConn::wrap(Rc::clone(conn), r.borrow().screen_idx).flush();
        }
    });
}

/// C: src/restore_layout.c:273 restore_kill_placeholder
///
/// Kill the placeholder window, if placeholder refers to a placeholder window.
/// This function is called when manage.c puts a window into an existing
/// container (C: src/manage.c:380 / :725).
pub fn restore_kill_placeholder(placeholder: u32) -> bool {
    let found = RESTORE.with(|r| {
        let mut r = r.borrow_mut();
        let idx = match r.states.iter().position(|s| s.window == placeholder) {
            Some(idx) => idx,
            None => return false,
        };
        if let Some(conn) = &r.conn {
            let mut x = RustXConn::wrap(Rc::clone(conn), r.screen_idx);
            x.destroy_window(placeholder);
            x.flush();
        }
        r.states.remove(idx);
        true
    });
    if found {
        crate::log::debuglog(&format!(
            "placeholder window {:#010x} destroyed.\n",
            placeholder
        ));
    } else {
        crate::log::debuglog(&format!(
            "{:#010x} is not a placeholder window, ignoring.\n",
            placeholder
        ));
    }
    found
}

/// Not in C: the port needs an entry point that cleans up placeholders whose
/// container no longer holds the placeholder window (manage.rs's swallow path
/// replaces con->window without calling restore_kill_placeholder, which is
/// manage.c:380/725 in C). Called once per event loop iteration from
/// crates/bins/i3/src/server.rs; C does the same work inside
/// restore_kill_placeholder.
pub fn prune_placeholders(t: &Tree) -> usize {
    let stale: Vec<u32> = RESTORE.with(|r| {
        r.borrow()
            .states
            .iter()
            .filter(|s| match t.cons.get(s.con) {
                // Only when the container already holds a *different* window
                // (i.e. the real client replaced the placeholder) — while the
                // container has no window yet, the placeholder is still doing
                // its job.
                Some(con) => match con.window.map(|w| t.windows[w].id) {
                    Some(w) => w != s.window,
                    None => false,
                },
                None => true,
            })
            .map(|s| s.window)
            .collect()
    });
    let mut n = 0;
    for window in stale {
        if restore_kill_placeholder(window) {
            n += 1;
        }
    }
    n
}

/// C: src/restore_layout.c:292 expose_event — C redraws the placeholder
/// contents (draw_util_text). Drawing is stubbed in this port and the contents
/// never change, so nothing is left to do.
pub fn restore_expose(_window: u32) {}

/// C: src/restore_layout.c:315 configure_notify — updates the cached rect of
/// the placeholder state (the surface resize is part of the drawing code that
/// is not ported yet).
pub fn restore_configure_notify(window: u32, width: u16, height: u16) {
    RESTORE.with(|r| {
        let mut r = r.borrow_mut();
        for state in r.states.iter_mut() {
            if state.window != window {
                continue;
            }
            state.rect.width = width as u32;
            state.rect.height = height as u32;
            return;
        }
    });
}

/// C: src/match.c:26 match_init — kept local so this module does not depend on
/// commands.rs' private copy.
fn match_init() -> Match {
    Match {
        urgent: MatchUrgency::U_DONTCHECK,
        window_mode: MatchWindowMode::WM_ANY,
        /* we use this as the placeholder value for "not set". */
        window_type: u32::MAX,
        id: XCB_WINDOW_NONE,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// In-place restart (C: src/util.c:223 store_restart_layout, :281 i3_restart,
// src/commands.c:1698 cmd_restart)
// ---------------------------------------------------------------------------

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// C: src/commands.c:1698 — cmd_restart is synchronous in C (it execs before
/// returning); the ported command layer cannot exec from inside the handler
/// (the IPC server, the X connection and the event loop belong to
/// crates/bins/i3), so the handler leaves a request behind:
///   * the fd to carry across the restart, in _I3_RESTART_FD (set by
///     cmd_restart, exactly like C),
///   * this flag, which crates/bins/i3/src/server.rs turns into
///     ipc_shutdown + store_restart_layout + execvp at the end of the current
///     event loop iteration.
static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);
/// C: cmd_restart's exempt_fd — the IPC client that survives the restart.
static RESTART_EXEMPT_FD: AtomicI32 = AtomicI32::new(-1);

/// C: src/commands.c:1700-1712 — carry the IPC client fd across the restart.
///
/// C clears FD_CLOEXEC on the client fd itself. The port duplicates the fd
/// first: ipc_shutdown() frees every IpcClient and dropping the stream closes
/// the original descriptor, so the duplicate (which execvp inherits, dup()
/// clears FD_CLOEXEC by definition) is what the new process adopts.
pub fn cmd_restart_prepare(client_fd: Option<i32>) {
    if let Some(fd) = client_fd {
        crate::log::log_(&format!("Carrying file descriptor {} across restart\n", fd));
        let dup_fd = unsafe { libc::dup(fd) };
        if dup_fd < 0 {
            crate::log::errorlog(&format!("Could not disable FD_CLOEXEC on fd {}\n", fd));
        } else {
            RESTART_EXEMPT_FD.store(dup_fd, Ordering::SeqCst);
            std::env::set_var("_I3_RESTART_FD", dup_fd.to_string());
        }
    }
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
}

/// C: main.c:263 parse_restart_fd — the fd handed down by the previous
/// process, or -1.
pub fn parse_restart_fd() -> Option<i32> {
    let restart_fd = match std::env::var("_I3_RESTART_FD") {
        Ok(v) => v,
        Err(_) => return None,
    };
    match restart_fd.trim().parse::<i32>() {
        Ok(fd) => Some(fd),
        Err(_) => {
            crate::log::errorlog(&format!("Malformed _I3_RESTART_FD \"{}\"\n", restart_fd));
            None
        }
    }
}

/// True while a restart was requested by cmd_restart.
pub fn restart_pending() -> bool {
    RESTART_REQUESTED.load(Ordering::SeqCst)
}

/// Take the restart request (clearing it). Returns the exempt fd, if any.
pub fn take_restart_request() -> Option<Option<i32>> {
    if !RESTART_REQUESTED.swap(false, Ordering::SeqCst) {
        return None;
    }
    let fd = RESTART_EXEMPT_FD.swap(-1, Ordering::SeqCst);
    Some(if fd < 0 { None } else { Some(fd) })
}

/// C: src/util.c:231-271 store_restart_layout — dump the tree and write it to
/// the restart state file. Returns the filename (None when the layout could
/// not be stored, i.e. when C returns NULL and the restart loses the layout).
pub fn store_restart_layout(t: &Tree, restart_state_path: Option<&str>) -> Option<String> {
    let croot = match t.croot {
        Some(croot) => croot,
        None => return None,
    };

    let mut gen = crate::dump::JsonGen::new();
    crate::dump::dump_node(t, &mut gen, croot, true);
    let payload = gen.get_buf().to_string();

    /* create a temporary file if one hasn't been specified, or just resolve
     * the tildes in the specified path */
    let filename = match restart_state_path {
        None => match libi3::get_process_filename("restart-state") {
            Some(f) => f,
            None => return None,
        },
        Some(path) => resolve_tilde(path),
    };

    /* create the directory, it could have been cleaned up before restarting or
     * may not exist at all in case it was user-specified. */
    let base = match std::path::Path::new(&filename).parent() {
        Some(base) => base.to_path_buf(),
        None => std::path::PathBuf::from("/"),
    };
    crate::log::debuglog(&format!(
        "Creating \"{}\" for storing the restart layout\n",
        base.display()
    ));
    if std::fs::create_dir_all(&base).is_err() {
        crate::log::errorlog(&format!(
            "Could not create \"{}\" for storing the restart layout, layout will be lost.\n",
            base.display()
        ));
    }

    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&filename)
    {
        Ok(f) => f,
        Err(e) => {
            crate::log::errorlog(&format!("open(): {}\n", e));
            return None;
        }
    };
    if let Err(e) = file.write_all(payload.as_bytes()) {
        crate::log::errorlog(&format!(
            "Could not write restart layout to \"{}\", layout will be lost: {}\n",
            filename, e
        ));
        return None;
    }
    drop(file);

    if !payload.is_empty() {
        crate::log::debuglog(&format!("layout: {}\n", payload));
    }

    Some(filename)
}

/// C: libi3/resolve_tilde.c resolve_tilde — replace ~/ with $HOME.
fn resolve_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            let mut resolved = home;
            if !resolved.ends_with('/') {
                resolved.push('/');
            }
            resolved.push_str(rest);
            return resolved;
        }
    }
    path.to_string()
}

/// C: src/util.c:281 i3_restart
///
/// Restart i3 in-place: appends -a to the argument list to disable autostart
/// and replaces -r <file> so that the layout is restored, then execvp()s a
/// fresh i3. Never returns on success.
pub fn i3_restart(argv: &[OsString], restart_filename: Option<&str>) -> Result<(), String> {
    // C: start_argv is the original argv; add_argument() appends (or
    // replaces) the options below.
    let mut args: Vec<OsString> = argv.to_vec();

    /* make sure -a is in the argument list or add it */
    args.push(OsString::from("-a"));

    /* make debuglog-on persist */
    if crate::log::get_debug_logging() {
        args.push(OsString::from("-d"));
        args.push(OsString::from("all"));
    }

    /* replace -r <file> so that the layout is restored */
    if let Some(filename) = restart_filename {
        args.push(OsString::from("--restart"));
        args.push(OsString::from(filename));
    }

    let c_args: Vec<CString> = args
        .iter()
        .map(|a| {
            CString::new(a.as_os_str().as_bytes())
                .unwrap_or_else(|_| CString::new(a.to_string_lossy().as_bytes()).unwrap())
        })
        .collect();
    let mut argv_ptrs: Vec<*const libc::c_char> =
        c_args.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    let argv0 = args.first().cloned().unwrap_or_else(|| OsString::from("i3"));
    crate::log::log_(&format!("restarting \"{}\"...\n", argv0.to_string_lossy()));
    unsafe {
        libc::execvp(argv_ptrs[0], argv_ptrs.as_ptr());
    }
    let err = std::io::Error::last_os_error();
    Err(format!("Could not exec i3: {}", err))
}

/// C: src/tree.c:66 tree_restore — load the tree from the file that the
/// previous process dumped (--restart <path>).
///
/// The sequence is the C one (create a throw-away root, tree_append_json
/// below it, then use its first child as the new croot and make sure the
/// pseudo-output __i3 is first). It lives here rather than being called
/// through tree.rs's tree_restore because a ConBackend does not implement
/// slurp()/tree_append_json() yet (backend.rs / xbackend.rs are outside this
/// task's files); the layout is loaded through load_layout directly.
pub fn tree_restore_from_file(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    path: &str,
    geometry: Rect,
) -> bool {
    let globbed = resolve_tilde(path);
    if !std::path::Path::new(&globbed).exists() {
        crate::log::log_(&format!("{} does not exist, not restoring tree\n", globbed));
        return false;
    }

    let buf = match crate::load_layout::slurp(&globbed) {
        Some(buf) => buf,
        // slurp already logged an error.
        None => return false,
    };

    /* TODO: refactor the following (C comment) */
    let croot = crate::con::con_new(t, b, None, None);
    t.cons[croot].rect = geometry;
    t.focused = Some(croot);
    t.croot = Some(croot);

    let _err = crate::load_layout::tree_append_json(t, b, croot, &buf);

    crate::log::debuglog("appended tree, using new root\n");
    let new_root = match t.cons[croot].nodes_head.front().copied() {
        Some(r) => r,
        None => {
            /* tree_append_json failed. Continuing here would segfault. */
            return false;
        }
    };
    t.croot = Some(new_root);
    let out = match t.cons[new_root].nodes_head.front().copied() {
        Some(out) => out,
        None => return false,
    };

    /* For in-place restarting into v4.2, we need to make sure the new
     * pseudo-output __i3 is present. */
    if t.cons[out].name.as_deref() != Some("__i3") {
        crate::log::debuglog("Adding pseudo-output __i3 during inplace restart\n");
        let __i3 = create_pseudo_output___i3(t, b);
        /* Ensure that it is the first output, other places in the code make
         * that assumption. */
        let parent = t.cons[__i3]
            .parent
            .expect("tree_restore: __i3 has no parent");
        t.nodes_remove(parent, __i3);
        t.nodes_insert_head(parent, __i3);
    }

    /* C: src/tree.c:110 restore_open_placeholder_windows(croot) — called here
     * at the end of tree_restore. The port calls it later, from
     * crates/bins/i3/src/server.rs right after manage_existing_windows():
     * C's placeholders live on a second X connection, so the MapRequests for
     * them and for the old clients are serviced in an order that lets the old
     * client win the swallow match. With one connection the placeholders are
     * created first and would take the container over (the id swallow of the
     * placeholder is at the head of swallow_head), replacing the real client.
     * Creating them after the existing clients are managed gives the same end
     * state as C: the restored containers hold the real windows again. */
    let _ = new_root;
    true
}

/// C: src/tree.c:22 _create___i3 (static in C, reachable here only through
/// tree.c's tree_init) — the pseudo-output __i3 that holds the
/// output-independent workspaces such as __i3_scratch.
fn create_pseudo_output___i3(t: &mut Tree, b: &mut dyn ConBackend) -> ConId {
    let __i3 = crate::con::con_new(t, b, None, None);
    t.cons[__i3].name = Some("__i3".to_string());
    t.cons[__i3].r#type = ConType::CT_OUTPUT;
    t.cons[__i3].layout = crate::data::Layout::L_OUTPUT;
    crate::con::con_fix_percent(t, t.croot.expect("create___i3: no croot"));
    b.x_set_name(t, __i3, "[i3 con] pseudo-output __i3");
    /* For retaining the correct position/size of a scratchpad window, the
     * dimensions of the real outputs should be multiples of the __i3
     * pseudo-output. Ensuring that is the job of scratchpad_fix_resolution()
     * which gets called after this function and after detecting all the
     * outputs (or whenever an output changes). */
    t.cons[__i3].rect.width = 1280;
    t.cons[__i3].rect.height = 1024;

    /* Add a content container. */
    let content = crate::con::con_new(t, b, None, None);
    t.cons[content].r#type = ConType::CT_CON;
    t.cons[content].name = Some("content".to_string());
    t.cons[content].layout = crate::data::Layout::L_SPLITH;
    b.x_set_name(t, content, "[i3 con] content __i3");
    crate::con::con_attach(t, b, content, __i3, false);

    /* Attach the __i3_scratch workspace. */
    let ws = crate::con::con_new(t, b, None, None);
    t.cons[ws].r#type = ConType::CT_WORKSPACE;
    t.cons[ws].num = -1;
    t.cons[ws].name = Some("__i3_scratch".to_string());
    t.cons[ws].layout = crate::data::Layout::L_SPLITH;
    crate::con::con_attach(t, b, ws, content, false);
    b.x_set_name(t, ws, "[i3 con] workspace __i3_scratch");
    t.cons[ws].fullscreen_mode = crate::data::FullscreenMode::CF_OUTPUT;

    __i3
}

/// C: src/commands.c:1698 cmd_restart — the process-level half that follows the
/// IPC layer (C: ipc_shutdown, unlink of the sockets, purge_zerobyte_logfile,
/// i3_restart). Called from crates/bins/i3/src/server.rs once the command has
/// been parsed.
pub fn perform_restart(
    t: &Tree,
    argv: &[OsString],
    restart_state_path: Option<&str>,
    forget_layout: bool,
) -> Result<(), String> {
    let restart_filename = if forget_layout {
        None
    } else {
        store_restart_layout(t, restart_state_path)
    };
    i3_restart(argv, restart_filename.as_deref())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::con::con_new;
    use crate::data::{Layout, Rect};
    use crate::tree::{tree_init, Tree};

    /// A tree with root -> output -> content -> workspace -> two cons and one
    /// swallow criterion on the first leaf.
    fn sample_tree() -> (Tree, NullBackend, ConId) {
        let mut t = Tree::new();
        let mut b = NullBackend;
        tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));
        let croot = t.croot.unwrap();
        let output = con_new(&mut t, &mut b, Some(croot), None);
        t.cons[output].name = Some("X11-1".to_string());
        t.cons[output].r#type = ConType::CT_OUTPUT;
        t.cons[output].layout = Layout::L_OUTPUT;
        let content = con_new(&mut t, &mut b, Some(output), None);
        t.cons[content].name = Some("content".to_string());
        t.cons[content].layout = Layout::L_SPLITH;
        let ws = con_new(&mut t, &mut b, Some(content), None);
        t.cons[ws].name = Some("1".to_string());
        t.cons[ws].num = 1;
        t.cons[ws].r#type = ConType::CT_WORKSPACE;
        t.cons[ws].layout = Layout::L_SPLITH;
        let leaf = con_new(&mut t, &mut b, Some(ws), None);
        t.cons[leaf].name = Some("xterm".to_string());
        t.cons[leaf].layout = Layout::L_SPLITH;
        t.cons[leaf].rect = Rect::new(0, 0, 1280, 800);
        let mut m = match_init();
        m.dock = MatchDock::M_DONTCHECK;
        m.class = Some(Box::new(crate::data::regex::new("^XTerm$")));
        t.cons[leaf].swallow_head.push_back(m);
        crate::con::con_fix_percent(&mut t, ws);
        (t, b, leaf)
    }

    /// C: src/restore_layout.c:129-186 — the placeholder text is the
    /// serialization of the swallow criteria.
    #[test]
    fn placeholder_criteria_serializes_swallows() {
        let (t, _b, leaf) = sample_tree();
        let lines = placeholder_criteria(&t, leaf);
        assert_eq!(lines, vec!["[class=\"^XTerm$\"]".to_string()]);

        // multiple criteria are joined with a space, in APPEND_REGEX order
        let (mut t2, _b2, leaf2) = sample_tree();
        let mut m = match_init();
        m.class = Some(Box::new(crate::data::regex::new("^XTerm$")));
        m.instance = Some(Box::new(crate::data::regex::new("^xterm$")));
        m.title = Some(Box::new(crate::data::regex::new("hello")));
        t2.cons[leaf2].swallow_head.push_back(m);
        let lines = placeholder_criteria(&t2, leaf2);
        assert_eq!(
            lines[1],
            "[class=\"^XTerm$\" instance=\"^xterm$\" title=\"hello\"]".to_string()
        );
    }

    /// C: src/restore_layout.c:273-290 — restore_kill_placeholder only knows
    /// the windows it created.
    #[test]
    fn kill_unknown_placeholder_is_false() {
        assert!(!restore_kill_placeholder(0xdead_beef));
    }

    /// C: src/restore_layout.c:254 — without an X connection no placeholder
    /// window is created (and nothing crashes).
    #[test]
    fn placeholder_without_connection_is_a_noop() {
        let (mut t, _b, leaf) = sample_tree();
        let before = t.cons[leaf].swallow_head.len();
        let ws = t.cons[leaf].parent.unwrap();
        restore_open_placeholder_windows(&mut t, ws);
        // only the criteria from the layout file, no temporary id swallow
        assert_eq!(t.cons[leaf].swallow_head.len(), before);
        assert!(t.cons[leaf].window.is_none());
    }

    /// C: src/util.c:223 store_restart_layout + src/tree.c:66 tree_restore —
    /// the layout survives a write/read round trip.
    #[test]
    fn store_and_restore_restart_layout() {
        let (t, _b, _leaf) = sample_tree();
        let dir = std::env::temp_dir().join(format!("i3-restart-test-{}", std::process::id()));
        let path = dir.join("layout.json");
        let filename =
            store_restart_layout(&t, Some(path.to_str().unwrap())).expect("layout stored");
        assert_eq!(filename, path.to_str().unwrap());
        let written = std::fs::read_to_string(&path).expect("layout file");
        assert!(written.contains("\"swallows\""));
        assert!(written.contains("\"name\""));

        // restore it into a fresh tree
        let mut t2 = Tree::new();
        let mut b2 = NullBackend;
        let restored =
            tree_restore_from_file(&mut t2, &mut b2, path.to_str().unwrap(), Rect::new(0, 0, 1280, 800));
        assert!(restored);
        let croot = t2.croot.expect("croot restored");
        // root -> __i3 / X11-1 -> content -> workspace -> leaf
        let output = t2.cons[croot]
            .nodes_head
            .iter()
            .copied()
            .find(|&c| t2.cons[c].name.as_deref() == Some("X11-1"))
            .expect("output X11-1");
        let content = t2.cons[output].nodes_head.front().copied().expect("content");
        let ws = t2.cons[content].nodes_head.front().copied().expect("workspace");
        assert_eq!(t2.cons[ws].name.as_deref(), Some("1"));
        let leaf = t2.cons[ws].nodes_head.front().copied().expect("leaf");
        assert_eq!(t2.cons[leaf].name.as_deref(), Some("xterm"));
        assert_eq!(t2.cons[leaf].swallow_head.len(), 1);
        assert_eq!(
            t2.cons[leaf].swallow_head[0]
                .class
                .as_ref()
                .unwrap()
                .pattern,
            "^XTerm$"
        );
        // the ported __i3 pseudo-output survives and stays the first output
        let outputs: Vec<Option<String>> = t2.cons[croot]
            .nodes_head
            .iter()
            .map(|&c| t2.cons[c].name.clone())
            .collect();
        assert_eq!(outputs.first().and_then(|n| n.clone()).as_deref(), Some("__i3"));
        assert!(outputs.iter().any(|n| n.as_deref() == Some("X11-1")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C: src/tree.c:66 tree_restore — a missing file is not an error, it just
    /// means "no tree to restore".
    #[test]
    fn restore_missing_file_returns_false() {
        let mut t = Tree::new();
        let mut b = NullBackend;
        assert!(!tree_restore_from_file(
            &mut t,
            &mut b,
            "/nonexistent/i3-layout.json",
            Rect::new(0, 0, 10, 10)
        ));
    }

    /// C: src/main.c:263 parse_restart_fd — the env var is the IPC fd.
    #[test]
    fn parse_restart_fd_reads_the_env() {
        let old = std::env::var("_I3_RESTART_FD").ok();
        std::env::set_var("_I3_RESTART_FD", "42");
        assert_eq!(parse_restart_fd(), Some(42));
        std::env::set_var("_I3_RESTART_FD", "notanumber");
        assert_eq!(parse_restart_fd(), None);
        match old {
            Some(v) => std::env::set_var("_I3_RESTART_FD", v),
            None => std::env::remove_var("_I3_RESTART_FD"),
        }
    }

    /// C: src/commands.c:1700-1712 — cmd_restart hands the client fd down in
    /// _I3_RESTART_FD and leaves a restart request behind.
    #[test]
    fn restart_request_carries_the_fd() {
        // the previous test may have left the flag set; clear it
        let _ = take_restart_request();
        assert!(!restart_pending());
        cmd_restart_prepare(None);
        assert!(restart_pending());
        assert_eq!(take_restart_request(), Some(None));
        assert!(!restart_pending());
        std::env::remove_var("_I3_RESTART_FD");
    }
}

