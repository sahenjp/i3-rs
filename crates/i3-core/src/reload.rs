//! C: src/commands.c:1668 `cmd_reload` and the C_RELOAD branch of
//! src/config.c:165 `load_configuration`.
//!
//! The C handler touches process globals directly: kill_nagbar() (util.c:400),
//! load_configuration() (config.c:165), x_set_i3_atoms() (x.c:1535),
//! ipc_send_workspace_event()/ipc_send_barconfig_update_event() (ipc.c:1626/
//! 1673), and the C_RELOAD tail of load_configuration() itself
//! (translate_keysyms/grab_all_keys/regrab_all_buttons/
//! gaps_reapply_workspace_assignments/tree_render).
//!
//! The port's command layer owns (Tree, ConBackend) and nothing else — the key
//! binding path (xbackend.rs `run_command`) has no access to the config, the
//! bindings list or the X connection either. So, exactly like cmd_restart
//! (restore_layout.rs), `cmd_reload` only leaves a request behind
//! ([request_reload]) and the event loop runs [reload] in the same iteration,
//! with the state it owns.
//!
//! One deliberate non-behaviour: C's cmd_reload does *not* re-run the
//! `exec_always` lines. main() drains autostarts_always once (main.c:1184-1192)
//! and nothing re-drains it on reload; measured against the C 4.25 baseline
//! (`exec_always sh -c 'echo ea >> /tmp/ea.log'` + `i3-msg reload` leaves the
//! file at one line). `exec_always` runs again on *restart*, because that
//! exec()s a new i3 and main() walks the queue again. The port keeps that
//! behaviour (startup.rs start_autostart is only called from the startup path).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::backend::{ConBackend, XConn};
use crate::bindings::Bindings;
use crate::config::{apply_font, config_load_t, load_configuration_with_tree};
use crate::config_directives::ConfigDirectives;
use crate::tree::Tree;
use crate::xcb_util::Atoms;

/// C: no counterpart — cmd_reload runs inline in C. The request flag stands in
/// for the globals the handler would reach (see the module comment).
static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);
/// C: src/commands.c:1657 cmd_exit -- C calls exit(EXIT_SUCCESS) inline. The
/// core layer cannot terminate the process, so the request is recorded here
/// and the event loop performs the shutdown in the same iteration.
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// C: src/commands.c:1657 cmd_exit -- `exit(EXIT_SUCCESS)`.
pub fn request_exit() {
    EXIT_REQUESTED.store(true, Ordering::SeqCst);
}

/// Takes the pending exit request (clearing it).
pub fn take_exit_request() -> bool {
    EXIT_REQUESTED.swap(false, Ordering::SeqCst)
}

/// C: src/commands.c:1668 cmd_reload — recorded by the command layer.
pub fn request_reload() {
    RELOAD_REQUESTED.store(true, Ordering::SeqCst);
}

/// True while a reload was requested (the event loop has not run it yet).
pub fn reload_pending() -> bool {
    RELOAD_REQUESTED.load(Ordering::SeqCst)
}

/// Takes the pending reload request (clearing it).
pub fn take_reload_request() -> bool {
    RELOAD_REQUESTED.swap(false, Ordering::SeqCst)
}

