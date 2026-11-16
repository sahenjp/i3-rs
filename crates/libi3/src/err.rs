//! C: <err.h> / <stdio.h> perror — process error reporting helpers used
//! throughout libi3 and the i3 tools (err, errx, warn, warnx, perror).
//!
//! glibc's err family prefixes messages with program_invocation_short_name
//! (the basename of argv[0]) and appends strerror(errno) for err/warn.

use std::ffi::CStr;
use std::sync::OnceLock;

fn progname() -> &'static str {
    static PROGNAME: OnceLock<String> = OnceLock::new();
    PROGNAME.get_or_init(|| {
        // glibc: program_invocation_short_name = basename of argv[0]
        match std::env::args().next() {
            Some(arg0) => match arg0.rsplit('/').next() {
                Some(base) if !base.is_empty() => base.to_string(),
                _ => arg0,
            },
            None => "libi3".to_string(),
        }
    })
}

/// C: <err.h> strerror() — the error string for an errno value.
pub fn strerror(errno: i32) -> String {
    unsafe { CStr::from_ptr(libc::strerror(errno)) }
        .to_string_lossy()
        .into_owned()
}

fn errno_value() -> i32 {
    // The current thread's errno, like the C errno variable.
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// C: <err.h> err() — "prog: message: strerror(errno)" then exit(exit_code).
#[allow(clippy::needless_pass_by_value)] // mirrors C's variadic err()
pub fn err(exit_code: i32, msg: &str) -> ! {
    eprintln!("{}: {}: {}", progname(), msg, strerror(errno_value()));
    std::process::exit(exit_code);
}

/// C: <err.h> errx() — "prog: message" then exit(exit_code) (no strerror).
#[allow(clippy::needless_pass_by_value)] // mirrors C's variadic errx()
pub fn errx(exit_code: i32, msg: &str) -> ! {
    eprintln!("{}: {}", progname(), msg);
    std::process::exit(exit_code);
}

/// C: <err.h> warn() — "prog: message: strerror(errno)".
pub fn warn(msg: &str) {
    eprintln!("{}: {}: {}", progname(), msg, strerror(errno_value()));
}

/// C: <err.h> warnx() — "prog: message".
pub fn warnx(msg: &str) {
    eprintln!("{}: {}", progname(), msg);
}

/// C: <stdio.h> perror() — "prefix: strerror(errno)".
pub fn perror(prefix: &str) {
    eprintln!("{}: {}", prefix, strerror(errno_value()));
}

/// C: errno = value (e.g. libi3/create_socket.c sets errno = EEXIST).
pub fn set_errno(value: i32) {
    unsafe {
        *libc::__errno_location() = value;
    }
}
