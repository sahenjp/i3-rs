//! Integration tests: byte-compare the CLI surface against the recorded C
//! baseline (baseline/i3-msg-*.txt, recorded from the C build) and exercise
//! a real IPC round trip against a stub i3 server speaking the i3-ipc
//! frame protocol (magic "i3-ipc", u32 size, u32 type, native endian).

//! Baseline byte-comparison tests (C: i3-msg/main.c:212-217 and the
//! ipc_connect failure path, recorded in baseline/i3-msg-*.txt).

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// The i3-msg binary of this crate.
const BIN: &str = env!("CARGO_BIN_EXE_i3-msg");

/// The recorded C baseline outputs.
const BASELINE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../baseline");

/// The recorded baseline was produced by the C build with
/// I3_VERSION "4.25.1 (2026-02-06)"; the Rust i3_version() is overridden at
/// runtime with I3RS_VERSION so the byte comparison stays meaningful.
const BASELINE_VERSION: &str = "4.25.1 (2026-02-06)";

fn read_baseline(name: &str) -> String {
    std::fs::read_to_string(PathBuf::from(BASELINE_DIR).join(name))
        .unwrap_or_else(|e| panic!("baseline {} : {}", name, e))
}

fn run(args: &[&str], env: &[(&str, Option<&str>)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.stdin(Stdio::null());
    for (k, v) in env {
        match v {
            Some(v) => {
                cmd.env(k, v);
            }
            None => {
                cmd.env_remove(k);
            }
        }
    }
    cmd.args(args).output().expect("spawn i3-msg")
}

fn read_frame(stream: &mut UnixStream) -> (u32, Vec<u8>) {
    let mut header = [0u8; 14];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(&header[0..6], b"i3-ipc", "bad magic from i3-msg");
    let size = u32::from_ne_bytes(header[6..10].try_into().unwrap());
    let r#type = u32::from_ne_bytes(header[10..14].try_into().unwrap());
    let mut payload = vec![0u8; size as usize];
    stream.read_exact(&mut payload).unwrap();
    (r#type, payload)
}

fn write_frame(stream: &mut UnixStream, r#type: u32, payload: &[u8]) {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"i3-ipc");
    frame.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    frame.extend_from_slice(&r#type.to_ne_bytes());
    frame.extend_from_slice(payload);
    stream.write_all(&frame).unwrap();
}

/// Bind a stub server socket in a fresh temp dir; returns (path, listener).
fn bind_stub(name: &str) -> (PathBuf, UnixListener) {
    let dir = std::env::temp_dir().join(format!("i3-msg-cli-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    (path, listener)
}

/// (a) --help matches the recorded C output byte for byte.
/// i3-msg's help output contains no argv[0] (unlike i3 itself), so no
/// argv[0] normalization is needed.

#[test]
fn help_matches_baseline() {
    let out = run(&["--help"], &[("I3RS_VERSION", Some(BASELINE_VERSION))]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty(), "stderr: {:?}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), read_baseline("i3-msg-help.txt"));
}


/// (b) --version matches the recorded C output byte for byte.
#[test]
fn version_matches_baseline() {
    let out = run(&["--version"], &[("I3RS_VERSION", Some(BASELINE_VERSION))]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), read_baseline("i3-msg-version.txt"));
}


/// -h is the same output as --help (C: i3-msg/main.c:214-217).
#[test]
fn h_matches_help() {
    let out = run(&["-h"], &[("I3RS_VERSION", Some(BASELINE_VERSION))]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), read_baseline("i3-msg-help.txt"));
}


/// (c) Without any IPC channel (no -s, no $I3SOCK, no X11), the C binary
/// dies with err(EXIT_FAILURE, "Could not determine i3 socket path") —
/// stderr "i3-msg: Could not determine i3 socket path: Success" (errno is
/// still 0 → strerror(0) = "Success") and exit code 1, empty stdout.
#[test]
fn no_ipc_socket_matches_baseline() {
    let out = run(
        &["nop"],
        &[("I3SOCK", None), ("DISPLAY", None), ("I3RS_VERSION", Some(BASELINE_VERSION))],
    );
    assert_eq!(out.status.code(), Some(1), "stderr: {:?}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty(), "stdout: {:?}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        read_baseline("i3-msg-noconn-err.txt"),
        "stderr mismatch"
    );
}


#[test]
fn roundtrip_command_success() {
    let (path, listener) = bind_stub("ok");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (r#type, payload) = read_frame(&mut stream);
        assert_eq!(r#type, 0); // I3_IPC_MESSAGE_TYPE_RUN_COMMAND
        assert_eq!(payload, b"nop"); // the joined remaining args
        write_frame(&mut stream, 0, br#"{"success": true}"#);
    });

    let out = run(&["nop"], &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)]);
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {:?}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{\"success\": true}\n");
    assert!(out.stderr.is_empty());
}


/// -t get_version: the type name resolves to 7 and the reply type must
/// match it, or i3-msg errx()s (type mismatch checked in main.c:265).
#[test]
fn roundtrip_get_version() {
    let (path, listener) = bind_stub("ver");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (r#type, _) = read_frame(&mut stream);
        assert_eq!(r#type, 7); // I3_IPC_MESSAGE_TYPE_GET_VERSION
        write_frame(&mut stream, 7, br#"{"major": 4, "minor": 25, "patch": 1}"#);
    });

    let out = run(
        &["-t", "get_version"],
        &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)],
    );
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{\"major\": 4, \"minor\": 25, \"patch\": 1}\n");
}


/// A failed command reply: exit_code becomes 2, the nicely formatted error
/// goes to stderr (main.c:89-99 reply_end_map_cb) and the raw reply still
/// goes to stdout (not quiet).
#[test]
fn roundtrip_command_failure_exit_code_2() {
    let (path, listener) = bind_stub("fail");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream);
        write_frame(
            &mut stream,
            0,
            br#"[{"success": false, "error": "BAD", "input": "nop", "errorposition": "oops"}]"#,
        );
    });

    let out = run(&["nop"], &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)]);
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "[{\"success\": false, \"error\": \"BAD\", \"input\": \"nop\", \"errorposition\": \"oops\"}]\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "ERROR: Your command: nop\nERROR:               oops\nERROR: BAD\n"
    );
}


