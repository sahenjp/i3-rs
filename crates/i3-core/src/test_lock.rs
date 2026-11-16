//! Process-global state guards for the test suite.
//!
//! C keeps its parser state in file statics and its working directory in the
//! process; the port mirrors that. cargo runs the tests of one binary in
//! parallel, so every test that touches those globals (parse_file chdir, the
//! included-file list, the variable list) must take this lock.

#![cfg(test)]

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serializes tests that change the process working directory or the config
/// parser globals (C: src/config.c / config_parser.c statics).
pub fn cwd_lock() -> MutexGuard<'static, ()> {
    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    CWD_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
