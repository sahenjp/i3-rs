//! The i3 startup sequence after the X connection is open and the main event
//! loop — the part of C: src/main.c that runs from line 597 to the end of the
//! file (the ev_* watchers, the grab_server/manage_existing_windows handshake
//! and ev_loop()).
//!
//! C: src/main.c:585-1231

#![allow(dead_code)]

/// I3_TRACE_LOOP=1 progress marks (development aid; the C code has no
/// counterpart — it prints one line per startup stage and loop iteration).
macro_rules! mark {
    ($m:expr) => {
        if std::env::var_os("I3_TRACE_LOOP").is_some() {
            eprintln!("TRACE: {}", $m);
        }
    };
}

use std::process::ExitCode;

use i3_core::config::{config_load_t, load_configuration};
use i3_core::config_directives::ConfigDirectives;
use i3_core::data::Rect;
use i3_core::ewmh::ewmh_setup_hints;
use i3_core::ipc::IpcServer;
use i3_core::randr::{randr_init, RandrState};
use i3_core::tree::tree_init;
use i3_core::xcb_util::{Atoms, RustXConn};
use i3_core::ConBackend;

use crate::I3Globals;

/// C: src/main.c:711 xcb_atom_name_by_screen("WM", conn_screen)
fn atom_name_by_screen(prefix: &str, screen: usize) -> Option<String> {
    // libxcb-util formats "%s_S%u" (so "WM" + 0 becomes "WM_S0").
    Some(format!("{}_S{}", prefix, screen))
}

/// C: src/main.c:707-781 — Acquire the WM_Sn selection. The order (intern →
/// get_selection_owner → create 1x1 window → set_selection_owner → wait for
/// the old WM to disappear, 15s → announce) is required for a clean takeover.
fn acquire_wm_sn(
    conn: &impl x11rb::connection::Connection,
    g: &mut I3Globals,
) -> Result<(), String> {
    use x11rb::protocol::xproto::ConnectionExt as _;

    let atom_name =
        atom_name_by_screen("WM", g.conn_screen).ok_or_else(|| "atom_name failed".to_string())?;

    // C: wm_sn_selection_owner = xcb_generate_id(conn);
    g.wm_sn_selection_owner = conn.generate_id().map_err(|e| e.to_string())?;

    // C: xcb_intern_atom("WM_S0")
    let atom = conn
        .intern_atom(false, atom_name.as_bytes())
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?
        .atom;
    g.wm_sn = atom;

    // C: xcb_get_selection_owner → if owner != NONE && !replace_wm → error
    let owner = conn
        .get_selection_owner(g.wm_sn)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?
        .owner;
    if owner != x11rb::NONE && !g.replace_wm {
        return Err("Another window manager is already running (WM_Sn is owned)".to_string());
    }

    // C: xcb_create_window(conn, root_depth, wm_sn_selection_owner, root,
    //                     -1, -1, 1, 1, 0, COPY_FROM_PARENT, ...)
    use x11rb::protocol::xproto::{CreateWindowAux, WindowClass};
    conn.create_window(
        g.root_depth,
        g.wm_sn_selection_owner,
        g.root,
        -1,
        -1,
        1,
        1,
        0,
        WindowClass::COPY_FROM_PARENT,
        0,
        &CreateWindowAux::new(),
    )
    .map_err(|e| e.to_string())?;

    // C: xcb_set_selection_owner(conn, wm_sn_selection_owner, wm_sn, timestamp)
    conn.set_selection_owner(g.wm_sn_selection_owner, g.wm_sn, g.last_timestamp)
        .map_err(|e| e.to_string())?;

    x11rb::connection::Connection::flush(conn).map_err(|e| e.to_string())?;
    Ok(())
}

/// C: src/output.c:141 get_first_output — the primary active output, else the
/// first active output. C falls back to `root_output` when nothing is active
/// (it logs "No output found, this is a bug, using root output"); the root
/// output is the one created by randr.c:1091 and is never deactivated at
/// startup, so the fallback is not reachable here and `None` is returned.
fn first_output(t: &i3_core::Tree) -> Option<usize> {
    let mut first = None;
    for (idx, output) in t.outputs.iter().enumerate() {
        if !output.active {
            continue;
        }
        if output.primary {
            return Some(idx);
        }
        if first.is_none() {
            first = Some(idx);
        }
    }
    if first.is_some() {
        return first;
    }
    i3_core::log::errorlog(
        "No output found, this is a bug, using root output, you should not see this.\n",
    );
    None
}

/// C: include/xcb.h ROOT_EVENT_MASK / src/main.c:804-810
///
/// The root window event mask is what makes this process the window manager:
/// XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT routes MapRequests, ConfigureRequests
/// and UnmapNotify of top-level windows to us. The checked request mirrors the
/// C error handling ("Another window manager seems to be running").
fn select_root_event_mask(
    conn: &i3_core::xcb_util::XConnection,
    root: u32,
) -> Result<(), String> {
    use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt as _, EventMask};

    // include/xcb.h: XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT | BUTTON_PRESS |
    // STRUCTURE_NOTIFY | POINTER_MOTION | PROPERTY_CHANGE | FOCUS_CHANGE |
    // ENTER_WINDOW
    let mask = EventMask::SUBSTRUCTURE_REDIRECT
        | EventMask::BUTTON_PRESS
        | EventMask::STRUCTURE_NOTIFY
        | EventMask::POINTER_MOTION
        | EventMask::PROPERTY_CHANGE
        | EventMask::FOCUS_CHANGE
        | EventMask::ENTER_WINDOW;

    conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(mask))
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| format!("Another window manager seems to be running (X error {e})"))?;
    x11rb::connection::Connection::flush(conn).map_err(|e| e.to_string())?;
    Ok(())
}

