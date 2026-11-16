//! 1:1 port of src/startup.c: Startup notification code. Ensures a startup
//! notification context is set up when launching applications. We store the
//! current workspace to open windows in that startup notification context on
//! the appropriate workspace.
//!
//! C: src/startup.c (i3 4.25)
//!
//! libstartup-notification is not linked. SnLauncherContext is abstracted as an
//! OpaqueHandle and the libsn calls the C code makes are expressed by the
//! StartupNotifier trait; NullNotifier implements the same flow (context,
//! startup ID, refcounting, completion) without libsn, so the whole lifecycle
//! is testable. The 60 s ev_timer of start_application() is modelled by
//! StartupState::timeouts, which the event loop drains with
//! run_expired_timeouts().

// The C global/macro names are kept 1:1 (a__net_startup_id, …).
#![allow(non_snake_case)]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use crate::backend::{ConBackend, PropertyReply, XConn};
use crate::con::{con_by_window_id, con_get_workspace};
use crate::data::{Autostart, ConId, OpaqueHandle, Startup_Sequence, Window};
use crate::log::log_;
use crate::tree::Tree;
use crate::xcb_util::xcursor_cursor_t;
use libi3::log::{debuglog, errorlog};

/// C: src/startup.c:157 ev_timer_init(timeout, startup_timeout, 60.0, 0.)
pub const STARTUP_TIMEOUT_SECONDS: i64 = 60;
/// C: src/startup.c:247 sequence->delete_at = current_time + 30
pub const STARTUP_DELETE_DELAY_SECONDS: i64 = 30;
/// C: xcb/xcb.h XCB_NONE
pub const XCB_NONE: u32 = 0;
/// C: xcb/xcb.h XCB_GET_PROPERTY_TYPE_ANY
pub const XCB_GET_PROPERTY_TYPE_ANY: u32 = 0;
/// C: src/startup.c:310/378 get_property long_length
pub const STARTUP_ID_PROPERTY_LENGTH: u32 = 512;

/// C: libsn/sn-monitor.h SnMonitorEventType
pub const SN_MONITOR_EVENT_NONE: i32 = 0;
/// C: libsn/sn-monitor.h SnMonitorEventType
pub const SN_MONITOR_EVENT_STARTED: i32 = 1;
/// C: libsn/sn-monitor.h SnMonitorEventType
pub const SN_MONITOR_EVENT_COMPLETED: i32 = 2;
/// C: libsn/sn-monitor.h SnMonitorEventType
pub const SN_MONITOR_EVENT_CANCELED: i32 = 3;

/// The libstartup-notification API used by src/startup.c.
///
/// The context argument is the C SnLauncherContext *, abstracted as an
/// OpaqueHandle (include/data.h:287).
pub trait StartupNotifier {
    /// C: sn_launcher_context_new(sndisplay, conn_screen)
    fn sn_launcher_context_new(&mut self, screen: i32) -> OpaqueHandle;

    /// C: sn_launcher_context_set_name(context, "i3")
    fn sn_launcher_context_set_name(&mut self, context: OpaqueHandle, name: &str) {
        let _ = (context, name);
    }

    /// C: sn_launcher_context_set_description(context, "exec command in i3")
    fn sn_launcher_context_set_description(&mut self, context: OpaqueHandle, description: &str) {
        let _ = (context, description);
    }

    /// C: sn_launcher_context_initiate(context, "i3", first_word, last_timestamp)
    fn sn_launcher_context_initiate(
        &mut self,
        context: OpaqueHandle,
        name: &str,
        description: &str,
        timestamp: u32,
    ) {
        let _ = (context, name, description, timestamp);
    }

    /// C: sn_launcher_context_get_startup_id(context)
    fn sn_launcher_context_get_startup_id(&self, context: OpaqueHandle) -> String;

    /// C: sn_launcher_context_ref(context)
    fn sn_launcher_context_ref(&mut self, context: OpaqueHandle) {
        let _ = context;
    }

    /// C: sn_launcher_context_unref(context)
    fn sn_launcher_context_unref(&mut self, context: OpaqueHandle) {
        let _ = context;
    }

    /// C: sn_launcher_context_complete(context)
    fn sn_launcher_context_complete(&mut self, context: OpaqueHandle) {
        let _ = context;
    }

    /// C: sn_launcher_context_setup_child_process(context) — the environment
    /// variables the child process needs (DESKTOP_STARTUP_ID and friends).
    fn sn_launcher_context_setup_child_process(
        &mut self,
        context: OpaqueHandle,
    ) -> Vec<(String, String)>;
}

/// A libsn-free StartupNotifier: it produces the same startup IDs and
/// reference counts as libsn so the src/startup.c flow can run and be tested.
///
/// C: libsn/sn-launcher.c (the parts src/startup.c relies on)
#[derive(Debug)]
pub struct NullNotifier {
    /// The pid used in the generated startup IDs (libsn uses getpid()).
    pub pid: u32,
    next_handle: u64,
    next_serial: u32,
    /// C: the context's _NET_STARTUP_ID
    pub startup_ids: HashMap<u64, String>,
    /// C: sn_launcher_context_set_name
    pub names: HashMap<u64, String>,
    /// C: sn_launcher_context_set_description
    pub descriptions: HashMap<u64, String>,
    /// C: the context's refcount (starts at 1)
    pub refcounts: HashMap<u64, u32>,
    /// Contexts completed with sn_launcher_context_complete().
    pub completed: Vec<u64>,
    /// Every call in order, for tests.
    pub calls: Vec<String>,
}

