//! 1:1 port of the util.c helpers used by the tree layer.
//!
//! C: src/util.c

use crate::data::{Layout, Orientation, Position, Rect, Direction};

/// C: src/util.c:39 `rect_add`
pub fn rect_add(a: Rect, b: Rect) -> Rect {
    Rect {
        x: a.x.wrapping_add(b.x),
        y: a.y.wrapping_add(b.y),
        width: a.width.wrapping_add(b.width),
        height: a.height.wrapping_add(b.height),
    }
}

/// C: src/util.c:46 `rect_sub`
pub fn rect_sub(a: Rect, b: Rect) -> Rect {
    Rect {
        x: a.x.wrapping_sub(b.x),
        y: a.y.wrapping_sub(b.y),
        width: a.width.wrapping_sub(b.width),
        height: a.height.wrapping_sub(b.height),
    }
}

/// C: src/util.c:53 `rect_sanitize_dimensions`
pub fn rect_sanitize_dimensions(mut rect: Rect) -> Rect {
    rect.width = if (rect.width as i64) <= 0 { 1 } else { rect.width };
    rect.height = if (rect.height as i64) <= 0 { 1 } else { rect.height };
    rect
}

/// C: src/util.c:59 `rect_equals`
pub fn rect_equals(a: Rect, b: Rect) -> bool {
    a.x == b.x && a.y == b.y && a.width == b.width && a.height == b.height
}

/// C: src/util.c:32 rect_contains
pub fn rect_contains(rect: Rect, x: u32, y: u32) -> bool {
    x >= rect.x
        && x <= rect.x.wrapping_add(rect.width)
        && y >= rect.y
        && y <= rect.y.wrapping_add(rect.height)
}

/// C: src/util.c:67 `name_is_digits`
pub fn name_is_digits(name: &str) -> bool {
    // C: an empty string iterates zero times and returns true (bug-for-bug).
    name.chars().all(|c| c.is_ascii_digit())
}

/// C: src/util.c:87 `layout_from_name`
pub fn layout_from_name(layout_str: &str) -> Option<Layout> {
    if layout_str == "default" {
        return Some(Layout::L_DEFAULT);
    }
    if layout_str.eq_ignore_ascii_case("stacked") || layout_str.eq_ignore_ascii_case("stacking") {
        return Some(Layout::L_STACKED);
    }
    if layout_str.eq_ignore_ascii_case("tabbed") {
        return Some(Layout::L_TABBED);
    }
    if layout_str.eq_ignore_ascii_case("splitv") {
        return Some(Layout::L_SPLITV);
    }
    if layout_str.eq_ignore_ascii_case("splith") {
        return Some(Layout::L_SPLITH);
    }
    None
}

/// C: src/util.c:115 `ws_name_to_number`
///
/// Parses the workspace name as a number. Returns -1 if the workspace should
/// be interpreted as a "named workspace".
// C keeps the strtoll out-parameter initialized ("char *endptr = NULL;",
// src/util.c:117); the port writes it before the first read, so the
// initialization is dead but stays 1:1.
#[allow(unused_assignments)]
pub fn ws_name_to_number(name: &str) -> i32 {
    /* positive integers and zero are interpreted as numbers */
    let mut endptr = 0usize;
    let mut parsed_num: i64;
    let bytes = name.as_bytes();
    // strtoll: skip leading whitespace, optional sign, then digits
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    let start_digits;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        neg = bytes[i] == b'-';
        i += 1;
        start_digits = i;
    } else {
        start_digits = i;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    endptr = i;
    if endptr == start_digits {
        // endptr == name: no digits parsed
        parsed_num = -1;
    } else {
        let digits = &name[start_digits..endptr];
        // saturating conversion; C uses errno/ERANGE for overflow
        let mut val: i64 = digits.parse::<i64>().unwrap_or(i64::MAX);
        if neg {
            val = -val;
        }
        parsed_num = val;
        if parsed_num > i32::MAX as i64 || parsed_num < 0 {
            parsed_num = -1;
        }
    }
    if parsed_num == -1 {
        return -1;
    }
    parsed_num as i32
}