/// -q suppresses the reply output but the exit code stays 2.
#[test]
fn quiet_silences_output() {
    let (path, listener) = bind_stub("quiet");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream);
        write_frame(&mut stream, 0, br#"{"success": true}"#);
    });

    let out = run(&["-q", "nop"], &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)]);
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "stdout: {:?}", String::from_utf8_lossy(&out.stdout));
}


/// -t subscribe: after the SUBSCRIBE reply (type 2), i3-msg waits for real
/// events (type | I3_IPC_EVENT_MASK); without -m exactly one event is
/// printed and the loop ends (do..while(monitor), main.c:304-322).
#[test]
fn subscribe_receives_one_event_without_monitor() {
    let (path, listener) = bind_stub("sub");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (r#type, payload) = read_frame(&mut stream);
        assert_eq!(r#type, 2); // I3_IPC_MESSAGE_TYPE_SUBSCRIBE
        assert_eq!(payload, br#"["workspace"]"#);
        write_frame(&mut stream, 2, br#"{"success": true}"#);
        // One event, then close: the client exits after the first event
        // because monitor == false.
        write_frame(&mut stream, 0x8000_0000, br#"{"change": "focus"}"#);
    });

    let out = run(
        &["-t", "subscribe", "[\"workspace\"]"],
        &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)],
    );
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {:?}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{\"change\": \"focus\"}\n");
}


/// With -m the client keeps going until EOF; EOF (ret == -2) exits 1
/// silently (main.c:307-311).
#[test]
fn monitor_until_eof_exits_1() {
    let (path, listener) = bind_stub("mon");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream);
        write_frame(&mut stream, 2, br#"{"success": true}"#);
        write_frame(&mut stream, 0x8000_0000, br#"{"first": true}"#);
        // drop → EOF on the client's next read
    });

    let out = run(
        &["-m", "-t", "subscribe", "[]"],
        &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)],
    );
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{\"first\": true}\n");
    assert!(out.stderr.is_empty(), "stderr: {:?}", String::from_utf8_lossy(&out.stderr));
}


/// A non-event reply inside the subscribe loop errx()s (main.c:314-316).
#[test]
fn subscribe_non_event_reply_is_fatal() {
    let (path, listener) = bind_stub("nonev");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream);
        write_frame(&mut stream, 2, br#"{"success": true}"#);
        // Not an event (no event mask bit set):
        write_frame(&mut stream, 4, br#"{}"#);
    });

    let out = run(
        &["-t", "subscribe", "[]"],
        &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)],
    );
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "i3-msg: IPC: Received reply of type 4 but expected an event\n"
    );
}


/// -t bogus prints the unknown-type listing on stdout and exits 1
/// (main.c:203-205).
#[test]
fn unknown_message_type() {
    let out = run(&["-t", "bogus"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Unknown message type\nKnown types: run_command, get_workspaces, get_outputs, get_tree, get_marks, get_bar_config, get_binding_modes, get_binding_state, get_version, get_config, send_tick, subscribe\n"
    );
}


/// -m without -t subscribe is rejected (main.c:225-228).
#[test]
fn monitor_requires_subscribe() {
    let out = run(&["-m", "nop"], &[("DISPLAY", None)]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "The monitor option -m is used with -t SUBSCRIBE exclusively.\n"
    );
}


/// An unmatched long option prints glibc's message and exits 1
/// (getopt_long returns '?', main.c:218-219).
#[test]
fn unrecognized_long_option() {
    let out = run(&["--bogus"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unrecognized option '--bogus'"),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}



#[test]
fn raw_config_reply_works() {
    let (path, listener) = bind_stub("rawcfg2");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (r#type, _) = read_frame(&mut stream);
        assert_eq!(r#type, 9); // I3_IPC_MESSAGE_TYPE_GET_CONFIG
        write_frame(&mut stream, 9, br#"{"success": true, "config": body"}"#);
    });

    let out = run(
        &["-r", "-t", "get_config"],
        &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)],
    );
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "code mismatch");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\"success\": true, \"config\": body\"}\n"
    );
}


/// Non-raw -t get_config: the parsed "config" member is written to stdout
/// with no trailing newline (config_string_cb, main.c:121-128).
#[test]
fn parsed_config_reply_prints_config_member() {
    let (path, listener) = bind_stub("cfg");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream);
        write_frame(&mut stream, 9, br#"{"success": true, "config": "cfg-body"}"#);
    });

    let out = run(
        &["-t", "get_config"],
        &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)],
    );
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "cfg-body");
}



/// A failed command reply without error/input keys: glibc printf prints
/// "(null)" for the NULL %s arguments (main.c:89-99 kept 1:1).
#[test]
fn roundtrip_command_failure_without_keys() {
    let (path, listener) = bind_stub("failnull");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream);
        write_frame(&mut stream, 0, br#"{"success": false}"#);
    });

    let out = run(&["nop"], &[("I3SOCK", Some(path.to_str().unwrap())), ("DISPLAY", None)]);
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "ERROR: (null)\n");
}