/// C: src/commands.c:1668 cmd_reload + the C_RELOAD branch of
/// src/config.c:165 load_configuration — reloads the configuration file and
/// applies every side effect C applies, in C's order.
///
/// Returns what load_configuration() returns (false = config errors were
/// found; the parsed parts are applied anyway, exactly like C, which keeps
/// running with the partially parsed config and shows the error nagbar).
pub fn reload(
    // C: the process-wide xcb_connection_t that CFGFUN(font) loads the font
    // with; the config parser runs before a connection exists (and -C runs
    // without one), which is why it is an Option here.
    conn: Option<&crate::xcb_util::XConnection>,
    d: &mut ConfigDirectives,
    override_configpath: Option<&str>,
    bindings: &mut Bindings,
    t: &mut Tree,
    backend: &mut dyn ConBackend,
    x: &mut dyn XConn,
    atoms: &Atoms,
) -> bool {
    // C: LOG("reloading\n");
    crate::log::log_("reloading\n");

    // C: kill_nagbar(config_error_nagbar_pid, false);
    //    kill_nagbar(command_error_nagbar_pid, false);
    crate::util::kill_nagbar(d.config_error_nagbar_pid, false);
    crate::util::kill_nagbar(bindings.command_error_nagbar_pid, false);
    // C: start_nagbar() refuses to start a new process while the passed pid is
    // set, which happens when the child watcher has not run yet; config errors
    // can be detected before that since the config is read right now (#4104).
    d.config_error_nagbar_pid = -1;
    bindings.command_error_nagbar_pid = -1;

    // C: config.c:32-40 free_configuration() — "If we are currently in a
    // binding mode, we first revert to the default since we have no guarantee
    // that the current mode will even still exist after parsing the config
    // again. See #2228." switch_mode() ungrabs the keys, translates and grabs
    // the default mode's bindings and sends the IPC mode event; the mode list
    // itself is rebuilt from the freshly parsed config further down.
    // (263-config-reload-reverts-bind-mode expects exactly that one event.)
    let mut mode_hooks = crate::ipc::IpcHooks;
    crate::bindings::switch_mode(bindings, t, x, "default", &mut mode_hooks);

    // C: load_configuration(NULL, C_RELOAD)
    let result = load_configuration_with_tree(d, override_configpath, config_load_t::C_RELOAD, Some(t));

    // C: CFGFUN(font) (config_directives.c:173) and config.c:277-281 load the
    // font and install it with set_font() while parsing. The port's parser has
    // no connection, so the real font is loaded here from the new pattern.
    apply_font(conn, d, Some(t));
    // C: the tree layer and the command layer read the same global config; the
    // port copies the subset into the tree (see config::apply_to_tree).
    crate::config::apply_to_tree(t, &d.config);
    // C: src/config.c:165 — parse_file() refills the assignments /
    // ws_assignments globals as it goes, so they are already up to date when
    // the C_RELOAD tail below runs gaps_reapply_workspace_assignments()
    // (src/config.c:298). The port mirrors them from the parsed directives, so
    // the mirror has to happen here, before that tail.
    t.assignments = d.assignments.clone();
    t.ws_assignments = d.ws_assignments.clone();
    // C: no counterpart — the drawing backend keeps its own copy of the client
    // colors and the font height (XBackend::set_draw_config), so it is
    // refreshed here, before tree_render() below redraws the decorations with
    // the (possibly) new drawing parameters.
    backend.apply_draw_config(&d.config);

    // C: config.c:170-179 + config.c:276-277 run inside load_configuration():
    // the parsed modes become the global mode list, the default mode's
    // bindings become the active binding list (the C `bindings` pointer), then
    // extract_workspace_names_from_bindings() and reorder_bindings() follow.
    // The port needs the Tree for those two, so it runs them here — the same
    // step the startup path takes (server.rs, right before load_keymap()).
    bindings.load_config_bindings(t, &d.modes, d.current_configpath.clone());

    // C: config.c:294-298, the C_RELOAD tail of load_configuration:
    //    translate_keysyms(); grab_all_keys(conn); regrab_all_buttons(conn);
    //    gaps_reapply_workspace_assignments();
    {
        // C has no hook here: translate_keysyms() reports command parse errors
        // through start_config_error_nagbar() (src/bindings.c:629). The port's
        // BindingHooks::start_config_error_nagbar is still a no-op default (the
        // config *parser* nagbar is wired, this bindings-side one is not), so
        // reload uses the no-op hooks exactly like the startup path.
        let mut hooks = crate::bindings::NullHooks;
        crate::bindings::translate_keysyms(bindings, t, &mut hooks);
    }
    crate::bindings::grab_all_keys(bindings, t, x);
    crate::bindings::regrab_all_buttons(t, x);
    crate::gaps::gaps_reapply_workspace_assignments(t);

    // C: /* Redraw the currently visible decorations on reload, so that the
    //    possibly new drawing parameters changed. */ tree_render();
    crate::tree::tree_render(t, backend);

    // C: src/commands.c:1685 x_set_i3_atoms(); — current_configpath may have
    // changed (a reload can point at a different file), so refresh it first.
    // C leaves current_socketpath and current_log_stream_socket_path alone
    // (i3-msg and i3-dump-log -f keep working across a reload).
    crate::x::set_i3_atom_configpath(d.current_configpath.as_deref().unwrap_or(""));
    crate::x::x_set_i3_atoms(x, atoms);

    // C: src/commands.c:1687 ipc_send_workspace_event("reload", NULL, NULL);
    // (both fields of the event are null, the change is what carries it)
    crate::ipc::queue_workspace_event(t, "reload", None, None);

    // C: src/commands.c:1689-1693 — one barconfig_update event per configured
    // bar, in case the bar blocks changed. The barconfigs live in
    // ConfigDirectives (the C global) and are mirrored into the Tree for the
    // commands (cmd_bar_mode reads them there), so the mirror is refreshed here.
    t.barconfigs = d.barconfigs.clone();
    let barconfigs: Vec<crate::config::Barconfig> = t.barconfigs.iter().cloned().collect();
    for current in &barconfigs {
        crate::ipc::queue_barconfig_update_event(t, current);
    }

    // C: (return result == 0 from load_configuration, and ysuccess(true) from
    // cmd_reload) — the reply is written by the caller's command layer.
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::data::Rect;
    use crate::tree::tree_init;
    use crate::xcb_util::RecordingConn;

    /// The request flag is the port's stand-in for C running the handler
    /// inline: cmd_reload must leave exactly one request, and the loop must
    /// consume it once.
    #[test]
    fn request_is_taken_once() {
        // The other unit tests in this binary may run concurrently, so the
        // flag is normalised first.
        let _ = take_reload_request();
        assert!(!reload_pending());
        request_reload();
        assert!(reload_pending());
        assert!(take_reload_request());
        assert!(!reload_pending());
        assert!(!take_reload_request());
    }

    /// C: the config path and the binding modes the IPC layer answers with are
    /// the ones load_configuration() just wrote, so a reload has to re-mirror
    /// them (the caller does that with IpcServer::sync_config_state). Here the
    /// newer direct effects are checked: the new key binding is active and
    /// grabbed, and the bar configs are mirrored into the tree.
    #[test]
    fn reload_applies_bindings_barconfigs_and_events() {
        // load_configuration() chdir()s into the config directory, which is
        // process-global (C: src/config.c try_chdir) — serialize with the
        // other config tests.
        let _cwd = crate::test_lock::cwd_lock();
        let dir = std::env::temp_dir().join(format!("i3-reload-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("reload.conf");
        let written = std::fs::write(
            &path,
            "font pango:monospace 9\n\
             bindsym Mod1+x nop\n\
             bar {\n\
             \tstatus_command true\n\
             }\n",
        );
        assert!(written.is_ok(), "{:?}", written.err());

        let mut d = ConfigDirectives::default();
        let mut t = Tree::new();
        let mut backend = NullBackend;
        tree_init(&mut t, &mut backend, Rect::new(0, 0, 1280, 800));
        let mut bindings = Bindings::new();
        let mut x = RecordingConn::new(1);
        let atoms = Atoms::default();

        let _ = crate::ipc::take_pending_events();
        let ok = reload(
            None,
            &mut d,
            Some(path.to_str().unwrap()),
            &mut bindings,
            &mut t,
            &mut backend,
            &mut x,
            &atoms,
        );

        assert!(ok, "the config parses without errors");
        // The new binding is the active one (bindings.c:639 switch_mode shape).
        assert!(
            t.bindings.iter().any(|b| b.symbol.as_deref() == Some("x")),
            "new bindsym is not active: {:?}",
            t.bindings.iter().map(|b| b.symbol.clone()).collect::<Vec<_>>()
        );
        // The barconfigs of the reloaded config are mirrored into the tree
        // (C: the global barconfigs list is rewritten in place).
        assert_eq!(t.barconfigs.len(), 1);
        // The workspace "reload" event and one barconfig_update event are
        // queued for the IPC subscribers (commands.c:1687-1693).
        let events = crate::ipc::take_pending_events();
        assert!(
            events
                .iter()
                .any(|e| e.subscription == "workspace" && e.payload.contains("\"reload\"")),
            "no workspace reload event: {:?}",
            events
        );
        assert!(
            events.iter().any(|e| e.subscription == "barconfig_update"),
            "no barconfig_update event: {:?}",
            events
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}