impl Default for NullNotifier {
    fn default() -> Self {
        NullNotifier::new(std::process::id())
    }
}

impl NullNotifier {
    pub fn new(pid: u32) -> NullNotifier {
        NullNotifier {
            pid,
            next_handle: 0,
            next_serial: 0,
            startup_ids: HashMap::new(),
            names: HashMap::new(),
            descriptions: HashMap::new(),
            refcounts: HashMap::new(),
            completed: Vec::new(),
            calls: Vec::new(),
        }
    }

    /// The refcount of the context (0 for an unknown handle).
    pub fn refcount(&self, context: OpaqueHandle) -> u32 {
        self.refcounts.get(&context.handle).copied().unwrap_or(0)
    }
}

impl StartupNotifier for NullNotifier {
    fn sn_launcher_context_new(&mut self, screen: i32) -> OpaqueHandle {
        self.next_handle += 1;
        self.next_serial += 1;
        let context = OpaqueHandle {
            handle: self.next_handle,
        };
        /* libsn: "<binary>-<pid>-<serial>-TIME<timestamp>" */
        let startup_id = format!("i3-{}-{}-TIME0", self.pid, self.next_serial);
        self.startup_ids.insert(context.handle, startup_id);
        self.refcounts.insert(context.handle, 1);
        self.calls.push(format!("new(screen={})", screen));
        context
    }

    fn sn_launcher_context_set_name(&mut self, context: OpaqueHandle, name: &str) {
        self.names.insert(context.handle, name.to_string());
        self.calls.push(format!("set_name({})", name));
    }

    fn sn_launcher_context_set_description(&mut self, context: OpaqueHandle, description: &str) {
        self.descriptions
            .insert(context.handle, description.to_string());
        self.calls.push(format!("set_description({})", description));
    }

    fn sn_launcher_context_initiate(
        &mut self,
        context: OpaqueHandle,
        name: &str,
        description: &str,
        timestamp: u32,
    ) {
        self.calls.push(format!(
            "initiate(name={}, description={}, timestamp={})",
            name, description, timestamp
        ));
        self.startup_ids.insert(
            context.handle,
            format!(
                "i3-{}-{}-TIME{}",
                self.pid,
                self.next_serial,
                timestamp
            ),
        );
    }

    fn sn_launcher_context_get_startup_id(&self, context: OpaqueHandle) -> String {
        self.startup_ids
            .get(&context.handle)
            .cloned()
            .unwrap_or_default()
    }

    fn sn_launcher_context_ref(&mut self, context: OpaqueHandle) {
        *self.refcounts.entry(context.handle).or_insert(1) += 1;
        self.calls.push("ref".to_string());
    }

    fn sn_launcher_context_unref(&mut self, context: OpaqueHandle) {
        let refcount = self.refcounts.entry(context.handle).or_insert(1);
        *refcount = refcount.saturating_sub(1);
        self.calls.push("unref".to_string());
    }

    fn sn_launcher_context_complete(&mut self, context: OpaqueHandle) {
        self.completed.push(context.handle);
        self.calls.push("complete".to_string());
    }

    fn sn_launcher_context_setup_child_process(
        &mut self,
        context: OpaqueHandle,
    ) -> Vec<(String, String)> {
        let startup_id = self.sn_launcher_context_get_startup_id(context);
        self.calls.push("setup_child_process".to_string());
        vec![("DESKTOP_STARTUP_ID".to_string(), startup_id)]
    }
}

/// A shared handle to a StartupNotifier, so a test can inspect the notifier
/// state while StartupState owns it as a trait object.
impl<T: StartupNotifier + ?Sized> StartupNotifier for Rc<RefCell<T>> {
    fn sn_launcher_context_new(&mut self, screen: i32) -> OpaqueHandle {
        self.borrow_mut().sn_launcher_context_new(screen)
    }

    fn sn_launcher_context_set_name(&mut self, context: OpaqueHandle, name: &str) {
        self.borrow_mut().sn_launcher_context_set_name(context, name)
    }

    fn sn_launcher_context_set_description(&mut self, context: OpaqueHandle, description: &str) {
        self.borrow_mut()
            .sn_launcher_context_set_description(context, description)
    }

    fn sn_launcher_context_initiate(
        &mut self,
        context: OpaqueHandle,
        name: &str,
        description: &str,
        timestamp: u32,
    ) {
        self.borrow_mut()
            .sn_launcher_context_initiate(context, name, description, timestamp)
    }

    fn sn_launcher_context_get_startup_id(&self, context: OpaqueHandle) -> String {
        self.borrow().sn_launcher_context_get_startup_id(context)
    }

    fn sn_launcher_context_ref(&mut self, context: OpaqueHandle) {
        self.borrow_mut().sn_launcher_context_ref(context)
    }

    fn sn_launcher_context_unref(&mut self, context: OpaqueHandle) {
        self.borrow_mut().sn_launcher_context_unref(context)
    }

    fn sn_launcher_context_complete(&mut self, context: OpaqueHandle) {
        self.borrow_mut().sn_launcher_context_complete(context)
    }

    fn sn_launcher_context_setup_child_process(
        &mut self,
        context: OpaqueHandle,
    ) -> Vec<(String, String)> {
        self.borrow_mut()
            .sn_launcher_context_setup_child_process(context)
    }
}