/// C: src/util.c:480 `position_from_direction`
pub fn position_from_direction(direction: Direction) -> Position {
    if direction == Direction::D_LEFT || direction == Direction::D_UP {
        Position::BEFORE
    } else {
        Position::AFTER
    }
}

/// C: src/util.c:472 `orientation_from_direction`
pub fn orientation_from_direction(direction: Direction) -> Orientation {
    if direction == Direction::D_LEFT || direction == Direction::D_RIGHT {
        Orientation::HORIZ
    } else {
        Orientation::VERT
    }
}

/// C: src/util.c:380 — the utility start_nagbar() forks and execs.
pub const I3_NAGBAR: &str = "i3-nagbar";

/// C: src/util.c:159-181 — the three binaries exec_i3_utility() tries, in C's
/// order. `dirname()` gives "." for a bare filename, so a relative start_argv0
/// yields "./i3-nagbar" like in C.
pub fn i3_utility_candidates(name: &str, start_argv0: &str) -> Vec<String> {
    let mut candidates = vec![name.to_string()];

    // C: char *pathbuf = sstrdup(start_argv[0]); char *dir = dirname(pathbuf);
    //    sasprintf(&binary, "%s/%s", dir, name);
    candidates.push(format!("{}/{}", dirname(start_argv0), name));

    // C: #if defined(__linux__) — readlink("/proc/self/exe") + dirname().
    //    C warn()s and _exit()s when the readlink fails, so the candidate is
    //    simply missing then.
    if let Ok(exe) = std::fs::read_link("/proc/self/exe") {
        let exe = exe.to_string_lossy().into_owned();
        candidates.push(format!("{}/{}", dirname(&exe), name));
    }

    candidates
}

/// C: dirname(3) — the directory part, "." for a bare filename.
fn dirname(path: &str) -> String {
    match std::path::Path::new(path).parent() {
        Some(parent) if parent.as_os_str().is_empty() => ".".to_string(),
        Some(parent) => parent.to_string_lossy().into_owned(),
        None => ".".to_string(),
    }
}

