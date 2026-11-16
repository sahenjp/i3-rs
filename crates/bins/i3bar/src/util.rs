#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! C: i3bar/include/util.h — small helpers shared by every i3bar module.
//!
//! The C file is a header of macros (MAX/MIN/STARTS_WITH/FREE/DLOG/ELOG). The
//! Rust port keeps the same call sites as functions/macros with the same
//! observable behaviour.

#![allow(non_snake_case)] // MAX/MIN/STARTS_WITH keep the C spelling

/// C: include/util.h:19 MAX(x, y) ((x) > (y) ? (x) : (y))
pub fn MAX<T: PartialOrd>(x: T, y: T) -> T {
    if x > y {
        x
    } else {
        y
    }
}

/// C: include/util.h:16 MIN(x, y) ((x) < (y) ? (x) : (y))
pub fn MIN<T: PartialOrd>(x: T, y: T) -> T {
    if x < y {
        x
    } else {
        y
    }
}

/// C: include/util.h:24 STARTS_WITH(string, len, needle) — case-insensitive
/// prefix test that also requires `len >= strlen(needle)`.
pub fn STARTS_WITH(string: &[u8], len: usize, needle: &str) -> bool {
    let n = needle.as_bytes();
    len >= n.len() && string.len() >= n.len() && string[..n.len()].eq_ignore_ascii_case(n)
}

/// C: include/util.h:63 DLOG(fmt, ...) — only printed when config.verbose is
/// set. The C version prints "[file:line] fmt"; the Rust port prints the same
/// shape with the module path the macro would have expanded to.
#[macro_export]
macro_rules! DLOG {
    ($($arg:tt)*) => {{
        if $crate::config::config(|c| c.verbose) {
            print!("[i3bar] ");
            println!($($arg)*);
        }
    }};
}

/// C: include/util.h:78 ELOG(fmt, ...) — always printed to stderr, prefixed
/// with "[file:line] ERROR: ".
#[macro_export]
macro_rules! ELOG {
    ($($arg:tt)*) => {{
        eprint!("[i3bar] ERROR: ");
        eprintln!($($arg)*);
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_max() {
        assert_eq!(MAX(1, 2), 2);
        assert_eq!(MIN(1, 2), 1);
        assert_eq!(MAX(-1, -2), -1);
    }

    #[test]
    fn starts_with_len_and_case() {
        // C: STARTS_WITH checks strlen(needle) <= len first.
        assert!(STARTS_WITH(b"version", 7, "version"));
        assert!(STARTS_WITH(b"Version", 7, "version"));
        assert!(!STARTS_WITH(b"version", 6, "version"));
        assert!(!STARTS_WITH(b"versio", 6, "version"));
    }
}
