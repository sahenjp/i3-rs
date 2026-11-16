//! 1:1 port of src/sighandler.c (plus the term-signal table of src/main.c).
//!
//! C: src/sighandler.c, src/main.c:238-261
//!
//! The crash dialog itself is drawn by the X/render layer (C:
//! sighandler_create_dialogs / sighandler_draw_dialog, which need create_window
//! and draw_util). This module owns:
//!
//! * the signal registration table (setup_signal_handler),
//! * the one-shot crash handler semantics (handle_signal),
//! * the crash-dialog key actions (sighandler_handle_key_press: b/r/f),
//! * the backtrace dump (C: gdb attached to the parent, here
//!   std::backtrace in the same file format).
//!
//! The sigaction(2)/signalfd(2) syscall layer lives in the OS/event-loop
//! dependency (calloop::signals); i3-core does not depend on libc or calloop
//! yet, so the table, the action mapping and the event-loop entry points are
//! complete while the syscall registration is a documented TODO. The event
//! loop feeds signals in with push_signal() and drains them with
//! dispatch_pending().

use std::backtrace::Backtrace;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use libi3::log::{debuglog, errorlog};

/******************************************************************************
 * Signals (signal(7), Linux numbering)
 *****************************************************************************/

/// C: signal.h SIGHUP
pub const SIGHUP: i32 = 1;
/// C: signal.h SIGINT
pub const SIGINT: i32 = 2;
/// C: signal.h SIGQUIT
pub const SIGQUIT: i32 = 3;
/// C: signal.h SIGILL
pub const SIGILL: i32 = 4;
/// C: signal.h SIGABRT
pub const SIGABRT: i32 = 6;
/// C: signal.h SIGFPE
pub const SIGFPE: i32 = 8;
/// C: signal.h SIGSEGV
pub const SIGSEGV: i32 = 11;
/// C: signal.h SIGALRM
pub const SIGALRM: i32 = 14;
/// C: signal.h SIGTERM
pub const SIGTERM: i32 = 15;
/// C: signal.h SIGUSR1
pub const SIGUSR1: i32 = 10;
/// C: signal.h SIGUSR2
pub const SIGUSR2: i32 = 12;

/// C: signal.h SIG_DFL
pub const SIG_DFL: usize = 0;
/// C: signal.h SA_NODEFER
pub const SA_NODEFER: i32 = 0x4000_0000;
/// C: signal.h SA_RESETHAND (the high bit of sa_flags)
pub const SA_RESETHAND: i32 = -0x8000_0000;
/// C: signal.h SA_SIGINFO
pub const SA_SIGINFO: i32 = 4;

/// The crash dialog texts.
///
/// C: src/sighandler.c:137-141
pub const MESSAGE_INTRO: &str = "i3 has just crashed. Please report a bug for this.";
/// C: src/sighandler.c:138
pub const MESSAGE_INTRO2: &str =
    "To debug this problem, you can either attach gdb or choose from the following options:";
/// C: src/sighandler.c:139
pub const MESSAGE_OPTION_BACKTRACE: &str = "- 'b' to save a backtrace (requires gdb)";
/// C: src/sighandler.c:140
pub const MESSAGE_OPTION_RESTART: &str = "- 'r' to restart i3 in-place";
/// C: src/sighandler.c:141
pub const MESSAGE_OPTION_FORGET: &str = "- 'f' to forget the previous layout and restart i3";

/******************************************************************************
 * Handler table
 *****************************************************************************/

/// How a signal is handled (src/main.c "Term" vs src/sighandler.c "Core").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAction {
    /// C: src/main.c:228 handle_term_signal — exit(128 + signum), which runs
    /// the atexit(i3_exit) cleanup.
    Term,
    /// C: src/sighandler.c:295 handle_signal — the crash handler (dialog,
    /// backtrace, restart).
    Core,
}

/// One configured signal handler.
///
/// C: struct sigaction (+ the libev ev_signal watchers of src/main.c:239)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalRegistration {
    pub signum: i32,
    pub action: SignalAction,
    /// C: action.sa_flags
    pub flags: i32,
}

/// What the handler did with a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalOutcome {
    /// C: src/main.c:231 exit(128 + signal->signum)
    Exit { code: i32 },
    /// C: src/sighandler.c:295 handle_signal — the crash handler owns the
    /// process now (the dialog event loop).
    Crash { signal: i32 },
}

