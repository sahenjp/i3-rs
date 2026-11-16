//! The observable command line behaviour of i3-dump-log, compared against the
//! strings of C: i3-dump-log/main.c:107-126 and :133-146.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i3-dump-log");

/// C: config.h I3_VERSION — the Rust build embeds the same fallback string
/// (libi3::i3_version, overridable with $I3RS_VERSION).
fn version() -> String {
    std::env::var("I3RS_VERSION").unwrap_or_else(|_| "4.25-non-git".to_string())
}

#[test]
fn version_is_printed_like_c() {
    // C: i3-dump-log/main.c:108-110
    let out = Command::new(BIN).arg("-v").output().expect("run i3-dump-log -v");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("i3-dump-log {}\n", version())
    );
    assert_eq!(out.status.code(), Some(0));

    // The long option is the same flag.
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("i3-dump-log {}\n", version())
    );
}

#[test]
fn help_is_printed_like_c() {
    // C: i3-dump-log/main.c:117-125
    let out = Command::new(BIN).arg("-h").output().expect("run i3-dump-log -h");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("i3-dump-log {}\ni3-dump-log [-fhVv]\n", version())
    );
    assert_eq!(out.status.code(), Some(0));

    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("i3-dump-log {}\ni3-dump-log [-fhVv]\n", version())
    );
}

#[test]
fn without_display_the_error_matches_c() {
    // C: i3-dump-log/main.c:137-143
    let out = Command::new(BIN)
        .env_remove("DISPLAY")
        .env_remove("I3SOCK")
        .output()
        .expect("run i3-dump-log without DISPLAY");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "i3-dump-log: ERROR: Cannot connect to X11.\n\n\
         Your DISPLAY environment variable is not set.\n\
         Are you running i3-dump-log via SSH or on a virtual console?\n\
         Try DISPLAY=:0 i3-dump-log\n"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

#[test]
fn with_a_broken_display_the_error_mentions_it() {
    // C: i3-dump-log/main.c:144
    let out = Command::new(BIN)
        .env("DISPLAY", ":12345")
        .env_remove("I3SOCK")
        .output()
        .expect("run i3-dump-log with a broken DISPLAY");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "i3-dump-log: ERROR: Cannot connect to X11.\n\n\
         FYI: The DISPLAY environment variable is set to \":12345\".\n"
    );
}