/// The fork()/execl() part of start_application().
///
/// C: src/startup.c:179-209
pub trait ApplicationExec {
    /// Starts command through /bin/sh with the given environment and returns
    /// the child pid.
    fn spawn(&mut self, command: &str, env: &[(String, String)]) -> i32;
}

/// Spawns the command through the system's bourne shell like C does.
///
/// C: execl(_PATH_BSHELL, _PATH_BSHELL, "-c", command, NULL). The child-side
/// setsid()/setrlimit()/close() calls are OS-level child setup and are a TODO
/// of the exec layer; the command and its environment are identical.
#[derive(Debug, Default)]
pub struct CommandExec;

impl ApplicationExec for CommandExec {
    fn spawn(&mut self, command: &str, env: &[(String, String)]) -> i32 {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        for (key, value) in env {
            cmd.env(key, value);
        }
        match cmd.spawn() {
            Ok(child) => child.id() as i32,
            Err(err) => {
                errorlog(&format!("Could not spawn \"{}\": {}\n", command, err));
                -1
            }
        }
    }
}

/// Test double recording the spawned commands.
///
/// C: src/startup.c:179 (fork())
#[derive(Debug, Default)]
pub struct RecordingExec {
    pub spawned: Vec<(String, Vec<(String, String)>)>,
    pub next_pid: i32,
}

impl ApplicationExec for RecordingExec {
    fn spawn(&mut self, command: &str, env: &[(String, String)]) -> i32 {
        self.spawned.push((command.to_string(), env.to_vec()));
        self.next_pid += 1;
        1000 + self.next_pid
    }
}

/// C: src/startup.c:156 ev_timer *timeout (ev_timer_init(…, 60.0, 0.))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupTimeout {
    /// C: timeout->data = context;
    pub context: OpaqueHandle,
    /// The time(NULL) at which the timer fires.
    pub due: i64,
}

/// The state of src/startup.c: the startup_sequences TAILQ plus the libsn
/// client and the timers the event loop owns.
pub struct StartupState {
    /// C: src/startup.c:24 static TAILQ_HEAD(startup_sequence_head,
    /// Startup_Sequence) startup_sequences
    pub startup_sequences: VecDeque<Startup_Sequence>,
    /// C: the libsn display/contexts
    pub notifier: Box<dyn StartupNotifier>,
    /// C: time(NULL) — injected so the 30 s/60 s windows are testable.
    pub now: i64,
    /// C: the ev_timer objects started in start_application()
    pub timeouts: Vec<StartupTimeout>,
}

impl Default for StartupState {
    fn default() -> Self {
        StartupState::new()
    }
}

impl StartupState {
    pub fn new() -> StartupState {
        StartupState::with_notifier(Box::new(NullNotifier::default()))
    }

    pub fn with_notifier(notifier: Box<dyn StartupNotifier>) -> StartupState {
        StartupState {
            startup_sequences: VecDeque::new(),
            notifier,
            now: 0,
            timeouts: Vec::new(),
        }
    }
}

/// C: src/startup.c:34 startup_timeout — after 60 seconds, a timeout is
/// triggered for each startup sequence; the timeout just triggers completion
/// of the sequence, so the normal completion process takes place.
///
/// C receives the ev_timer and gets the context from w->data; the libsn
/// monitor callback is invoked by libsn after complete(), here
/// startup_monitor_event() is called directly with the completion event.
pub fn startup_timeout(s: &mut StartupState, context: OpaqueHandle, x: &mut dyn XConn) {
    let id = s.notifier.sn_launcher_context_get_startup_id(context);
    debuglog(&format!("Timeout for startup sequence {}\n", id));

    let sequence = s.startup_sequences.iter().position(|c| c.id == id);

    /* Unref the context (for the timeout itself, see start_application) */
    s.notifier.sn_launcher_context_unref(context);

    if sequence.is_none() {
        debuglog("Sequence already deleted, nevermind.\n");
        return;
    }

    /* Complete the startup sequence, will trigger its deletion. */
    s.notifier.sn_launcher_context_complete(context);
    startup_monitor_event(s, SN_MONITOR_EVENT_COMPLETED, &id, x);
}

/// The event-loop hook for the ev_timer: fires every timeout whose 60 s have
/// elapsed (C: libev invokes startup_timeout).
pub fn run_expired_timeouts(s: &mut StartupState, x: &mut dyn XConn) {
    let now = s.now;
    let due: Vec<OpaqueHandle> = s
        .timeouts
        .iter()
        .filter(|timeout| timeout.due <= now)
        .map(|timeout| timeout.context)
        .collect();
    for context in due {
        s.timeouts.retain(|timeout| timeout.context != context);
        startup_timeout(s, context, x);
    }
}

/// C: src/startup.c:73 _prune_startup_sequences — returns the number of
/// active (!) startup notifications, that is, those which are not marked for
/// deletion yet.
pub fn _prune_startup_sequences(s: &mut StartupState) -> i32 {
    let current_time = s.now;
    let mut active_sequences = 0;

    /* Traverse the list and delete everything which was marked for deletion 30
     * seconds ago or earlier. */
    let mut index = 0;
    while index < s.startup_sequences.len() {
        let delete_at = s.startup_sequences[index].delete_at;

        if delete_at == 0 {
            active_sequences += 1;
            index += 1;
            continue;
        }

        if current_time <= delete_at {
            index += 1;
            continue;
        }

        startup_sequence_delete(s, index);
    }

    active_sequences
}

