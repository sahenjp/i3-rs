//! The observable command line behaviour of i3-config-wizard, compared against
//! C: i3-config-wizard/main.c:807-866.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_i3-config-wizard");

fn version() -> String {
    std::env::var("I3RS_VERSION").unwrap_or_else(|_| "4.25-non-git".to_string())
}

fn temp_xdg(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "i3rs-wizard-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn version_is_printed_like_c() {
    // C: main.c:807-809
    let out = Command::new(BIN)
        .arg("-v")
        .output()
        .expect("run i3-config-wizard -v");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("i3-config-wizard {}\n", version())
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn help_is_printed_like_c() {
    // C: main.c:820-823
    let out = Command::new(BIN)
        .arg("-h")
        .output()
        .expect("run i3-config-wizard -h");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "i3-config-wizard {}\ni3-config-wizard [-s <socket>] [-m win|alt] [-v] [-h]\n",
            version()
        )
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn an_existing_config_makes_the_wizard_exit_immediately() {
    // C: main.c:827-832 — get_config_path() finds the file and the wizard does
    // not touch X11 at all.
    let xdg = temp_xdg("exists");
    std::fs::create_dir_all(xdg.join("i3")).unwrap();
    let config = xdg.join("i3").join("config");
    std::fs::write(&config, "# existing\n").unwrap();

    let out = Command::new(BIN)
        .env("XDG_CONFIG_HOME", &xdg)
        .env_remove("DISPLAY")
        .output()
        .expect("run i3-config-wizard with an existing config");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "The config file \"{}\" already exists. Exiting.\n",
            config.display()
        )
    );
    let _ = std::fs::remove_dir_all(&xdg);
}

#[test]
fn an_invalid_modifier_is_rejected() {
    // C: main.c:810-818 — err(EXIT_FAILURE, "Invalid modifier key %s", optarg)
    let out = Command::new(BIN)
        .args(["-m", "hyper"])
        .env_remove("DISPLAY")
        .output()
        .expect("run i3-config-wizard -m hyper");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("i3-config-wizard: Invalid modifier key hyper: "),
        "unexpected stderr: {:?}",
        stderr
    );
}

#[test]
fn without_display_the_error_matches_c() {
    // C: main.c:862-866 — errx(1, "Cannot open display"). The config file is
    // created and unlinked before, so XDG_CONFIG_HOME must point at a scratch
    // directory.
    let xdg = temp_xdg("nodisplay");
    let out = Command::new(BIN)
        .env("XDG_CONFIG_HOME", &xdg)
        .env_remove("DISPLAY")
        .env_remove("I3SOCK")
        .output()
        .expect("run i3-config-wizard without DISPLAY");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "i3-config-wizard: Cannot open display\n"
    );
    // The i3 config directory was created (C: :842-852) and the temporary
    // config file was removed again (C: :854-860).
    assert!(xdg.join("i3").is_dir());
    assert!(!xdg.join("i3").join("config").exists());
    let _ = std::fs::remove_dir_all(&xdg);
}
