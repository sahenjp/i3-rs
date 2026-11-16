//! C: include/libi3.h — verboselog/errorlog/debuglog fallback declarations.
//!
//! In C, the embedding program (e.g. i3-msg/main.c:35,43) defines these
//! functions and libi3's LOG/ELOG/DLOG macros resolve to them at link time.
//! Rust has no link-time late binding: these implementations print like
//! i3-msg's definitions (verboselog → stdout, errorlog → stderr).
//! TODO(i3 core port): let the i3 binary redirect these to its log file.

/// C: libi3.h verboselog() — i3-msg/main.c:35 definition: vfprintf(stdout, ...).
pub fn verboselog(msg: &str) {
    print!("{}", msg);
}

/// C: libi3.h errorlog() — i3-msg/main.c:43 definition: vfprintf(stderr, ...).
pub fn errorlog(msg: &str) {
    eprint!("{}", msg);
}

/// C: libi3.h debuglog() — i3 itself logs to a file; i3-msg never defines it
/// (the DLOG call sites are only linked into i3). Stderr for now.
pub fn debuglog(msg: &str) {
    eprint!("{}", msg);
}