/// C: src/startup.c:105 startup_sequence_delete — deletes a startup sequence,
/// ignoring whether its timeout has elapsed.
///
/// C returns void and frees the sequence; Rust returns the removed value (the
/// caller can drop it; the workspace is also needed by
/// startup_workspace_for_window before the delete).
pub fn startup_sequence_delete(s: &mut StartupState, index: usize) -> Startup_Sequence {
    let sequence = s
        .startup_sequences
        .remove(index)
        .expect("startup sequence index out of bounds");
    debuglog(&format!(
        "Deleting startup sequence {}, delete_at = {}, current_time = {}\n",
        sequence.id, sequence.delete_at, s.now
    ));

    /* Unref the context, will be free()d */
    s.notifier.sn_launcher_context_unref(sequence.context);

    sequence
}

/// C: src/startup.c:136 start_application — starts the given application by
/// passing it through a shell. The no_startup_id flag determines whether a
/// startup notification context (and ID) should be created. The window_id, if
/// given, will be provided to the child process via the I3_WINDOW_ID
/// environment variable.
///
/// The C globals sndisplay/conn_screen/last_timestamp/current_socketpath and
/// focused are passed in.
#[allow(clippy::too_many_arguments)]
pub fn start_application(
    s: &mut StartupState,
    x: &mut dyn XConn,
    exec: &mut dyn ApplicationExec,
    t: &Tree,
    focused: Option<ConId>,
    command: &str,
    no_startup_id: bool,
    window_id: u32,
    screen: i32,
    last_timestamp: u32,
    current_socketpath: &str,
) {
    let mut context = OpaqueHandle::default();

    if !no_startup_id {
        /* Create a startup notification context to monitor the progress of
         * this startup. */
        context = s.notifier.sn_launcher_context_new(screen);
        s.notifier.sn_launcher_context_set_name(context, "i3");
        s.notifier
            .sn_launcher_context_set_description(context, "exec command in i3");
        /* Chop off everything starting from the first space (if there are any
         * spaces in the command), since we don't want the parameters. */
        let first_word = command.split(' ').next().unwrap_or("").to_string();
        s.notifier
            .sn_launcher_context_initiate(context, "i3", &first_word, last_timestamp);

        /* Trigger a timeout after 60 seconds */
        let due = s.now + STARTUP_TIMEOUT_SECONDS;
        s.timeouts.push(StartupTimeout { context, due });

        debuglog(&format!(
            "startup id = {}\n",
            s.notifier.sn_launcher_context_get_startup_id(context)
        ));

        /* Save the ID and current workspace in our internal list of startup
         * sequences */
        let workspace = focused
            .and_then(|focused| con_get_workspace(t, focused))
            .map(|ws| t.cons[ws].name.clone().unwrap_or_default());
        let id = s.notifier.sn_launcher_context_get_startup_id(context);
        s.startup_sequences.push_back(Startup_Sequence {
            id,
            workspace,
            context,
            delete_at: 0,
        });

        /* Increase the refcount once (it starts with 1, so it will be 2 now)
         * for the timeout. Even if the sequence gets completed, the timeout
         * still needs the context (but will unref it then) */
        s.notifier.sn_launcher_context_ref(context);
    }

    debuglog(&format!("executing: {}\n", command));

    /* Child process setup (C: the fork() child block).
     *
     * C closes the socket activation file descriptors and unsets LISTEN_PID /
     * LISTEN_FDS, resets SIGPIPE and calls setsid()/setrlimit(); that is the
     * exec layer's job (see CommandExec). */
    let mut env: Vec<(String, String)> = Vec::new();
    if !no_startup_id {
        env.extend(s.notifier.sn_launcher_context_setup_child_process(context));
    }
    env.push(("I3SOCK".to_string(), current_socketpath.to_string()));

    if window_id != XCB_NONE {
        env.push(("I3_WINDOW_ID".to_string(), window_id.to_string()));
    }

    exec.spawn(command, &env);

    if !no_startup_id {
        /* Change the pointer of the root window to indicate progress */
        x.set_root_cursor(xcursor_cursor_t::XCURSOR_CURSOR_WATCH as u16);
    }
}

/// C: src/main.c:1169-1196 — the autostart block of main(): "Autostarting
/// exec-lines" drains the `autostarts` TAILQ (skipped entirely with -a) and
/// "Autostarting exec_always-lines" always drains `autostarts_always`. Each
/// entry is started with window_id XCB_NONE and removed from the queue, so a
/// `reload` that re-reads the config starts the exec_always lines again
/// (`-a` only suppresses the plain `exec` lines).
///
/// The queues live in ConfigDirectives (C: the globals of src/main.c:90/93
/// filled by CFGFUN(exec)); the backend hook is start_application().

/// C: src/main.c:1195-1205 — start one i3bar per configured `bar {}` block.
///
/// The command is built exactly like C:
///   `%s %s --bar_id=%s --socket="%s"`
/// where the first `%s` is `i3bar_command` or the default `exec i3bar`, and the
/// second is `-V` when `verbose` is set.
pub fn start_bars(
    t: &Tree,
    backend: &mut dyn ConBackend,
    barconfigs: &VecDeque<crate::config::Barconfig>,
    socketpath: &str,
) {
    for barconfig in barconfigs {
        // C: barconfig->i3bar_command ? barconfig->i3bar_command : "exec i3bar"
        let base = barconfig
            .i3bar_command
            .clone()
            .unwrap_or_else(|| "exec i3bar".to_string());
        let verbose = if barconfig.verbose { "-V" } else { "" };
        let command = format!(
            "{} {} --bar_id={} --socket=\"{}\"",
            base,
            verbose,
            barconfig.id.as_deref().unwrap_or(""),
            socketpath
        );
        // C: LOG("Starting bar process: %s\n", command);
        log_(&format!("Starting bar process: {}\n", command));
        // C: start_application(command, true, XCB_NONE);
        backend.start_application(t, &command, true, XCB_NONE);
    }
}