/// C: src/main.c:988-1006 — query the pointer, pick the output under it
/// (falling back to the first output) and activate the focused con of that
/// output's content container.
fn focus_start_output(
    x: &mut dyn i3_core::backend::XConn,
    t: &mut i3_core::Tree,
    b: &mut dyn ConBackend,
    root: u32,
) {
    // C: xcb_query_pointer(conn, root) + xcb_query_pointer_reply; both halves
    // are one call through the port's XConn abstraction.
    let pointer = x.query_pointer(root);

    let mut output = None;
    match &pointer {
        None => {
            // C: ELOG("Could not query pointer position, using first screen\n");
            i3_core::log::errorlog(
                "Could not query pointer position, using first screen\n",
            );
        }
        Some(reply) => {
            // C: DLOG("Pointer at %d, %d\n", ...)
            output = i3_core::output::get_output_containing(t, reply.root_x as u32, reply.root_y as u32);
            if output.is_none() {
                // C: ELOG("ERROR: No screen at (%d, %d), starting on the first
                // screen\n", ...)
                i3_core::log::errorlog(&format!(
                    "ERROR: No screen at ({}, {}), starting on the first screen\n",
                    reply.root_x, reply.root_y
                ));
            }
        }
    }

    let output = match output.or_else(|| first_output(t)) {
        Some(output) => output,
        // Unreachable with a real X server: randr_query_outputs() always
        // leaves at least the root output behind (randr.c:fallback_to_root_output
        // / randr.c:1091).
        None => return,
    };

    // C: Output *output = ...; con_activate(con_descend_focused(
    //     output_get_content(output->con)));
    let content = t.outputs[output]
        .con
        .and_then(|con| i3_core::output::output_get_content(t, con));
    match content {
        Some(content) => {
            let focused = i3_core::con::con_descend_focused(t, content);
            i3_core::con::con_activate(t, b, focused);
        }
        // C dereferences output->con here; the output list never contains an
        // active output without a con after randr_query_outputs().
        None => return,
    }
}

/// C: src/main.c:1095-1123 — grab_server; manage what is already on screen;
/// ungrab.
fn manage_existing_windows_guarded(
    t: &mut i3_core::Tree,
    b: &mut dyn ConBackend,
    x: &mut RustXConn,
    a: &Atoms,
) {
    use i3_core::backend::XConn;
    // C: xcb_grab_server(conn);
    x.grab_server();
    // C: manage_existing_windows(root)
    i3_core::manage::manage_existing_windows(t, b, x, a);
    // C: xcb_ungrab_server(conn);
    x.ungrab_server();
}

