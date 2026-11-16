//! C: libi3/strndup.c
//!
//! The fallback strndup() for platforms without HAVE_STRNDUP is not needed
//! in Rust: string slicing (&str[..n] / String::truncate) provides the same
//! semantics with bounds checking, and every C sstrndup(str, n) call site in
//! the port maps to a Rust slice. The module is kept so the C-to-Rust file
//! correspondence stays complete.