pub fn start_autostart(
    t: &Tree,
    backend: &mut dyn ConBackend,
    autostarts: &mut VecDeque<Autostart>,
    autostarts_always: &mut VecDeque<Autostart>,
    autostart: bool,
) {
    if autostart {
        while let Some(exec) = autostarts.pop_front() {
            // C: LOG("auto-starting %s\n", exec->command);
            log_(&format!("auto-starting {}\n", exec.command));
            // C: start_application(exec->command, exec->no_startup_id, XCB_NONE);
            backend.start_application(t, &exec.command, exec.no_startup_id, XCB_NONE);
        }
    }

    while let Some(exec_always) = autostarts_always.pop_front() {
        // C: LOG("auto-starting (always!) %s\n", exec_always->command);
        log_(&format!("auto-starting (always!) {}\n", exec_always.command));
        backend.start_application(
            t,
            &exec_always.command,
            exec_always.no_startup_id,
            XCB_NONE,
        );
    }
}

/// C: src/startup.c:221 startup_monitor_event — called by
/// libstartup-notification when something happens.
///
/// C receives an SnMonitorEvent; Rust receives its type and the sequence ID
/// (sn_monitor_event_get_type / sn_startup_sequence_get_id).
pub fn startup_monitor_event(s: &mut StartupState, event_type: i32, id: &str, x: &mut dyn XConn) {
    /* Get the corresponding internal startup sequence */
    let index = match s.startup_sequences.iter().position(|c| c.id == id) {
        Some(index) => index,
        None => {
            debuglog(&format!(
                "Got event for startup sequence that we did not initiate (ID = {}). Ignoring.\n",
                id
            ));
            return;
        }
    };

    match event_type {
        SN_MONITOR_EVENT_COMPLETED => {
            debuglog(&format!("startup sequence {} completed\n", id));

            /* Mark the given sequence for deletion in 30 seconds. */
            let delete_at = s.now + STARTUP_DELETE_DELAY_SECONDS;
            s.startup_sequences[index].delete_at = delete_at;
            debuglog(&format!(
                "Will delete startup sequence {} at timestamp {}\n",
                id, delete_at
            ));

            if _prune_startup_sequences(s) == 0 {
                debuglog(
                    "No more startup sequences running, changing root window cursor to default pointer.\n",
                );
                /* Change the pointer of the root window to indicate progress */
                x.set_root_cursor(xcursor_cursor_t::XCURSOR_CURSOR_POINTER as u16);
            }
        }
        _ => {
            /* ignore */
        }
    }
}

/// C: src/startup.c:267 startup_sequence_rename_workspace — renames workspaces
/// that are mentioned in the startup sequences.
pub fn startup_sequence_rename_workspace(s: &mut StartupState, old_name: &str, new_name: &str) {
    for current in &mut s.startup_sequences {
        if current.workspace.as_deref() != Some(old_name) {
            continue;
        }
        debuglog(&format!(
            "Renaming workspace \"{}\" to \"{}\" in startup sequence {}.\n",
            old_name, new_name, current.id
        ));
        current.workspace = Some(new_name.to_string());
    }
}

/// C: src/startup.c:284 startup_sequence_get — gets the stored startup
/// sequence for the _NET_STARTUP_ID of a given window. Returns the index of
/// the sequence in StartupState::startup_sequences.
pub fn startup_sequence_get(
    s: &StartupState,
    t: &Tree,
    cwindow: &Window,
    startup_id_reply: Option<PropertyReply>,
    ignore_mapped_leader: bool,
    a__net_startup_id: u32,
    x: &mut dyn XConn,
) -> Option<usize> {
    /* The _NET_STARTUP_ID is only needed during this function, so we get it
     * here and don't save it in the 'cwindow'. */
    let mut reply = startup_id_reply;

    if reply.as_ref().map(|r| r.value_len()).unwrap_or(0) == 0 {
        debuglog(&format!(
            "No _NET_STARTUP_ID set on window 0x{:08x}\n",
            cwindow.id
        ));
        if cwindow.leader == XCB_NONE {
            return None;
        }

        /* This is a special case that causes the leader's startup sequence to
         * only be returned if it has never been mapped, useful primarily when
         * trying to delete a sequence.
         *
         * It's generally inappropriate to delete a leader's sequence when
         * moving a child window, but if the leader has no container, it's
         * likely permanently unmapped and the child is the "real" window. */
        if ignore_mapped_leader && con_by_window_id(t, cwindow.leader).is_some() {
            debuglog(&format!("Ignoring leader window 0x{:08x}\n", cwindow.leader));
            return None;
        }

        debuglog(&format!("Checking leader window 0x{:08x}\n", cwindow.leader));

        reply = x.get_property(
            cwindow.leader,
            a__net_startup_id,
            XCB_GET_PROPERTY_TYPE_ANY,
            0,
            STARTUP_ID_PROPERTY_LENGTH,
        );

        if reply.as_ref().map(|r| r.value_len()).unwrap_or(0) == 0 {
            debuglog("No _NET_STARTUP_ID set on the leader either\n");
            return None;
        }
    }

    let startup_id = match reply {
        Some(reply) => String::from_utf8_lossy(&reply.value)
            .split('\u{0}')
            .next()
            .unwrap_or("")
            .to_string(),
        None => return None,
    };

    match s.startup_sequences.iter().position(|c| c.id == startup_id) {
        Some(index) => Some(index),
        None => {
            debuglog(&format!(
                "WARNING: This sequence (ID {}) was not found\n",
                startup_id
            ));
            None
        }
    }
}