/// The action selected by a key press in the crash dialog.
///
/// C: src/sighandler.c:269-293 sighandler_handle_key_press
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashAction {
    /// The keysym is not one of b/r/f.
    None,
    /// C: 'b' — sighandler_backtrace()
    Backtrace,
    /// C: 'r' / 'f' — i3_restart(forget_layout)
    Restart { forget_layout: bool },
}

/// The state of src/sighandler.c: the handler table, the raised signal, the
/// backtrace status and the pending-signal queue the event loop drains.
pub struct SignalHandlers {
    /// C: the sigaction registrations of setup_signal_handler()
    pub registrations: Vec<SignalRegistration>,
    /// Whether setup_signal_handlers() ran.
    pub installed: bool,
    /// Signals delivered by the OS, waiting for the event loop.
    pub pending: VecDeque<i32>,
    /// C: src/sighandler.c:23 static int raised_signal
    pub raised_signal: Option<i32>,
    /// C: src/sighandler.c:24 static int backtrace_done
    pub backtrace_done: i32,
    /// The last written backtrace file (C: the private filename variable).
    pub last_backtrace: Option<PathBuf>,
    /// Whether the crash dialog is up (C: the dialogs TAILQ is non-empty).
    /// The dialog windows themselves belong to the X/render layer.
    pub dialogs: bool,
}

impl Default for SignalHandlers {
    fn default() -> Self {
        SignalHandlers::new()
    }
}

impl SignalHandlers {
    /// The zero-initialized C statics.
    pub fn new() -> SignalHandlers {
        SignalHandlers {
            registrations: Vec::new(),
            installed: false,
            pending: VecDeque::new(),
            raised_signal: None,
            backtrace_done: 0,
            last_backtrace: None,
            dialogs: false,
        }
    }

    /// C: src/main.c:238 setup_term_handlers — the (one-shot, graceful) term
    /// handlers, plus src/sighandler.c:334 setup_signal_handler — the core
    /// (crash) handlers.
    ///
    /// The returned table is the registration list; calling
    /// sigaction(signum, &action) for each entry is the OS layer (TODO: needs
    /// libc/calloop, which i3-core does not depend on).
    pub fn setup_signal_handler(&mut self) {
        /* C: src/main.c:248-253 — one watcher per Term signal. libev installs
         * the sigaction; the flags stay 0. */
        for signum in [SIGHUP, SIGINT, SIGALRM, SIGTERM, SIGUSR1, SIGUSR2] {
            self.registrations.push(SignalRegistration {
                signum,
                action: SignalAction::Term,
                flags: 0,
            });
        }

        /* C: src/sighandler.c:334-349 — catch all signals with default action
         * "Core", see signal(7). */
        for signum in [SIGQUIT, SIGILL, SIGABRT, SIGFPE, SIGSEGV] {
            self.registrations.push(SignalRegistration {
                signum,
                action: SignalAction::Core,
                flags: SA_NODEFER | SA_RESETHAND | SA_SIGINFO,
            });
        }

        self.installed = true;
    }

    /// C: src/sighandler.c:295 handle_signal (core signals) and
    /// src/main.c:228 handle_term_signal (term signals).
    ///
    /// For a core signal C first resets the disposition to SIG_DFL (the
    /// SA_RESETHAND flag) so a second fault kills the process, then records the
    /// signal and opens the crash dialog.
    pub fn handle_signal(&mut self, signum: i32) -> SignalOutcome {
        let action = self
            .registrations
            .iter()
            .find(|registration| registration.signum == signum)
            .map(|registration| registration.action);

        match action {
            Some(SignalAction::Core) => {
                debuglog(&format!("i3 crashed. SIG: {}\n", signum));

                /* C: struct sigaction action; action.sa_handler = SIG_DFL;
                 *    action.sa_flags = 0; sigaction(sig, &action, NULL);
                 * SA_RESETHAND already restored SIG_DFL, and the port drops the
                 * registration so a second crash terminates the process. */
                self.registrations
                    .retain(|registration| registration.signum != signum);
                self.raised_signal = Some(signum);

                /* C: sighandler_setup(); sighandler_create_dialogs(); — the
                 * dialog text/size setup and the window creation belong to the
                 * X/render layer; the dialog state is tracked here. */
                self.dialogs = true;
                SignalOutcome::Crash { signal: signum }
            }
            Some(SignalAction::Term) => {
                /* C: exit(128 + signal->signum) — the atexit(i3_exit) cleanup
                 * runs. */
                SignalOutcome::Exit {
                    code: 128 + signum,
                }
            }
            None => {
                /* Not registered: the default Term disposition applies. */
                SignalOutcome::Exit {
                    code: 128 + signum,
                }
            }
        }
    }