/// The argument vectors of every execvp() candidate, prepared before fork().
///
/// Each entry is (binary, argv): argv[0] is the binary, the remaining entries
/// are `argv[1..]` of the caller — C does the same assignment in
/// exec_i3_utility().
pub fn prepare_i3_utility(
    name: &str,
    argv: &[String],
    start_argv0: &str,
) -> Vec<(std::ffi::CString, Vec<std::ffi::CString>)> {
    let mut prepared = Vec::new();
    for binary in i3_utility_candidates(name, start_argv0) {
        let binary_c = match std::ffi::CString::new(binary.clone()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut args = Vec::with_capacity(argv.len());
        for (index, arg) in argv.iter().enumerate() {
            let value = if index == 0 {
                // C: argv[0] = binary;
                binary.clone()
            } else {
                arg.clone()
            };
            match std::ffi::CString::new(value) {
                Ok(c) => args.push(c),
                Err(_) => break,
            }
        }
        if args.len() != argv.len() {
            continue;
        }
        prepared.push((binary_c, args));
    }
    prepared
}

/// C: src/util.c:153-183 `exec_i3_utility` — the child half.
///
/// execvp()s every prepared candidate in C's order; never returns on success.
/// After the last failure C does `warn("Could not start %s", name);
/// _exit(2)`.
pub fn exec_i3_utility(
    name: &str,
    prepared: &[(std::ffi::CString, Vec<std::ffi::CString>)],
) -> ! {
    for (binary, args) in prepared {
        let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
        argv.push(std::ptr::null());
        // C: execvp(binary, argv);
        unsafe { libc::execvp(binary.as_ptr(), argv.as_ptr()) };
    }

    // C: warn("Could not start %s", name); _exit(2);
    // glibc's warn() prefixes program_invocation_short_name, i.e. argv[0].
    let progname = std::env::args_os()
        .next()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_else(|| "i3".to_string());
    let message = format!("{}: could not start {}\n", progname, name);
    unsafe {
        libc::write(2, message.as_ptr() as *const libc::c_void, message.len());
        libc::_exit(2);
    }
}

/// C: src/util.c:366 `start_nagbar` — the fork()/exec() half of the function.
///
/// The trait exists so the argv assembly and the pid bookkeeping can be tested
/// without forking (C calls fork()/exec_i3_utility() inline).
pub trait NagbarSpawner {
    /// Fork()s and execs "i3-nagbar" with `argv`; returns the child pid, or -1
    /// when fork() failed. `argv[0]` is the placeholder C passes as NULL.
    fn fork_exec_nagbar(&mut self, argv: &[String]) -> i32;
}

/// The production spawner.
///
/// C: src/util.c:374-397 — fork(), the child execs the utility, the parent
/// keeps the pid and installs an ev_child watcher (`nagbar_exited`,
/// util.c:352-358) which resets the pid slot to -1 when the nagbar exits.
///
/// The port has no libev loop and the pid lives in a plain field of
/// ConfigDirectives/Bindings, so the watcher cannot write it back. Instead the
/// child is reaped from a detached thread and [start_nagbar] treats a pid that
/// is no longer alive as "no nagbar is running" — the only state the C reset
/// produces.
#[derive(Debug, Default)]
pub struct ForkExecNagbar;

impl NagbarSpawner for ForkExecNagbar {
    fn fork_exec_nagbar(&mut self, argv: &[String]) -> i32 {
        // C: start_argv[0] — main() keeps argv around for exactly this
        // dirname() fallback.
        let start_argv0 = std::env::args_os()
            .next()
            .map(|a| a.to_string_lossy().into_owned())
            .unwrap_or_else(|| "i3".to_string());
        let prepared = prepare_i3_utility(I3_NAGBAR, argv, &start_argv0);

        // C: *nagbar_pid = fork();
        let pid = unsafe { libc::fork() };
        if pid == -1 {
            // C: warn("Could not fork()");
            crate::log::errorlog("Could not fork()\n");
            return -1;
        }

        if pid == 0 {
            // C: exec_i3_utility("i3-nagbar", argv);
            exec_i3_utility(I3_NAGBAR, &prepared);
        }

        // C: ev_child_init(child, &nagbar_exited, *nagbar_pid, 0);
        //    ev_child_start(main_loop, child);
        std::thread::spawn(move || {
            let mut status: libc::c_int = 0;
            unsafe { libc::waitpid(pid, &mut status, 0) };
        });

        pid
    }
}

/// Test double recording the fork()/exec_i3_utility() call.
///
/// C: src/util.c:374 `*nagbar_pid = fork()`
#[derive(Debug, Default)]
pub struct RecordingNagbarSpawner {
    /// The argv of every spawn, in call order (argv[0] is the placeholder).
    pub calls: Vec<Vec<String>>,
    /// C: fork() returning -1 (the failure path).
    pub fail: bool,
    next_pid: i32,
}

impl NagbarSpawner for RecordingNagbarSpawner {
    fn fork_exec_nagbar(&mut self, argv: &[String]) -> i32 {
        self.calls.push(argv.to_vec());
        if self.fail {
            return -1;
        }
        self.next_pid += 1;
        2000 + self.next_pid
    }
}

/// True when `pid` still exists — the port's stand-in for the ev_child watcher
/// that C's nagbar_exited() callback provides.
fn process_is_running(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    match unsafe { libc::kill(pid, 0) } {
        0 => true,
        _ => std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM),
    }
}

