//! The observable command line behaviour of i3-nagbar, compared against
//! C: i3-nagbar/main.c:433-472 and :477-480.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i3-nagbar");

fn version() -> String {
    std::env::var("I3RS_VERSION").unwrap_or_else(|_| "4.25-non-git".to_string())
}

#[test]
fn version_is_printed_like_c() {
    // C: i3-nagbar/main.c:433-436
    let out = Command::new(BIN).arg("-v").output().expect("run i3-nagbar -v");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("i3-nagbar {}\n", version())
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn help_is_printed_like_c() {
    // C: i3-nagbar/main.c:448-452
    let out = Command::new(BIN).arg("-h").output().expect("run i3-nagbar -h");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "i3-nagbar {}\n\
             i3-nagbar [-m <message>] [-b <button> <action>] [-B <button> <action>] [-t warning|error] [-f <font>] [-v] [-p]\n",
            version()
        )
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn buttons_are_announced_on_stdout_like_c() {
    // C: i3-nagbar/main.c:462-466 — two debug printfs per button, then the
    // display dies because there is no X server.
    let out = Command::new(BIN)
        .args(["-b", "edit config", "true", "-B", "show errors", "false"])
        .env_remove("DISPLAY")
        .output()
        .expect("run i3-nagbar with buttons");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "button with label *edit config* and action *true*\n\
         now 1 buttons\n\
         button with label *show errors* and action *false*\n\
         now 2 buttons\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "i3-nagbar: Cannot open display\n"
    );
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn without_display_the_error_matches_c() {
    // C: i3-nagbar/main.c:477-480 — die("Cannot open display").
    let out = Command::new(BIN)
        .env_remove("DISPLAY")
        .output()
        .expect("run i3-nagbar without DISPLAY");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "i3-nagbar: Cannot open display\n"
    );
}