    /// C: src/sighandler.c:209 sighandler_destroy_dialogs (the X side is the
    /// dialog port; the state is cleared here).
    pub fn sighandler_destroy_dialogs(&mut self) {
        self.dialogs = false;
    }

    /// C: src/sighandler.c:269 sighandler_handle_key_press — a key press in the
    /// crash dialog: 'b' saves a backtrace, 'r' restarts in place, 'f' forgets
    /// the layout and restarts.
    pub fn sighandler_handle_key_press(&mut self, keysym: u32) -> CrashAction {
        if keysym == u32::from(b'b') {
            debuglog("User issued core-dump command.\n");

            /* fork and exec/attach GDB to the parent to get a backtrace in the
             * tmpdir */
            self.backtrace_done = self.sighandler_backtrace();
            CrashAction::Backtrace
        } else if keysym == u32::from(b'r') {
            self.sighandler_destroy_dialogs();
            /* C: i3_restart(false); */
            CrashAction::Restart {
                forget_layout: false,
            }
        } else if keysym == u32::from(b'f') {
            self.sighandler_destroy_dialogs();
            /* C: i3_restart(true); */
            CrashAction::Restart { forget_layout: true }
        } else {
            CrashAction::None
        }
    }

    /// C: src/sighandler.c:49 sighandler_backtrace — attach gdb to the parent
    /// and dump a backtrace to i3-backtrace.<pid>.<suffix>.txt in the tmpdir.
    ///
    /// std::backtrace replaces the gdb attachment; the file name, the
    /// uniqueness loop and the return values (-1 failure, 1 success) match the
    /// C function.
    pub fn sighandler_backtrace(&mut self) -> i32 {
        let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        self.sighandler_backtrace_with_dir(Path::new(&tmpdir))
    }

    /// sighandler_backtrace() with an explicit tmpdir (the TMPDIR lookup is
    /// process-global, so tests pass the directory in).
    pub fn sighandler_backtrace_with_dir(&mut self, tmpdir: &Path) -> i32 {
        let pid = std::process::id();

        /* Find a unique filename for the backtrace (since the PID of i3 stays
         * the same), so that we don't overwrite earlier backtraces. */
        let mut suffix = 0;
        let filename = loop {
            let candidate = tmpdir.join(format!("i3-backtrace.{}.{}.txt", pid, suffix));
            if !candidate.exists() {
                break candidate;
            }
            suffix += 1;
        };

        let backtrace = Backtrace::force_capture();
        let content = format!(
            "i3 crashed. SIG: {}\nBacktrace (std::backtrace, standing in for gdb \"bt full\"):\n{}",
            self.raised_signal.unwrap_or(0),
            format_backtrace(&backtrace)
        );

        match std::fs::write(&filename, content) {
            Ok(()) => {
                self.backtrace_done = 1;
                self.last_backtrace = Some(filename);
                1
            }
            Err(err) => {
                errorlog(&format!(
                    "Could not write the backtrace to {}: {}\n",
                    filename.display(),
                    err
                ));
                self.backtrace_done = -1;
                -1
            }
        }
    }

    /// The event-loop hand-off of the OS signal handler (C: the libev
    /// ev_signal callback / a signalfd read): enqueue a delivered signal.
    pub fn push_signal(&mut self, signum: i32) {
        self.pending.push_back(signum);
    }

    /// The event-loop hook (calloop::signals equivalent): handle every pending
    /// signal and return the actions the loop has to carry out.
    pub fn dispatch_pending(&mut self) -> Vec<SignalOutcome> {
        let mut outcomes = Vec::new();
        while let Some(signum) = self.pending.pop_front() {
            outcomes.push(self.handle_signal(signum));
        }
        outcomes
    }
}

/// C: setup_signal_handler() — install the handler table (the sigaction calls
/// are the OS layer, see setup_signal_handler).
pub fn setup_signal_handlers() -> SignalHandlers {
    let mut handlers = SignalHandlers::new();
    handlers.setup_signal_handler();
    handlers
}