/// C: src/util.c:366 `start_nagbar`
///
/// Starts an i3-nagbar instance with the given parameters. The resulting pid is
/// stored in `*nagbar_pid` and can be passed to [kill_nagbar] later on.
///
/// C's signature takes `pid_t *nagbar_pid` and `char *argv[]`; the spawner is
/// the seam for the fork()/exec_i3_utility() pair ([ForkExecNagbar] in
/// production, [RecordingNagbarSpawner] in tests).
pub fn start_nagbar(spawner: &mut dyn NagbarSpawner, nagbar_pid: &mut i32, argv: &[String]) {
    if *nagbar_pid != -1 {
        if process_is_running(*nagbar_pid) {
            // C: DLOG("i3-nagbar already running (PID %d), not starting again.\n", *nagbar_pid);
            crate::log::dlog_at(
                "src/util.c",
                "start_nagbar",
                370,
                &format!(
                    "i3-nagbar already running (PID {}), not starting again.\n",
                    *nagbar_pid
                ),
            );
            return;
        }
        // C: nagbar_exited() (util.c:352-358) resets the slot as soon as the
        // child is reaped; the port notices it here instead.
        *nagbar_pid = -1;
    }

    let pid = spawner.fork_exec_nagbar(argv);
    if pid == -1 {
        // C: warn("Could not fork()") — already logged by the spawner.
        return;
    }

    *nagbar_pid = pid;
    // C: DLOG("Starting i3-nagbar with PID %d\n", *nagbar_pid);
    crate::log::dlog_at(
        "src/util.c",
        "start_nagbar",
        386,
        &format!("Starting i3-nagbar with PID {}\n", pid),
    );
}

/// C: src/util.c:400 `kill_nagbar`
///
/// Kills the i3-nagbar process, if any: SIGTERM, plus a blocking waitpid()
/// when `wait_for_it` is set (C needs that when restarting: the libev loop is
/// not entered again and SIGCHLD is therefore never reaped). A pid of -1 means
/// "no nagbar is running" and returns immediately — the state of both nagbar
/// slots until a config/command error starts one (src/config_parser.c:507,
/// src/bindings.c:883).
pub fn kill_nagbar(nagbar_pid: i32, wait_for_it: bool) {
    if nagbar_pid == -1 {
        return;
    }

    // C: if (kill(nagbar_pid, SIGTERM) == -1) warn("kill(configerror_nagbar) failed");
    if unsafe { libc::kill(nagbar_pid, libc::SIGTERM) } == -1 {
        crate::log::errorlog("kill(configerror_nagbar) failed\n");
    }

    if !wait_for_it {
        return;
    }

    // C: waitpid(nagbar_pid, NULL, 0);
    let mut status: libc::c_int = 0;
    unsafe {
        libc::waitpid(nagbar_pid, &mut status, 0);
    }
}