/// C: src/main.c:1011-1231 — set up the watchers and run the event loop.
pub fn run(
    conn: &i3_core::xcb_util::SharedConnection,
    g: &mut I3Globals,
    _screen_num: usize,
    argv: &[std::ffi::OsString],
) -> Result<ExitCode, String> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;

    // C: main.c:544-545 — "Enable logging to handle the case when the user did
    // not specify --shmlog-size": init_logging() opens the SHM log buffer for
    // debug builds (shmlog_size defaults to SHMLOG_SIZE), which is what
    // exports I3_SHMLOG_PATH in x_set_i3_atoms() below.
    i3_core::log::init_logging();

    // C: main.c:611-613 root attributes + the ICCCM timestamp handshake
    conn.change_window_attributes(
        g.root,
        &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
            .event_mask(x11rb::protocol::xproto::EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| e.to_string())?;
    // C: xcb_change_property(conn, XCB_PROP_MODE_APPEND, root,
    //     XCB_ATOM_SUPERSCRIPT_X, XCB_ATOM_CARDINAL, 32, 0, "");
    conn.change_property(
        x11rb::protocol::xproto::PropMode::APPEND,
        g.root,
        x11rb::protocol::xproto::AtomEnum::SUPERSCRIPT_X,
        x11rb::protocol::xproto::AtomEnum::CARDINAL,
        32,
        0,
        b"",
    )
    .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;

    // C: main.c:656-670 — wait for the PropertyNotify and remember the time.
    {
        use x11rb::protocol::Event;
        loop {
            match x11rb::connection::Connection::wait_for_event(conn) {
                Ok(Event::PropertyNotify(e)) => {
                    g.last_timestamp = e.time;
                    // C: last_timestamp is the one global; the X layer reads it
                    // for send_take_focus / xcb_set_input_focus (src/x.c:1415).
                    i3_core::x::set_last_timestamp(e.time);
                    break;
                }
                Ok(_) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    mark!("before atoms");
    // C: main.c:672-685 — resolve the atoms (the xmacro list).
    // C: one xcb_connection_t for the whole process (see RustXConn).
    let mut x = RustXConn::wrap(std::rc::Rc::clone(conn), g.conn_screen);
    let atoms = Atoms::intern_all(&mut x);

    // C: src/main.c:642 restore_connect() — in C this opens the second X
    // connection used for the placeholder windows; the port reuses the one
    // connection (see restore_layout.rs's module comment).
    i3_core::restore_layout::restore_connect(
        std::rc::Rc::clone(conn),
        g.conn_screen,
        atoms.clone(),
    );

    // C: src/main.c:679 init_dpi() — the Xft.dpi resource, else the DPI guess
    // from the root window's physical size. logical_px() and the Pango font
    // metrics depend on it.
    {
        let root = conn.setup().roots.get(g.conn_screen);
        libi3::dpi::set_root_screen(root.map(|root| libi3::dpi::RootScreen {
            root: root.root,
            // C: root_screen->width_in_pixels/height_in_pixels — also read by
            // src/floating.c:22 total_outputs_dimensions().
            width_in_pixels: root.width_in_pixels,
            height_in_pixels: root.height_in_pixels,
            height_in_millimeters: root.height_in_millimeters,
        }));
        libi3::dpi::init_dpi(Some(conn));
    }

    mark!("atoms done");
    // C: src/config_parser.c:570-580 — `set_from_resource` reads the X
    // resource database (RESOURCE_MANAGER on the root window). In C the parser
    // has the xcb connection as a global and loads it lazily; the port's parser
    // has no connection, so the caller that owns it installs the database
    // before load_configuration() runs.
    i3_core::config::load_resource_manager(Some(&**conn));
    // C: main.c:687 load_configuration(override_configpath, C_LOAD);
    let mut directives = ConfigDirectives::default();
    load_configuration(
        &mut directives,
        g.override_configpath.as_deref(),
        config_load_t::C_LOAD,
    );

    // C: src/config.c:277-281 + config_directives.c:173 CFGFUN(font) — the
    // font is loaded right after parsing and set_font() makes it the current
    // font for every draw_text() call. The port's parser has no X connection,
    // so the real metrics are loaded here (i3_core::config::apply_font is the
    // shared step; cmd_reload runs the same one).
    i3_core::config::apply_font(Some(&**conn), &mut directives, None);

    mark!("config loaded");
    // C: main.c:689-701 — the IPC socket (config value, else $I3SOCK, else
    // the per-process socket file)
    let socket_path = directives
        .config
        .ipc_socket_path
        .clone()
        .or_else(|| std::env::var("I3SOCK").ok())
        .or_else(|| libi3::get_process_filename("ipc-socket"))
        .unwrap_or_else(|| format!("/tmp/i3-{}.ipc-socket.{}", unsafe { libc::getuid() }, std::process::id()));
    let mut ipc = match IpcServer::bind(&socket_path) {
        Ok(v) => v,
        Err(e) => {
            return Err(format!(
                "Could not create the IPC socket: {} ({})",
                socket_path, e
            ))
        }
    };

    mark!("ipc socket bound");
    // C: no counterpart — in C the IPC handlers read the globals of
    // src/config.c / src/config_directives.c / src/bindings.c directly
    // (current_configpath set at config.c:235, included_files at config.c:259
    // and config_directives.c:53, modes + current_binding_mode at
    // config.c:170-179 with bindings.c:651 for later `mode` switches,
    // barconfigs in config_directives.c). load_configuration() at main.c:687
    // has just filled them, so the port copies them into the IPC state once
    // here; without this the GET_VERSION / GET_CONFIG / GET_BINDING_MODES /
    // GET_BINDING_STATE / GET_BAR_CONFIG replies stay empty.
    ipc.sync_config_state(&directives);
    // C: CFGFUN(ipc_kill_timeout) (config_directives.c:707) wrote the ipc
    // layer's kill_timeout while the configuration was parsed. The port keeps
    // it in the config state, so it is applied to the server here (and again
    // after a reload).
    ipc.ipc_set_kill_timeout(directives.ipc_kill_timeout);

    // C: src/main.c:1016-1026 — "Chose a file name in /tmp/ based on the PID":
    // get_process_filename("log-stream-socket") + create_socket(), and the
    // ev_io watcher (log_new_client) on the listening fd. create_socket() hands
    // back the path it actually bound, which is what
    // I3_LOG_STREAM_SOCKET_PATH exports (src/x.c:1544) and what i3-dump-log -f
    // connects to. -1 keeps i3 running without the socket, like C.
    let log_socket = match libi3::get_process_filename("log-stream-socket") {
        Some(path) => {
            let mut bound = None;
            let fd = libi3::create_socket(&path, &mut bound);
            i3_core::log::set_current_log_stream_socket_path(bound);
            fd
        }
        None => -1,
    };
    if log_socket == -1 {
        // C: ELOG("Could not create the log socket, i3-dump-log -f will not work\n");
        i3_core::log::errorlog("Could not create the log socket, i3-dump-log -f will not work\n");
    }

    // C: src/main.c:1030-1053 — "Also handle the UNIX domain sockets passed
    // via socket activation. The parameter 0 means 'do not remove the
    // environment variables', we need to be able to reexec."
    {
        let listen_fds = libi3::sd_daemon::sd_listen_fds(false);
        if listen_fds < 0 {
            i3_core::log::errorlog("socket activation: Error in sd_listen_fds\n");
        } else if listen_fds == 0 {
            i3_core::log::debuglog("socket activation: no sockets passed\n");
        } else {
            for fd in libi3::sd_daemon::SD_LISTEN_FDS_START
                ..libi3::sd_daemon::SD_LISTEN_FDS_START + listen_fds
            {
                i3_core::log::debuglog(&format!(
                    "socket activation: also listening on fd {}\n",
                    fd
                ));
                ipc.add_activation_listener(fd);
            }
        }
    }

    // C: src/main.c:1059-1065 — adopt the IPC fd that the previous process
    // carried across an in-place restart (cmd_restart puts it into
    // _I3_RESTART_FD). The waiting i3-msg becomes a client of the new i3.
    if let Some(fd) = i3_core::restore_layout::parse_restart_fd() {
        use std::os::unix::io::FromRawFd;
        i3_core::log::debuglog(&format!("serving restart fd {}", fd));
        let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
        let _ = ipc.ipc_new_client_on_fd(stream);
        std::env::remove_var("_I3_RESTART_FD");
    }

    // C: src/main.c:1055-1057 — "Set up i3 specific atoms like
    // I3_SOCKET_PATH and I3_CONFIG_PATH". Other i3 tools (i3-msg above all)
    // read these root window properties to find this instance, and the
    // testsuite's get_socket_path() does the same.
    // C: current_log_stream_socket_path (src/log.c:390) is read by
    // x_set_i3_atoms() from src/log.c itself, so only the IPC/config paths are
    // mirrored here.
    i3_core::x::set_i3_atom_paths(
        Some(&socket_path),
        directives.current_configpath.as_deref().unwrap_or(""),
    );
    i3_core::x::x_set_i3_atoms(&mut x, &atoms);

    // C: main.c:703-705 config.force_xinerama
    if directives.config.force_xinerama {
        g.force_xinerama = true;
    }

    mark!("before WM_Sn");
    // C: main.c:707 Acquire the WM_Sn selection.
    acquire_wm_sn(conn, g)?;

    mark!("WM_Sn acquired");

    // C: main.c:804-810 — register as the window manager: ROOT_EVENT_MASK on
    // the root window. SUBSTRUCTURE_REDIRECT is what makes the X server turn
    // every XMapWindow of a top-level client into a MapRequest for us; without
    // it no MapRequest ever arrives and manage_window() is never reached.
    // The C uses the checked variant and exits with "Another window manager
    // seems to be running" when the request fails (another WM holds the
    // redirect).
    select_root_event_mask(conn, g.root)?;

    // C: main.c:785+ — EWMH hints, keysyms, keymap, key grabs
    let _ewmh_owner = ewmh_setup_hints(&mut x, &atoms);

    mark!("ewmh done");

    // C: main.c:840-887 — the xcb-xkb registration (use_extension,
    // select_events, per_client_flags) and the XKB event base. Without it the
    // server does not report keyboard group changes and does not fill the
    // group bits of KeyPress/KeyRelease events.
    mark!("before xkb setup");
    let mut xkb_factory = i3_core::xkb::RealKeymapFactory::new();
    let xkb_base = match xkb_factory.as_ref() {
        Some(factory) => factory.setup_extension(conn).unwrap_or(-1),
        // C: xkb_supported = extreply->present — a missing xkbcommon context
        // also means "no XKB", and load_keymap() falls back to the local RMLVO.
        None => -1,
    };
    libi3::log::debuglog(&format!("xkb_base = {}\n", xkb_base));

    // C: main.c:941-958 — tree init (or restore during an in-place restart),
    // then the output setup (fake outputs / Xinerama / RandR).
    let mut t = i3_core::Tree::new();
    // C: the tree layer reads the global `config` (src/render.c:27
    // render_deco_height() reads config.font.height, src/gaps.c the gap
    // sizes, …). The port keeps a subset in the tree, so the values that were
    // just parsed are copied over here (cmd_reload runs the same step).
    i3_core::config::apply_to_tree(&mut t, &directives.config);
    let geom = {
        use i3_core::backend::XConn;
        x.get_geometry(g.root)
            .map(|r| Rect::new(r.x as u32, r.y as u32, r.width as u32, r.height as u32))
            .unwrap_or_else(|| Rect::new(0, 0, 0, 0))
    };

    // C: src/config.c:170-179 + config.c:276-277 — the parsed modes become the
    // global mode list and the global `bindings` points at the default mode.
    // C does this inside load_configuration(); the port needs the Tree for
    // extract_workspace_names_from_bindings()/reorder_bindings(), so it runs
    // here, right before the keymap is loaded.
    let mut bindings = i3_core::bindings::Bindings::new();
    {
        bindings.load_config_bindings(
            &mut t,
            &directives.modes,
            directives.current_configpath.clone(),
        );
        libi3::log::debuglog(&format!(
            "loaded {} binding(s) from the default mode\n",
            t.bindings.len()
        ));
        // C: main.c:912-921 — keysyms, numlock mask, keymap, translate, grabs.
        bindings.xkb_supported = xkb_base > -1;
        match xkb_factory.as_mut() {
            Some(factory) => {
                if !i3_core::bindings::load_keymap(&mut bindings, &mut x, factory) {
                    // C: die("Could not load keymap\n")
                    return Err("Could not load keymap".to_string());
                }
            }
            None => return Err("Could not create xkbcommon context".to_string()),
        }
        // C: main.c:914 xcb_numlock_mask = aio_get_mod_mask_for(XCB_NUM_LOCK, keysyms)
        // (the port resolves Num_Lock through the keymap loaded above).
        if let Some(keymap) = bindings.keymap.as_deref() {
            let numlock_mask =
                i3_core::xkb::aio_get_mod_mask_for(&mut x, keymap, i3_core::xkb::XCB_NUM_LOCK);
            libi3::log::debuglog(&format!("xcb_numlock_mask = {}\n", numlock_mask));
            i3_core::bindings::set_xcb_numlock_mask(numlock_mask);
        }
        // C: main.c:920-921 translate_keysyms(); grab_all_keys(conn);
        {
            let mut hooks = i3_core::bindings::NullHooks;
            i3_core::bindings::translate_keysyms(&bindings, &mut t, &mut hooks);
        }
        i3_core::bindings::grab_all_keys(&bindings, &t, &mut x);
    }

    // C: there is no backend abstraction in C — tree_init/con_new call
    // x_con_init() directly. The port's ConBackend has to be the real X11 one
    // here, otherwise no frame windows exist, manage_window() reparents the
    // client into XCB_WINDOW_NONE and nothing is ever mapped.
    let mut backend = i3_core::xbackend::XBackend::new(
        std::rc::Rc::clone(conn),
        g.conn_screen,
        atoms.clone(),
    );
    // C: the config globals (config.client colors, config.font) are what
    // x_draw_decoration reads; the draw backend gets its copy here.
    backend.set_draw_config(&directives.config);
    // C: start_application() exports I3SOCK=current_socketpath (startup.c:203)
    // so that the started application can talk to this i3's IPC socket.
    backend.set_socketpath(&socket_path);
    let restored = match &g.layout_path {
        // C: src/main.c:924-926 needs_tree_init = !tree_restore(layout_path, …)
        Some(path) => i3_core::restore_layout::tree_restore_from_file(
            &mut t,
            &mut backend,
            path,
            geom,
        ),
        None => false,
    };
    if !restored {
        tree_init(&mut t, &mut backend, geom);
    }

    // C: src/main.c:927-930 — unlink() the layout file and rmdir() its
    // directory after a successful in-place restart.
    if let Some(path) = g.layout_path.clone() {
        if g.delete_layout_path && restored {
            let _ = std::fs::remove_file(&path);
            if let Some(dir) = std::path::Path::new(&path).parent() {
                let _ = std::fs::remove_dir(dir);
            }
        }
        // C: src/main.c:986 FREE(layout_path)
        g.layout_path = None;
        g.delete_layout_path = false;
    }

    mark!("tree init done");
    // C: src/config_directives.c fills the global `barconfigs` list while the
    // configuration is parsed (main.c:687); commands.c:2207 cmd_bar_mode,
    // commands.c:2266 cmd_bar_hidden_state and ipc.c:1109 GET_BAR_CONFIG all
    // read and write that one list, so it is copied into the tree here.
    t.barconfigs = directives.barconfigs.clone();
    // C: the assign / for_window / "workspace … output …" directives are
    // stored in globals (src/assignments.c assignments, src/config.c
    // ws_assignments) that the parser fills at config.c:165 and that
    // manage.c / assignments.c read directly. The port keeps them in
    // ConfigDirectives, so they are copied into the Tree together with the
    // barconfigs — without this no for_window criterion ever runs.
    t.assignments = directives.assignments.clone();
    t.ws_assignments = directives.ws_assignments.clone();

    let mut randr = RandrState::default();
    let mut randr_event_base: i32 = -1;
    // C: src/main.c:938-943 — "Setup fake outputs for testing": the
    // --fake-outputs command line option wins over the config directive.
    let fake_outputs = g
        .fake_outputs
        .clone()
        .or_else(|| directives.config.fake_outputs.clone());
    if let Some(spec) = fake_outputs {
        // C: main.c:945-949 fake_outputs_init(fake_outputs); FREE(fake_outputs);
        // config.fake_outputs = NULL;
        i3_core::fake_outputs::fake_outputs_init(&mut t, &mut backend, &spec);
        g.fake_outputs = None;
        directives.config.fake_outputs = None;
    } else if g.force_xinerama {
        // C: main.c:950-956 else if (force_xinerama) xinerama_init(); — when
        // Xinerama is forced, RandR is *not* initialized at all (otherwise the
        // outputs of both backends end up in the list and every workspace
        // exists twice).
        use i3_core::backend::XConn as _;
        let screens = x.xinerama_screens();
        i3_core::randr::xinerama_init(&mut t, &mut backend, &mut x, screens);
    } else {
        // C: main.c:961-963 randr_init(&randr_base, disable_randr15 || ...)
        randr_init(
            &mut t,
            &mut backend,
            &mut x,
            &mut randr,
            &mut randr_event_base,
            g.disable_randr15,
        );
    }

    mark!("randr init done");

    // C: main.c:988 scratchpad_fix_resolution() — scale the __i3 pseudo
    // output (1280x1024, tree.c:_create___i3) to the real output dimensions
    // once the outputs are known.
    i3_core::scratchpad::scratchpad_fix_resolution(&mut t, &mut backend);

    // C: main.c:990 xcb_query_pointer + main.c:1000-1006
    // con_activate(con_descend_focused(output_get_content(output->con))).
    // tree_init() alone leaves the global `focused` unset (src/tree.c only
    // builds croot and __i3), and commands.c dereferences `focused` without a
    // NULL check (e.g. maybe_back_and_forth, commands.c:83), so without this
    // step every command that needs a focused con is executed on a NULL
    // pointer in C and on `None` here.
    focus_start_output(&mut x, &mut t, &mut backend, g.root);

    // C: main.c:1008 tree_render(); — the first render after the initial focus,
    // before any client is managed. Without it the first push happens only when
    // a client maps (C pushes the empty tree first).
    i3_core::tree::tree_render(&mut t, &mut backend);

    // C: main.c:1070 ewmh_update_workarea() — i3 does not implement _NET_WORKAREA,
    // so the property some display managers (LightDM) set is deleted actively.
    i3_core::ewmh::ewmh_update_workarea(&mut x, &atoms);

    // C: main.c:1073 ewmh_update_desktop_properties() — the initial values of
    // _NET_NUMBER_OF_DESKTOPS / _NET_DESKTOP_VIEWPORT / _NET_CURRENT_DESKTOP /
    // _NET_DESKTOP_NAMES / _NET_WM_DESKTOP, now that the workspaces exist.
    i3_core::ewmh::ewmh_update_desktop_properties(&mut t, &mut x, &atoms);

    // C: main.c:1095-1123 — the grab_server/manage_existing_windows handshake.
    manage_existing_windows_guarded(&mut t, &mut backend, &mut x, &atoms);

    // C: main.c:1128-1145 — "This is not an in-place restart, checking if a
    // wallpaper is set." When the root's window background is NONE, old content
    // stays visible when a window is closed ("my terminal does not seem to
    // close!"), so i3 tests for it by opening and closing a window and, if the
    // content stayed, copies the root content into a pixmap.
    if g.autostart {
        libi3::log::debuglog("This is not an in-place restart, checking if a wallpaper is set.\n");
        // C: xcb_screen_t *root = xcb_aux_get_screen(conn, conn_screen);
        if let Some(screen) = conn.setup().roots.get(g.conn_screen) {
            if libi3::is_background_set::is_background_set(&**conn, screen) {
                libi3::log::debuglog("A wallpaper is set, so no screenshot is necessary.\n");
            } else {
                libi3::log::debuglog("No wallpaper set, copying root window contents to a pixmap");
                libi3::screenshot_wallpaper::set_screenshot_as_wallpaper(&**conn, screen);
            }
        }
    }

    // C: src/tree.c:110 restore_open_placeholder_windows(croot) — see the
    // comment in restore_layout::tree_restore_from_file: the placeholders are
    // opened after the existing clients have been re-managed, so the restored
    // containers keep their real windows.
    if restored {
        let croot = t.croot.expect("restored tree without croot");
        i3_core::restore_layout::restore_open_placeholder_windows(&mut t, croot);
    }

    // C: main.c:1169-1196 — "Autostarting exec-lines" (skipped with -a) and
    // "Autostarting exec_always-lines". The queues were filled by
    // CFGFUN(exec) while the configuration was parsed; a reload re-fills them,
    // which is how exec_always runs again on reload.
    i3_core::startup::start_autostart(
        &t,
        &mut backend,
        &mut directives.autostarts,
        &mut directives.autostarts_always,
        g.autostart,
    );

    // C: main.c:1195-1205 — one i3bar per configured bar block. This is what
    // makes an existing rice work unchanged: the user does not run i3bar
    // themselves, i3 starts it with --bar_id and --socket.
    i3_core::startup::start_bars(
        &t,
        &mut backend,
        &directives.barconfigs,
        &ipc.socketpath.clone().unwrap_or_default(),
    );

    mark!("manage_existing_windows done");
    // C: main.c:1224 ev_loop(main_loop, 0);
    //
    // C reads config.* live inside the loop (cmd_reload rewrites the same
    // struct), so the parsed configuration is handed to the loop instead of
    // copying single fields out of it.
    event_loop(
        conn,
        &mut t,
        &mut backend,
        &mut x,
        &atoms,
        &mut ipc,
        log_socket,
        &mut randr,
        &mut directives,
        g,
        argv,
        &mut bindings,
        xkb_base,
        &mut xkb_factory,
    )
}

/// C: src/main.c:129 xcb_prepare_cb + the ev_io/ev_timer/ev_child watchers
/// driven by libev. In Rust the loop is explicit: drain X events, dispatch
/// them through handle_x11_event_ctx, service the IPC socket, then push the
/// rendered tree and flush — the same order libev gives us through ev_prepare
/// plus the io callbacks.
fn event_loop(
    conn: &i3_core::xcb_util::XConnection,
    t: &mut i3_core::Tree,
    backend: &mut dyn ConBackend,
    x: &mut RustXConn,
    atoms: &Atoms,
    ipc: &mut IpcServer,
    // C: src/main.c:1016 log_socket — the listening fd of the log stream
    // socket, -1 when create_socket() failed. C registers the ev_io watcher
    // (log_new_client) only in the success case.
    log_socket: i32,
    randr: &mut RandrState,
    // C: the parsed configuration is the global `config` + the lists of
    // src/config_directives.c. cmd_reload rewrites all of it in place, and the
    // loop reads it live (disable_focus_follows_mouse per event,
    // restart_state_path when restarting), so the whole state is passed in
    // rather than copied field by field.
    directives: &mut i3_core::config_directives::ConfigDirectives,
    g: &mut I3Globals,
    argv: &[std::ffi::OsString],
    bindings: &mut i3_core::bindings::Bindings,
    xkb_base: i32,
    xkb_factory: &mut Option<i3_core::xkb::RealKeymapFactory>,
) -> Result<ExitCode, String> {
    use i3_core::backend::XConn;
    use i3_core::handlers::{handle_x11_event_ctx, EventContext};

    // C: src/main.c:893-902 — query the extension event bases (randr_base,
    // shape_base). xkb_base comes from the registration done in run():
    // xkb_x11_setup_xkb_extension() reports the same event base.
    mark!("event_loop entered");
    let randr_base = extension_first_event(conn, "RANDR");
    mark!("randr extension queried");
    let shape_base = extension_first_event(conn, "SHAPE");
    mark!("xkb/shape extensions queried");
    let shape_supported = shape_base > -1;

    // C: the loop runs until cmd_exit/cmd_restart tears it down.
    // I3_TRACE_LOOP=1 prints one line per iteration (development aid; C has no
    // counterpart, it is not part of the ported behaviour).
    // C: the ev_timer watchers are part of the libev loop; the port keeps the
    // one timer state it needs next to the loop (see the poll below).
    let mut urgency_timers = i3_core::workspace::UrgencyTimerQueue::default();
    let trace = std::env::var_os("I3_TRACE_LOOP").is_some();
    let trace_events = std::env::var_os("I3_TRACE_EVENTS").is_some();
    if trace {
        eprintln!("TRACE: event loop entered");
    }
    // C: src/main.c:1164 setup_term_handlers() — the graceful Term handlers are
    // installed unconditionally (only the crash handler obeys
    // --disable-signalhandler). libev runs them inside the loop; the port
    // records the signal number and exits here.
    i3_core::sighandler::setup_term_handlers();

    loop {
        let mut had_events = false;

        // C: src/main.c:226 handle_term_signal — exit(128 + signum), which runs
        // the atexit(i3_exit) cleanup: ipc_shutdown(), unlink of the IPC
        // socket, shm_unlink of the SHM log and purge_zerobyte_logfile().
        // 540-sigterm-cleanup.t checks the socket file is gone after SIGTERM.
        if let Some(signum) = i3_core::sighandler::take_term_signal() {
            i3_core::log::log_(&format!("Received signal {}, exiting\n", signum));
            if let Some(path) = ipc.socketpath.clone() {
                ipc.ipc_shutdown(
                    i3_core::ipc::shutdown_reason_t::SHUTDOWN_REASON_EXIT,
                    None,
                );
                let _ = std::fs::remove_file(&path);
            }
            // C: src/main.c:188-190 (i3_exit) — unlink(current_log_stream_socket_path).
            i3_core::log::unlink_log_stream_socket();
            // C: i3_exit — if (*shmlogname != '\0') shm_unlink(shmlogname);
            i3_core::log::shmlog_destroy();
            i3_core::log::purge_zerobyte_logfile();
            std::process::exit(128 + signum);
        }

        // C: main.c:129-160 xcb_prepare_cb — poll *all* pending events and
        // hand them to handle_event(type, event).
        loop {
            let event = match x11rb::connection::Connection::poll_for_event(conn) {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(e) => return Err(e.to_string()),
            };
            had_events = true;
            if trace_events {
                // Development aid (I3_TRACE_EVENTS=1), not part of C: prints the
                // X events the loop receives, before they are dispatched.
                match &event {
                    x11rb::protocol::Event::PropertyNotify(e) => eprintln!(
                        "TRACE: PropertyNotify win={:#x} atom={} state={:?}",
                        e.window, e.atom, e.state
                    ),
                    x11rb::protocol::Event::MapRequest(e) => {
                        eprintln!("TRACE: MapRequest win={:#x}", e.window)
                    }
                    x11rb::protocol::Event::UnmapNotify(e) => {
                        eprintln!("TRACE: UnmapNotify win={:#x}", e.window)
                    }
                    x11rb::protocol::Event::ButtonPress(e) => eprintln!(
                        "TRACE: ButtonPress event={:#x} child={:#x} detail={} state={:#x} root=({},{})",
                        e.event, e.child, e.detail, u16::from(e.state), e.root_x, e.root_y
                    ),
                    x11rb::protocol::Event::ButtonRelease(e) => eprintln!(
                        "TRACE: ButtonRelease event={:#x} child={:#x} detail={} state={:#x} root=({},{})",
                        e.event, e.child, e.detail, u16::from(e.state), e.root_x, e.root_y
                    ),
                    other => eprintln!(
                        "TRACE: {}",
                        format!("{:?}", other).chars().take(40).collect::<String>()
                    ),
                }
            }

            // The C globals of handle_event, assembled per event so the
            // mutable borrows end before the IPC/rendering step.
            let mut ctx = EventContext {
                wm_sn: g.wm_sn,
                root: g.root,
                randr_base,
                xkb_base,
                shape_base,
                shape_supported,
                bindings: Some(&mut *bindings),
                xkb_factory: xkb_factory.as_mut(),
                // C: the randr.c output state; handle_screen_change and the
                // ConfigureNotify handler re-query the outputs through the X
                // connection that is passed to them.
                randr: Some(&mut *randr),
                ipc: Some(&mut *ipc),
                last_timestamp: g.last_timestamp,
                // C: handlers.c reads config.disable_focus_follows_mouse.
                disable_focus_follows_mouse: directives.config.disable_focus_follows_mouse,
                force_xinerama: g.force_xinerama,
            };

            // C: handle_event(type, event)
            let keep_running = handle_x11_event_ctx(t, backend, x, atoms, &event, &mut ctx);
            g.last_timestamp = ctx.last_timestamp;
            // C: the C global last_timestamp is what src/x.c reads.
            i3_core::x::set_last_timestamp(g.last_timestamp);

            if !keep_running {
                // C: src/handlers.c:1211 — lost the WM_Sn selection,
                // exit(EXIT_SUCCESS) (which runs atexit(purge_zerobyte_logfile)
                // in C; call it explicitly so an empty errorlog never stays).
                // C: i3_exit (src/main.c:188) unlinks the log stream socket
                // on every exit path, including this one.
                i3_core::log::unlink_log_stream_socket();
                i3_core::log::purge_zerobyte_logfile();
                return Ok(ExitCode::SUCCESS);
            }

            // C: the handlers call tree_render() themselves (e.g.
            // src/handlers.c:725 at the end of handle_net_wm_state_change), so
            // the render + push happens *inside* handle_event(), before the
            // next event of the same batch is looked at. Deferring it to the
            // end of the iteration would let the i3 sync protocol reply win the
            // race (sync_respond() flushes immediately) and a client would see
            // the pre-render geometry.
            if i3_core::tree::tree_render_requested() {
                i3_core::tree::tree_render(t, backend);
            }
        }

        // C: src/main.c:129-160 xcb_prepare_cb is an ev_prepare watcher: libev
        // runs prepare watchers BEFORE the io callbacks of the same iteration,
        // and the C handlers send their IPC events synchronously inside
        // handle_event(). The port queues those events instead, so they have to
        // be forwarded here — before any IPC message (send_tick, commands,
        // replies) of this iteration is processed — or a subscriber's tick
        // sentinel would overtake events that C had already delivered.
        // (275-ipc-window-close, 220-ipc-window-title and the urgent events of
        // 277-ipc-window-urgent depend on that order.)
        ipc.drain_pending_events();

        // C: the ev_timer watchers. The deferred urgency reset
        // (src/workspace.c:504-516 ev_timer_init / ev_timer_again →
        // workspace.c:414 workspace_defer_update_urgent_hint_cb) is the one
        // timer the port needs so far; the queue turns the con-side
        // registrations into expiries (libev does that in C) and the callback
        // is the same function C's watcher calls.
        for con in urgency_timers.poll(t, std::time::Instant::now()) {
            i3_core::workspace::workspace_defer_update_urgent_hint_cb(t, backend, con);
        }

        // C: ev_io on the IPC listening socket and on every client fd.
        while ipc.accept_client().is_some() {
            if trace {
                eprintln!("TRACE: accepted an IPC client");
            }
        }
        if trace {
            eprintln!("TRACE: service_ipc_clients (clients={})", ipc.clients.len());
        }
        service_ipc_clients(ipc, t, backend);

        // C: src/main.c:1024-1025 ev_io_init(log_io, log_new_client, log_socket,
        // EV_READ) — every i3-dump-log -f connects here. The listening socket is
        // non-blocking, so the loop accepts whatever is pending.
        if log_socket != -1 {
            while i3_core::log::log_new_client(log_socket).is_some() {
                if trace {
                    eprintln!("TRACE: accepted a log stream client");
                }
            }
        }
        // C: the log clients have no read watcher at all; the port polls them
        // (see poll_wait), so their bytes are drained and closed clients are
        // dropped here instead of spinning poll().
        i3_core::log::service_log_clients();

        // C: src/commands.c:2347 update_shmlog_atom() — cmd_shmlog runs inside
        // its IPC handler in C and rewrites the I3_SHMLOG_PATH root atom right
        // away. The port's command layer has no X connection, so the request is
        // performed here, in the same iteration and before x.flush() below.
        if i3_core::log::take_shmlog_atom_update() {
            i3_core::x::update_shmlog_atom(x, atoms);
        }

        // C: src/commands.c:986 cmd_mode → src/bindings.c:639 switch_mode().
        // C runs switch_mode() synchronously inside the command handler (the
        // handler reaches the global bindings). The port's command layer only
        // has the backend, which records the request (ConBackend::switch_mode),
        // and the loop applies it here: ungrabs, translates and grabs the new
        // mode's keys, and queues the IPC "mode" event — in the same iteration,
        // before any later key event is dispatched.
        // C: bindings.c:669 ipc_send_mode_event() sends through the global IPC
        // server; the port queues the event (ipc::queue_mode_event).
        let mut mode_hooks = i3_core::ipc::IpcHooks;
        i3_core::bindings::apply_pending_mode_switches(bindings, t, x, backend, &mut mode_hooks);
        // GET_BINDING_STATE (C: src/ipc.c:1398) reads the global
        // current_binding_mode, which the port mirrors into the IPC state.
        ipc.state.current_binding_mode = bindings
            .current_binding_mode
            .clone()
            .unwrap_or_default();

        // C: src/commands.c:1668 cmd_reload — the handler cannot reach the
        // configuration, the bindings list, the X connection or the IPC server
        // from the command layer (the key binding path has no more than the
        // Tree and the backend either), so it leaves a request behind and the
        // loop performs the whole reload here, in the same iteration and in
        // C's order (i3_core::reload::reload).
        // C: src/commands.c:1657 cmd_exit -- exit(EXIT_SUCCESS). The command
        // layer records the request; the loop performs it here so pending IPC
        // replies are flushed first, exactly like the C exit path which runs
        // inside the command handler.
        if i3_core::reload::take_exit_request() {
            ipc.drain_pending_events();
            // C: src/main.c:187 (i3_exit) -- unlink(config.ipc_socket_path).
            // ipc_shutdown() closes the clients and the listener first, then
            // the socket file goes away.
            if let Some(path) = ipc.socketpath.clone() {
                ipc.ipc_shutdown(i3_core::ipc::shutdown_reason_t::SHUTDOWN_REASON_EXIT, None);
                let _ = std::fs::remove_file(&path);
            }
            // C: src/main.c:188-190 (i3_exit) — unlink(current_log_stream_socket_path);
            // exit() runs the atexit handler that does it in C.
            i3_core::log::unlink_log_stream_socket();
            i3_core::log::purge_zerobyte_logfile();
            return Ok(ExitCode::SUCCESS);
        }

        if i3_core::reload::take_reload_request() {
            i3_core::reload::reload(
                Some(conn),
                directives,
                g.override_configpath.as_deref(),
                bindings,
                t,
                backend,
                &mut *x,
                atoms,
            );
            // C: the IPC handlers read the very globals load_configuration()
            // has just rewritten (current_configpath, included_files, modes,
            // barconfigs); the port mirrors them into the IPC state — the same
            // step run() does once after the initial load_configuration().
            ipc.sync_config_state(directives);
            ipc.state.current_binding_mode = bindings
                .current_binding_mode
                .clone()
                .unwrap_or_default();
            // C: CFGFUN(ipc_kill_timeout) wrote ipc.c's kill_timeout while
            // parsing; the port keeps it in the config state and applies it to
            // the IpcServer here.
            ipc.ipc_set_kill_timeout(directives.ipc_kill_timeout);
        }

        // C: src/commands.c:1703-1716 cmd_restart — the handler cannot exec
        // from inside the command layer (it does not own the IPC server, the
        // X connection or argv), so it leaves a request behind and the restart
        // happens here, at the same point of the loop where C would have done
        // it.
        if let Some(exempt_fd) = i3_core::restore_layout::take_restart_request() {
            i3_core::log::log_("restarting i3\n");
            // C: ipc_shutdown(SHUTDOWN_REASON_RESTART, exempt_fd);
            ipc.ipc_shutdown(i3_core::ipc::shutdown_reason_t::SHUTDOWN_REASON_RESTART, exempt_fd);
            // C: unlink(config.ipc_socket_path);
            if let Some(path) = ipc.socketpath.clone() {
                let _ = std::fs::remove_file(&path);
            }
            // C: src/commands.c:1715-1716 — unlink(current_log_stream_socket_path);
            // the exec()ed i3 creates its own /tmp/i3-*/log-stream-socket.<pid>,
            // so the old socket file must not be left behind.
            i3_core::log::unlink_log_stream_socket();
            // C: src/util.c:295 restore_geometry() — reparent the clients to the
            // root and give up SUBSTRUCTURE_REDIRECT, otherwise the frames
            // (and with them the client windows) die with this connection.
            i3_core::manage::restore_geometry(t, &mut *x, &atoms);
            // C: src/commands.c:1720 purge_zerobyte_logfile() — atexit does
            // not run when exec()ing, so it is called explicitly here.
            i3_core::log::purge_zerobyte_logfile();
            // C: i3_restart(false) — store the layout and exec a fresh i3.
            match i3_core::restore_layout::perform_restart(
                t,
                argv,
                directives.config.restart_state_path.as_deref(),
                false,
            ) {
                Ok(()) => {} // never reached: execvp replaces the process
                Err(e) => return Err(e),
            }
        }

        // C: tree_render() runs at the end of the handler that changed the tree
        // (cmd_output->needs_tree_render, src/ipc.c:218, src/bindings.c:879,
        // src/handlers.c:...). tree_render() is render_con(croot) followed by
        // x_push_changes(croot) — the render pass is what recomputes rect /
        // window_rect / mapped (src/render.c), so pushing without it leaves the
        // X windows at their old geometry (e.g. a client message that only
        // toggles fullscreen: handlers.c:725 tree_render()).
        if i3_core::tree::take_tree_render_request() {
            i3_core::tree::tree_render(t, backend);
        }

        // C: ipc_send_window_event / ipc_send_workspace_event write to the
        // subscribers as soon as the tree/X layer calls them. The port
        // queues them (the IpcServer belongs to this loop) and forwards
        // them here, once per iteration, in the order they were queued.
        ipc.drain_pending_events();

        // C: xcb_flush(conn) at the end of xcb_prepare_cb.
        x.flush();

        // C: ev_loop() sleeps in the kernel until one of the watched file
        // descriptors becomes readable (libev: the xcb fd via ev_io, the IPC
        // listener and every client fd). Sleeping a fixed 10ms here would add
        // that latency to every IPC round trip (i3test's sync_with_i3 does
        // 65536 of them in one testcase), so the same set of fds is polled
        // with poll(2) and only a short timeout is used for the timers.
        if !had_events {
            poll_wait(conn, ipc, log_socket);
        }
    }
}


/// C: the libev watchers of the main loop (src/main.c:1011-1084): the xcb
/// connection, the IPC listening socket and the client sockets.
///
/// Blocks until at least one of them is readable or the timer deadline
/// expires. The X connection is drained by the caller, so a readable xcb fd
/// simply returns here.
fn poll_wait(conn: &i3_core::xcb_util::XConnection, ipc: &i3_core::ipc::IpcServer, log_socket: i32) {
    use std::os::unix::io::AsRawFd;

    let log_clients = i3_core::log::log_client_fds();
    let mut fds: Vec<libc::pollfd> =
        Vec::with_capacity(3 + ipc.clients.len() + log_clients.len());
    fds.push(libc::pollfd {
        // C: the xcb connection's fd (libev's ev_io on xcb_get_file_descriptor).
        fd: std::os::unix::io::AsRawFd::as_raw_fd(conn),
        events: libc::POLLIN,
        revents: 0,
    });
    fds.push(libc::pollfd {
        fd: ipc.listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    });
    for client in &ipc.clients {
        fds.push(libc::pollfd {
            fd: client.stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
    }
    // C: src/main.c:1024 the ev_io on the log stream listening socket (only
    // registered when create_socket() succeeded).
    if log_socket != -1 {
        fds.push(libc::pollfd {
            fd: log_socket,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    // C: src/log.c keeps the accepted log clients in a write-only list (no ev_io
    // for them); the port polls them as well so a client that disconnects is
    // noticed, and service_log_clients() drains them.
    for fd in log_clients {
        fds.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }

    // The ev_timer watchers (urgency hints, IPC kill timeouts) are serviced
    // once per wakeup; 10ms is the granularity the port had before.
    let timeout_ms: libc::c_int = 10;
    unsafe {
        libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms);
    }
}

/// C: src/main.c:893 xcb_query_extension + first_event — the event base of an
/// extension, or -1 when it is not present or the query fails.
fn extension_first_event(
    conn: &i3_core::xcb_util::XConnection,
    name: &'static str,
) -> i32 {
    // C: xcb_query_extension + xcb_get_extension_data(...)->first_event.
    //
    // x11rb's RequestConnection::extension_information() caches into a global
    // table and needs a blocking round trip; the direct qeury is used here so
    // the call has the same shape as xcb_query_extension (one request, one
    // reply) and cannot interact with the cached state.
    use x11rb::protocol::xproto::ConnectionExt as _;
    match conn.query_extension(name.as_bytes()) {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) if reply.present => i32::from(reply.first_event),
            _ => -1,
        },
        Err(_) => -1,
    }
}

/// C: the ev_io read watcher per IPC client (src/ipc.c:1444 ipc_receive_message
/// is wrapped by ipc::handle_readable).
fn service_ipc_clients(ipc: &mut IpcServer, t: &mut i3_core::Tree, backend: &mut dyn ConBackend) {
    let mut index = 0;
    while index < ipc.clients.len() {
        let alive = i3_core::ipc::handle_readable(ipc, index, t, backend);
        if alive {
            index += 1;
        }
        // When the client was freed, the next client shifted into this index.
    }
}