/// C: src/startup.c:356 startup_workspace_for_window — checks if the given
/// window belongs to a startup notification by checking if the
/// _NET_STARTUP_ID property is set on the window (or on its leader, if it's
/// unset). If so, returns the workspace on which the startup was initiated.
pub fn startup_workspace_for_window(
    s: &mut StartupState,
    t: &Tree,
    cwindow: &Window,
    startup_id_reply: Option<PropertyReply>,
    a__net_startup_id: u32,
    x: &mut dyn XConn,
) -> Option<String> {
    let index = startup_sequence_get(s, t, cwindow, startup_id_reply, false, a__net_startup_id, x)?;

    /* If the startup sequence's time span has elapsed, delete it. */
    let current_time = s.now;
    if s.startup_sequences[index].delete_at > 0
        && current_time > s.startup_sequences[index].delete_at
    {
        debuglog(&format!(
            "Deleting expired startup sequence {}\n",
            s.startup_sequences[index].id
        ));
        startup_sequence_delete(s, index);
        return None;
    }

    s.startup_sequences[index].workspace.clone()
}

/// C: src/startup.c:377 startup_sequence_delete_by_window — deletes the
/// startup sequence for a window if it exists.
pub fn startup_sequence_delete_by_window(
    s: &mut StartupState,
    t: &Tree,
    win: &Window,
    a__net_startup_id: u32,
    x: &mut dyn XConn,
) {
    let startup_id_reply =
        x.get_property(win.id, a__net_startup_id, XCB_GET_PROPERTY_TYPE_ANY, 0, STARTUP_ID_PROPERTY_LENGTH);

    if let Some(index) = startup_sequence_get(s, t, win, startup_id_reply, true, a__net_startup_id, x) {
        startup_sequence_delete(s, index);
    }
}