/// C: libi3/safewrappers.c:124 `strcasecmp_nullable`
pub fn strcasecmp_nullable(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C: src/util.c:159-181 — the candidates of exec_i3_utility().
    #[test]
    fn i3_utility_candidates_follow_c() {
        let candidates = i3_utility_candidates("i3-nagbar", "/opt/i3/bin/i3");
        // C: execvp(name, ...) — the PATH lookup comes first.
        assert_eq!(candidates[0], "i3-nagbar");
        // C: sasprintf(&binary, "%s/%s", dirname(start_argv[0]), name)
        assert_eq!(candidates[1], "/opt/i3/bin/i3-nagbar");
        // C: the __linux__ fallback uses dirname(/proc/self/exe).
        let exe = std::fs::read_link("/proc/self/exe").unwrap();
        let exe_dir = exe.parent().unwrap().to_string_lossy().into_owned();
        assert_eq!(candidates[2], format!("{}/i3-nagbar", exe_dir));

        // C: dirname("i3") == "." — a relative start_argv0 yields "./i3-nagbar".
        assert_eq!(i3_utility_candidates("i3-nagbar", "i3")[1], "./i3-nagbar");
    }

    /// C: util.c:163 `argv[0] = binary` — the NULL placeholder slot of the
    /// caller becomes the binary name of the candidate.
    #[test]
    fn prepare_i3_utility_replaces_the_placeholder() {
        let argv = vec![
            String::new(),
            "-f".to_string(),
            "fixed".to_string(),
            "-m".to_string(),
            "hi".to_string(),
        ];
        let prepared = prepare_i3_utility("i3-nagbar", &argv, "/opt/i3/bin/i3");
        assert_eq!(prepared.len(), i3_utility_candidates("i3-nagbar", "/opt/i3/bin/i3").len());
        let (binary, args) = &prepared[1];
        assert_eq!(binary.to_str().unwrap(), "/opt/i3/bin/i3-nagbar");
        assert_eq!(args[0].to_str().unwrap(), "/opt/i3/bin/i3-nagbar");
        // The remaining arguments are untouched (C keeps argv[1..] as-is).
        let rest: Vec<&str> = args[1..].iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(rest, vec!["-f", "fixed", "-m", "hi"]);
    }

    /// C: src/util.c:366-372 — "i3-nagbar already running (PID %d), not
    /// starting again."
    #[test]
    fn start_nagbar_refuses_a_second_instance() {
        let mut spawner = RecordingNagbarSpawner::default();
        // The own pid is alive, so the guard triggers (C would rely on the
        // ev_child watcher not having reset the slot yet).
        let mut nagbar_pid = std::process::id() as i32;
        start_nagbar(&mut spawner, &mut nagbar_pid, &[String::from(""), String::from("-m")]);
        assert!(spawner.calls.is_empty());
        assert_eq!(nagbar_pid, std::process::id() as i32);
    }

    /// C: src/util.c:374-397 — fork(), the pid is stored in *nagbar_pid and
    /// the argv reaches exec_i3_utility() unchanged.
    #[test]
    fn start_nagbar_forks_and_stores_the_pid() {
        let mut spawner = RecordingNagbarSpawner::default();
        let mut nagbar_pid = -1;
        let argv = vec![
            String::new(),
            "-t".to_string(),
            "error".to_string(),
            "-m".to_string(),
            "You have an error in your i3 config file!".to_string(),
        ];
        start_nagbar(&mut spawner, &mut nagbar_pid, &argv);
        assert_eq!(spawner.calls, vec![argv]);
        assert_eq!(nagbar_pid, 2001);
    }

    /// C: src/util.c:352-358 nagbar_exited() resets the slot to -1 once the
    /// child is gone; the port detects that (no libev watcher) and starts the
    /// new nagbar instead of returning early.
    #[test]
    fn start_nagbar_starts_again_after_the_nagbar_exited() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id() as i32;
        let _ = child.wait();

        let mut spawner = RecordingNagbarSpawner::default();
        let mut nagbar_pid = dead_pid;
        start_nagbar(&mut spawner, &mut nagbar_pid, &[String::from("")]);
        assert_eq!(spawner.calls.len(), 1);
        assert_eq!(nagbar_pid, 2001);
    }

    /// C: src/util.c:375-378 — fork() == -1 leaves the slot untouched.
    #[test]
    fn start_nagbar_keeps_the_slot_when_fork_fails() {
        let mut spawner = RecordingNagbarSpawner { fail: true, ..Default::default() };
        let mut nagbar_pid = -1;
        start_nagbar(&mut spawner, &mut nagbar_pid, &[String::from("")]);
        assert_eq!(spawner.calls.len(), 1);
        assert_eq!(nagbar_pid, -1);
    }

    /// C: src/util.c:400-404 — kill_nagbar(-1) is a no-op.
    #[test]
    fn kill_nagbar_without_a_pid_returns_immediately() {
        kill_nagbar(-1, true);
    }
}
