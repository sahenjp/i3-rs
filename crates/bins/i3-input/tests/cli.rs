//! The observable command line behaviour of i3-input, compared against
//! C: i3-input/main.c:409-440 and :448-452.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i3-input");

fn version() -> String {
    std::env::var("I3RS_VERSION").unwrap_or_else(|_| "4.25-non-git".to_string())
}

#[test]
fn version_is_printed_like_c() {
    // C: i3-input/main.c:409-411
    let out = Command::new(BIN).arg("-v").output().expect("run i3-input -v");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("i3-input {}\n", version())
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn help_is_printed_like_c() {
    // C: i3-input/main.c:433-440
    let out = Command::new(BIN).arg("-h").output().expect("run i3-input -h");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "i3-input {}\n\
             i3-input [-s <socket>] [-F <format>] [-l <limit>] [-P <prompt>] [-f <font>] [-v]\n\
             \n\
             Example:\n\
             \x20   i3-input -F 'workspace \"%s\"' -P 'Switch to workspace: '\n",
            version()
        )
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn the_deprecated_prefix_option_warns_and_is_used_as_a_format() {
    // C: i3-input/main.c:412-417 — the warning goes to stderr, the format is
    // "<prefix>%s". Without a display the process then dies like C's die().
    let out = Command::new(BIN)
        .args(["-p", "exec "])
        .env_remove("DISPLAY")
        .env_remove("I3SOCK")
        .output()
        .expect("run i3-input -p");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "i3-input: WARNING: the -p option is DEPRECATED in favor of the -F (format) option\n\
         i3-input: Cannot open display\n"
    );
    assert_eq!(out.status.code(), Some(1));
    // The format is printed after the option loop (C: :446).
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "using format \"exec %s\"\n"
    );
}

#[test]
fn without_display_the_error_matches_c() {
    // C: i3-input/main.c:448-452 — die("Cannot open display").
    let out = Command::new(BIN)
        .env_remove("DISPLAY")
        .env_remove("I3SOCK")
        .output()
        .expect("run i3-input without DISPLAY");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "using format \"%s\"\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "i3-input: Cannot open display\n"
    );
}