/******************************************************************************
 * Tests
 *****************************************************************************/

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::ConType;
    use crate::xcb_util::RecordingConn;

    fn workspace_tree() -> (Tree, ConId) {
        let mut t = Tree::new();
        let root = t.cons.insert(crate::data::Con {
            r#type: ConType::CT_ROOT,
            ..Default::default()
        });
        t.croot = Some(root);
        let ws = t.cons.insert(crate::data::Con {
            r#type: ConType::CT_WORKSPACE,
            name: Some("1".to_string()),
            parent: Some(root),
            ..Default::default()
        });
        t.cons[root].nodes_head.push_back(ws);
        t.focused = Some(ws);
        (t, ws)
    }

    /// A StartupState plus the shared NullNotifier so tests can inspect the
    /// libsn-side bookkeeping.
    fn state_with_null_notifier() -> (StartupState, Rc<RefCell<NullNotifier>>) {
        let notifier = Rc::new(RefCell::new(NullNotifier::new(4242)));
        (
            StartupState::with_notifier(Box::new(notifier.clone())),
            notifier,
        )
    }

    /// C: start_application creates the sequence, refs the context for the
    /// timeout, sets the child environment and switches the root cursor.
    #[test]
    fn start_application_creates_sequence_and_environment() {
        let (t, ws) = workspace_tree();
        let (mut s, notifier) = state_with_null_notifier();
        let mut x = RecordingConn::new(1);
        let mut exec = RecordingExec::default();

        start_application(
            &mut s,
            &mut x,
            &mut exec,
            &t,
            Some(ws),
            "firefox --new-window https://example.org",
            false,
            0x99,
            0,
            123,
            "/run/user/1000/i3/ipc-socket.1",
        );

        assert_eq!(s.startup_sequences.len(), 1);
        let sequence = &s.startup_sequences[0];
        assert!(sequence.id.starts_with("i3-4242-"));
        assert_eq!(sequence.workspace.as_deref(), Some("1"));
        assert_eq!(sequence.delete_at, 0);
        // One ref for the sequence, one for the 60 s timeout.
        assert_eq!(notifier.borrow().refcount(sequence.context), 2);

        // The timeout is registered for now + 60 s.
        assert_eq!(s.timeouts.len(), 1);
        assert_eq!(s.timeouts[0].due, STARTUP_TIMEOUT_SECONDS);

        // The shell gets the command and the i3 environment.
        assert_eq!(exec.spawned.len(), 1);
        let (command, env) = &exec.spawned[0];
        assert_eq!(command, "firefox --new-window https://example.org");
        let env_map: HashMap<&str, &str> = env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            env_map.get("DESKTOP_STARTUP_ID").copied(),
            Some(sequence.id.as_str())
        );
        assert_eq!(
            env_map.get("I3SOCK").copied(),
            Some("/run/user/1000/i3/ipc-socket.1")
        );
        assert_eq!(env_map.get("I3_WINDOW_ID").copied(), Some("153"));

        // The root cursor shows the progress indicator.
        assert_eq!(
            x.log.last().unwrap().as_str(),
            format!("set_root_cursor({})", xcursor_cursor_t::XCURSOR_CURSOR_WATCH as u16)
        );
    }

    /// C: no_startup_id skips the context, the timer and the cursor change but
    /// still spawns the command.
    #[test]
    fn start_application_without_startup_id() {
        let (t, ws) = workspace_tree();
        let (mut s, notifier) = state_with_null_notifier();
        let mut x = RecordingConn::new(1);
        let mut exec = RecordingExec::default();

        start_application(
            &mut s,
            &mut x,
            &mut exec,
            &t,
            Some(ws),
            "xterm",
            true,
            XCB_NONE,
            0,
            0,
            "/tmp/i3.sock",
        );

        assert!(s.startup_sequences.is_empty());
        assert!(s.timeouts.is_empty());
        assert!(x.log.is_empty());
        assert_eq!(exec.spawned.len(), 1);
        let env = &exec.spawned[0].1;
        assert!(env.iter().all(|(k, _)| k != "DESKTOP_STARTUP_ID"));
        assert!(env.iter().all(|(k, _)| k != "I3_WINDOW_ID"));
        assert_eq!(env[0].0, "I3SOCK");
    }

    /// C: startup_timeout completes the sequence, the completion marks it for
    /// deletion in 30 s and the last completion resets the root cursor.
    #[test]
    fn startup_sequence_lifecycle() {
        let (t, ws) = workspace_tree();
        let (mut s, notifier) = state_with_null_notifier();
        let mut x = RecordingConn::new(1);
        let mut exec = RecordingExec::default();
        start_application(&mut s, &mut x, &mut exec, &t, Some(ws), "xterm", false, 0, 0, 0, "/tmp/i3.sock");
        let context = s.startup_sequences[0].context;
        let id = s.startup_sequences[0].id.clone();

        // 60 seconds later the ev_timer fires.
        s.now = STARTUP_TIMEOUT_SECONDS;
        run_expired_timeouts(&mut s, &mut x);
        assert!(s.timeouts.is_empty());
        // The timeout unref'd its reference; the sequence still holds one.
        assert_eq!(notifier.borrow().refcount(context), 1);
        assert!(notifier.borrow().completed.contains(&context.handle));
        assert_eq!(
            s.startup_sequences[0].delete_at,
            STARTUP_TIMEOUT_SECONDS + STARTUP_DELETE_DELAY_SECONDS
        );
        assert_eq!(
            x.log.last().unwrap().as_str(),
            format!("set_root_cursor({})", xcursor_cursor_t::XCURSOR_CURSOR_POINTER as u16)
        );

        // The sequence is still around (Firefox-style early completion).
        assert_eq!(_prune_startup_sequences(&mut s), 0);
        assert_eq!(s.startup_sequences.len(), 1);

        // After the 30 s grace period it is pruned.
        s.now = STARTUP_TIMEOUT_SECONDS + STARTUP_DELETE_DELAY_SECONDS + 1;
        assert_eq!(_prune_startup_sequences(&mut s), 0);
        assert!(s.startup_sequences.is_empty());
        assert_eq!(notifier.borrow().refcount(context), 0);

        // A second timeout for the same (deleted) sequence is a no-op.
        startup_timeout(&mut s, context, &mut x);
        assert!(s.startup_sequences.is_empty());
        let _ = id;
    }

    /// C: startup_workspace_for_window returns the workspace of the sequence
    /// and deletes expired sequences.
    #[test]
    fn workspace_for_window_and_expiry() {
        let (t, ws) = workspace_tree();
        let (mut s, notifier) = state_with_null_notifier();
        let mut x = RecordingConn::new(1);
        let mut exec = RecordingExec::default();
        start_application(&mut s, &mut x, &mut exec, &t, Some(ws), "xterm", false, 0, 0, 0, "/tmp/i3.sock");
        let startup_id = s.startup_sequences[0].id.clone();
        let atom = 77u32;

        let window = Window {
            id: 0x100,
            ..Default::default()
        };
        let reply = Some(PropertyReply {
            type_: 0,
            format: 8,
            value: startup_id.bytes().chain(std::iter::once(0)).collect(),
        });
        assert_eq!(
            startup_workspace_for_window(&mut s, &t, &window, reply.clone(), atom, &mut x),
            Some("1".to_string())
        );

        // Mark it completed and expire it.
        s.now = 100;
        startup_monitor_event(&mut s, SN_MONITOR_EVENT_COMPLETED, &startup_id, &mut x);
        assert_eq!(s.startup_sequences[0].delete_at, 130);
        s.now = 131;
        assert_eq!(
            startup_workspace_for_window(&mut s, &t, &window, reply, atom, &mut x),
            None
        );
        assert!(s.startup_sequences.is_empty());
    }

    /// C: startup_sequence_get follows the leader window when the window
    /// itself has no _NET_STARTUP_ID, and ignores a mapped leader when asked to.
    #[test]
    fn sequence_get_follows_leader() {
        let (mut t, ws) = workspace_tree();
        let (mut s, notifier) = state_with_null_notifier();
        let mut x = RecordingConn::new(1);
        let mut exec = RecordingExec::default();
        start_application(&mut s, &mut x, &mut exec, &t, Some(ws), "xterm", false, 0, 0, 0, "/tmp/i3.sock");
        let startup_id = s.startup_sequences[0].id.clone();
        let atom = 88u32;
        x.props.insert(
            (0x200, atom),
            PropertyReply {
                type_: 0,
                format: 8,
                value: startup_id.bytes().chain(std::iter::once(0)).collect(),
            },
        );

        let window = Window {
            id: 0x100,
            leader: 0x200,
            ..Default::default()
        };
        assert_eq!(
            startup_sequence_get(&s, &t, &window, None, false, atom, &mut x),
            Some(0)
        );

        // A mapped leader is ignored with ignore_mapped_leader == true.
        let window_id = t.windows.insert(Window {
            id: 0x200,
            ..Default::default()
        });
        let leader_con = t.cons.insert(crate::data::Con {
            window: Some(window_id),
            ..Default::default()
        });
        t.all_cons.push_back(leader_con);
        // con_by_window_id looks the window up through the index.
        t.window_index.insert(0x200, window_id);
        assert_eq!(
            startup_sequence_get(&s, &t, &window, None, true, atom, &mut x),
            None
        );

        // A window without leader and without property has no sequence.
        let plain = Window {
            id: 0x300,
            ..Default::default()
        };
        assert_eq!(
            startup_sequence_get(&s, &t, &plain, None, false, atom, &mut x),
            None
        );
    }

    /// C: startup_sequence_delete_by_window removes the sequence of a window.
    #[test]
    fn delete_by_window_removes_sequence() {
        let (t, ws) = workspace_tree();
        let (mut s, notifier) = state_with_null_notifier();
        let mut x = RecordingConn::new(1);
        let mut exec = RecordingExec::default();
        start_application(&mut s, &mut x, &mut exec, &t, Some(ws), "xterm", false, 0, 0, 0, "/tmp/i3.sock");
        let startup_id = s.startup_sequences[0].id.clone();
        let atom = 99u32;
        x.props.insert(
            (0x100, atom),
            PropertyReply {
                type_: 0,
                format: 8,
                value: startup_id.bytes().chain(std::iter::once(0)).collect(),
            },
        );

        let window = Window {
            id: 0x100,
            ..Default::default()
        };
        startup_sequence_delete_by_window(&mut s, &t, &window, atom, &mut x);
        assert!(s.startup_sequences.is_empty());
    }

    /// C: startup_sequence_rename_workspace follows a workspace rename.
    #[test]
    fn rename_workspace_in_sequences() {
        let (t, ws) = workspace_tree();
        let (mut s, notifier) = state_with_null_notifier();
        let mut x = RecordingConn::new(1);
        let mut exec = RecordingExec::default();
        start_application(&mut s, &mut x, &mut exec, &t, Some(ws), "xterm", false, 0, 0, 0, "/tmp/i3.sock");

        startup_sequence_rename_workspace(&mut s, "1", "work");
        assert_eq!(s.startup_sequences[0].workspace.as_deref(), Some("work"));
        startup_sequence_rename_workspace(&mut s, "other", "nope");
        assert_eq!(s.startup_sequences[0].workspace.as_deref(), Some("work"));
    }

    /// Records the exec hook (C: start_application's fork/exec child block).
    #[derive(Default)]
    struct AutostartRecorder {
        started: Vec<(String, bool, u32)>,
    }

    impl ConBackend for AutostartRecorder {
        fn start_application(
            &mut self,
            _t: &Tree,
            command: &str,
            no_startup_id: bool,
            window_id: u32,
        ) {
            self.started
                .push((command.to_string(), no_startup_id, window_id));
        }
    }

    fn autostart(command: &str, no_startup_id: bool) -> Autostart {
        Autostart {
            command: command.to_string(),
            no_startup_id,
        }
    }

    /// C: src/main.c:1169-1196 — the exec lines run only without -a, the
    /// exec_always lines always; both are started with XCB_NONE and removed
    /// from their queue (so a reload starts the exec_always ones again).
    #[test]
    fn autostart_runs_exec_and_exec_always() {
        let (t, _ws) = workspace_tree();

        // -a: only the exec_always queue is drained.
        let mut execs = VecDeque::from(vec![autostart("xterm", false), autostart("feh", true)]);
        let mut always = VecDeque::from(vec![autostart("i3bar-wrapper", true)]);
        let mut backend = AutostartRecorder::default();
        start_autostart(&t, &mut backend, &mut execs, &mut always, false);
        assert_eq!(
            backend.started,
            vec![("i3bar-wrapper".to_string(), true, XCB_NONE)]
        );
        assert!(always.is_empty());
        // C: the disabled block leaves the exec queue untouched.
        assert_eq!(execs.len(), 2);

        // Default: exec first (in order), then exec_always; both drained.
        let mut backend = AutostartRecorder::default();
        start_autostart(&t, &mut backend, &mut execs, &mut always, true);
        assert_eq!(
            backend.started,
            vec![
                ("xterm".to_string(), false, XCB_NONE),
                ("feh".to_string(), true, XCB_NONE),
            ]
        );
        assert!(execs.is_empty());
        assert!(always.is_empty());
    }

    /// C: an event for an unknown sequence is ignored.
    #[test]
    fn unknown_monitor_event_is_ignored() {
        let (mut s, notifier) = state_with_null_notifier();
        let mut x = RecordingConn::new(1);
        startup_monitor_event(&mut s, SN_MONITOR_EVENT_COMPLETED, "not-ours", &mut x);
        assert!(s.startup_sequences.is_empty());
        assert!(x.log.is_empty());
    }
}