/******************************************************************************
 * Term signals (C: src/main.c:226-261)
 *****************************************************************************/

/// C: the libev ev_signal watcher of setup_term_handlers(). A signal handler
/// may not run exit(), so the number is recorded here (async-signal-safe) and
/// the event loop turns it into exit(128 + signum) — exactly the deferral
/// libev provides in C.
static TERM_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// C: src/main.c:229 handle_term_signal — exits with 128 + signum.
extern "C" fn handle_term_signal(signum: libc::c_int) {
    TERM_SIGNAL.store(signum, std::sync::atomic::Ordering::SeqCst);
}

/// C: src/main.c:238 setup_term_handlers — one graceful (one-shot) handler per
/// signal with default action "Term" (SIGHUP, SIGINT, SIGALRM, SIGTERM,
/// SIGUSR1, SIGUSR2). libev installs the sigactions in C; the port installs
/// them directly (signal(2), i.e. SA_RESTART, so a blocked read is not torn).
pub fn setup_term_handlers() {
    for signum in [SIGHUP, SIGINT, SIGALRM, SIGTERM, SIGUSR1, SIGUSR2] {
        unsafe {
            let handler = handle_term_signal as extern "C" fn(libc::c_int);
            libc::signal(signum, handler as libc::sighandler_t);
        }
    }
}

/// The signal the event loop has to exit for: C's handle_term_signal runs
/// inside the loop and exits there, the port polls the recorded signal once
/// per iteration.
pub fn take_term_signal() -> Option<i32> {
    match TERM_SIGNAL.swap(0, std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        signum => Some(signum),
    }
}

/// Render a std::backtrace in the gdb "bt full" shape used by
/// sighandler_backtrace: one "#<n> <frame>" line per frame, with the source
/// location indented below it.
fn format_backtrace(backtrace: &Backtrace) -> String {
    let text = format!("{}", backtrace);
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        match split_frame_header(trimmed) {
            Some((index, rest)) => out.push_str(&format!("#{} {}\n", index, rest)),
            None => {
                let content = trimmed.trim_end();
                if !content.is_empty() {
                    out.push_str(&format!("    {}\n", content));
                }
            }
        }
    }
    out
}

/// Split a std::backtrace frame header ("12: frame name") into its index and
/// name. Returns None for the indented "at ..." location lines.
fn split_frame_header(line: &str) -> Option<(u32, &str)> {
    let position = line.find(": ")?;
    let (number, rest) = line.split_at(position);
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let index = number.parse().ok()?;
    Some((index, rest[2..].trim()))
}

/// C: backtrace_symbols() stand-in for the log: the symbolic frames of the
/// current stack, one per line.
pub fn backtrace_symbols() -> Vec<String> {
    let backtrace = Backtrace::force_capture();
    format!("{}", backtrace)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/******************************************************************************
 * Tests
 *****************************************************************************/

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("i3-sighandler-{}-{}", tag, nanos));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// C: src/main.c:238-261 + src/sighandler.c:334-349 — the registration
    /// table.
    #[test]
    fn signal_registration_table() {
        let handlers = setup_signal_handlers();
        assert!(handlers.installed);

        let term: Vec<i32> = handlers
            .registrations
            .iter()
            .filter(|registration| registration.action == SignalAction::Term)
            .map(|registration| registration.signum)
            .collect();
        assert_eq!(term, vec![SIGHUP, SIGINT, SIGALRM, SIGTERM, SIGUSR1, SIGUSR2]);

        let core: Vec<i32> = handlers
            .registrations
            .iter()
            .filter(|registration| registration.action == SignalAction::Core)
            .map(|registration| registration.signum)
            .collect();
        assert_eq!(core, vec![SIGQUIT, SIGILL, SIGABRT, SIGFPE, SIGSEGV]);

        for registration in &handlers.registrations {
            match registration.action {
                SignalAction::Term => assert_eq!(registration.flags, 0),
                SignalAction::Core => assert_eq!(
                    registration.flags,
                    SA_NODEFER | SA_RESETHAND | SA_SIGINFO
                ),
            }
        }
    }

    /// C: the signal -> action mapping: Term signals exit with 128+signum, Core
    /// signals enter the crash handler once.
    #[test]
    fn signal_to_action_mapping() {
        let mut handlers = setup_signal_handlers();

        assert_eq!(
            handlers.handle_signal(SIGHUP),
            SignalOutcome::Exit { code: 129 }
        );
        assert_eq!(
            handlers.handle_signal(SIGINT),
            SignalOutcome::Exit { code: 130 }
        );
        assert_eq!(
            handlers.handle_signal(SIGTERM),
            SignalOutcome::Exit { code: 143 }
        );
        assert_eq!(handlers.raised_signal, None);

        assert_eq!(
            handlers.handle_signal(SIGSEGV),
            SignalOutcome::Crash { signal: SIGSEGV }
        );
        assert_eq!(handlers.raised_signal, Some(SIGSEGV));
        assert!(handlers.dialogs);
        // SA_RESETHAND: the registration is gone after the first crash.
        assert!(handlers
            .registrations
            .iter()
            .all(|registration| registration.signum != SIGSEGV));
    }

    /// The event-loop hand-off: push_signal + dispatch_pending.
    #[test]
    fn dispatch_pending_signals() {
        let mut handlers = setup_signal_handlers();
        handlers.push_signal(SIGTERM);
        handlers.push_signal(SIGSEGV);
        let outcomes = handlers.dispatch_pending();
        assert_eq!(
            outcomes,
            vec![
                SignalOutcome::Exit { code: 143 },
                SignalOutcome::Crash { signal: SIGSEGV },
            ]
        );
        assert!(handlers.dispatch_pending().is_empty());
    }

    /// C: src/sighandler.c:269-293 — the crash dialog keys.
    #[test]
    fn crash_dialog_keys() {
        let mut handlers = setup_signal_handlers();
        handlers.handle_signal(SIGSEGV);

        // 'b' writes a backtrace (into $TMPDIR, which we do not touch).
        assert_eq!(
            handlers.sighandler_handle_key_press(u32::from(b'b')),
            CrashAction::Backtrace
        );
        assert_eq!(handlers.backtrace_done, 1);
        let written = handlers.last_backtrace.clone();
        if let Some(path) = &written {
            let _ = std::fs::remove_file(path);
        }

        // 'r' restarts i3 in-place.
        assert_eq!(
            handlers.sighandler_handle_key_press(u32::from(b'r')),
            CrashAction::Restart {
                forget_layout: false
            }
        );
        assert!(!handlers.dialogs);

        // 'f' forgets the layout and restarts.
        handlers.dialogs = true;
        assert_eq!(
            handlers.sighandler_handle_key_press(u32::from(b'f')),
            CrashAction::Restart { forget_layout: true }
        );

        // Any other key is ignored.
        assert_eq!(
            handlers.sighandler_handle_key_press(u32::from(b'x')),
            CrashAction::None
        );

    }

    /// C: src/sighandler.c:49 — the backtrace file name is unique per run and
    /// contains the frames in the gdb "bt full" shape.
    #[test]
    fn backtrace_file_written_with_unique_name() {
        let dir = temp_dir("backtrace");
        let mut handlers = setup_signal_handlers();
        handlers.handle_signal(SIGSEGV);

        assert_eq!(handlers.sighandler_backtrace_with_dir(&dir), 1);
        assert_eq!(handlers.backtrace_done, 1);
        let first = handlers.last_backtrace.clone().expect("backtrace path");
        assert_eq!(
            first.file_name().unwrap().to_string_lossy().to_string(),
            format!("i3-backtrace.{}.0.txt", std::process::id())
        );
        let content = std::fs::read_to_string(&first).expect("backtrace file");
        assert!(content.starts_with("i3 crashed. SIG: 11\n"));
        assert!(content.contains("#0 "));

        // A second dump picks the next free suffix instead of overwriting.
        assert_eq!(handlers.sighandler_backtrace_with_dir(&dir), 1);
        let second = handlers.last_backtrace.clone().expect("backtrace path");
        assert_eq!(
            second.file_name().unwrap().to_string_lossy().to_string(),
            format!("i3-backtrace.{}.1.txt", std::process::id())
        );
        assert!(first.exists() && second.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The symbolic-frame helper yields readable frame strings.
    #[test]
    fn format_backtrace_prefixes_frame_numbers() {
        let backtrace = Backtrace::force_capture();
        let formatted = format_backtrace(&backtrace);
        assert!(formatted.contains("#0 "));
        assert!(formatted.lines().count() >= 1);
        assert!(!backtrace_symbols().is_empty());
    }
}
