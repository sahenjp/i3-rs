//! 1:1 port of i3bar/src/main.c (198 lines).
//!
//! C: i3bar/src/main.c
//!
//! The libev main loop is replaced by a poll(2) loop (see main_loop()); every
//! step it performs is the same one libev would drive:
//!   * the i3 IPC socket becomes readable -> ipc::got_data()
//!   * a child's stdout becomes readable  -> child::stdin_io_cb() /
//!     child::ws_stdin_io_cb() (or the first-line variant)
//!   * SIGCHLD                            -> child::child_sig_cb()
//!   * SIGTERM/SIGINT/SIGHUP              -> stop the loop

#![allow(non_snake_case)]

mod child;
mod common;
mod config;
mod ipc;
mod live;
mod mode;
mod outputs;
mod parse_json_header;
mod util;
mod workspaces;
mod xcb;

use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};

use i3_core::xcb_util::RustXConn;
use libi3::getopt::{getopt_long, option, Getopt};
use libi3::ipc::I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG;
use x11rb::xcb_ffi::XCBConnection;

use config::{config, config_mut};

/// C: main.c:62 static void sig_cb() — sets a flag instead of ev_unloop().
static STOP_REQUESTED: AtomicI32 = AtomicI32::new(0);

extern "C" fn sig_cb(signum: libc::c_int) {
    // C: case SIGTERM/SIGINT/SIGHUP: DLOG("Got a SIGxxx, stopping"); then
    // ev_unloop(main_loop, EVUNLOOP_ALL).
    STOP_REQUESTED.store(signum, Ordering::SeqCst);
}

extern "C" fn atexit_kill_children() {
    child::kill_children_at_exit();
}

/// C: main.c:79 static void print_usage(char *elf_name)
fn print_usage(elf_name: &str) {
    println!("Usage: {} [-b bar_id] [-s sock_path] [-t] [-h] [-v] [-V]", elf_name);
    println!();
    println!("-b, --bar_id       <bar_id>\tBar ID for which to get the configuration, defaults to the first bar from the i3 config");
    println!("-s, --socket       <sock_path>\tConnect to i3 via <sock_path>");
    println!("-t, --transparency Enable transparency (RGBA colors)");
    println!("-h, --help         Display this help message and exit");
    println!("-v, --version      Display version number and exit");
    println!("-V, --verbose      Enable verbose mode");
    println!();
    println!(" PLEASE NOTE that i3bar will be automatically started by i3");
    println!(" as soon as there is a 'bar' configuration block in your");
    println!(" config file. You should never need to start it manually.");
    println!();
}

/// C: main.c:44 static char *expand_path(char *path) — g_strdup(glob(...)), i.e.
/// tilde expansion (libi3::resolve_tilde, the ported glob() subset i3 uses).
fn expand_path(path: &str) -> String {
    libi3::resolve_tilde(path)
}

fn main() {
    let exit = i3bar_main();
    std::process::exit(exit);
}

/// C: main.c:110 int main(int argc, char **argv)
fn i3bar_main() -> i32 {
    let mut socket_path: Option<String> = None;

    // Initialize the standard config to use 0 as default.
    config_mut(|c| *c = config::config_t::default());

    let mut go = Getopt::new(std::env::args_os().collect());
    let long_options = vec![
        option { name: "socket", has_arg: libi3::getopt::required_argument, val: b's' },
        option { name: "bar_id", has_arg: libi3::getopt::required_argument, val: b'b' },
        option { name: "transparency", has_arg: libi3::getopt::no_argument, val: b't' },
        option { name: "help", has_arg: libi3::getopt::no_argument, val: b'h' },
        option { name: "version", has_arg: libi3::getopt::no_argument, val: b'v' },
        option { name: "verbose", has_arg: libi3::getopt::no_argument, val: b'V' },
    ];

    loop {
        let opt = getopt_long(&mut go, "b:s:thvV", &long_options);
        if opt == -1 {
            break;
        }
        match opt as u8 {
            b's' => {
                let arg = go.optarg.as_ref().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
                socket_path = Some(expand_path(&arg));
            }
            b'v' => {
                println!("i3bar version {} (c) 2010 Axel Wagner and contributors", libi3::i3_version());
                return 0;
            }
            b'b' => {
                let arg = go.optarg.as_ref().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
                config_mut(|c| c.bar_id = Some(arg));
            }
            b't' => config_mut(|c| c.transparency = true),
            b'V' => config_mut(|c| c.verbose = true),
            _ => {
                let elf_name = go.remaining().first().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
                print_usage(&elf_name);
                return 1;
            }
        }
    }

    println!("i3bar version {}", libi3::i3_version());

    // C: main.c:143 xcb_connection_t *xcb_connection = xcb_connect(NULL, &screen)
    // (init_xcb_early() opens it in C; the port opens it here so that main
    // owns the Rc the whole process shares).
    let (conn, screen_idx) = match live::connect() {
        Some(connection) => connection,
        None => {
            eprintln!("[i3bar] ERROR: Cannot open display");
            return 1;
        }
    };
    let mut x = RustXConn::wrap(Rc::clone(&conn), screen_idx);
    let atom_sock_path = xcb::init_xcb_early(&mut x);

    // Select a socket_path if the user hasn't specified one.
    if socket_path.is_none() {
        socket_path = std::env::var("I3SOCK").ok();
    }
    if socket_path.is_none() {
        socket_path = atom_sock_path;
    }

    let socket_path = match socket_path {
        Some(path) => path,
        None => {
            let i3_default_sock_path = "/tmp/i3-ipc.sock";
            eprintln!("[i3bar] ERROR: No socket path specified, default to {}", i3_default_sock_path);
            i3_default_sock_path.to_string()
        }
    };

    // C: main.c:166 init_dpi();
    libi3::dpi::init_dpi(Some(&*conn));

    outputs::init_outputs();

    ipc::init_connection(&socket_path);
    // Request the bar configuration. When it arrives, we fill the config array.
    // In case that config.bar_id is empty, we will receive a list of available
    // configs and then request the configuration for the first bar.
    let bar_id = config(|c| c.bar_id.clone());
    ipc::i3_send_msg(I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG, bar_id.as_deref());

    // We listen to SIGTERM/QUIT/INT and try to exit cleanly, by stopping the
    // main loop.
    unsafe {
        let handler = sig_cb as extern "C" fn(libc::c_int);
        let handler = handler as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGHUP, handler);
        libc::atexit(atexit_kill_children);
        // C: libev's ev_child watcher relies on SIGCHLD being delivered.
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }

    main_loop(&conn, &mut x);

    xcb::clean_xcb(&mut x);
    0
}

/// C: main.c:180 ev_loop(main_loop, 0) — poll(2)-based replacement.
///
/// The watchers of the C loop map to one poll(2) set:
///   * the X connection fd (C: xcb_io, whose callback is empty; the events are
///     drained by xcb_prep_cb, here after every wakeup)
///   * the i3 IPC socket (gets fed to ipc::got_data)
///   * the two child stdout pipes (child.c's ev_io watchers)
/// SIGCHLD/SIGTERM interrupt poll(), exactly like libev's signal watchers.
fn main_loop(conn: &Rc<XCBConnection>, x: &mut RustXConn) {
    let mut draw = live::LiveDraw::new(Rc::clone(conn));
    let x_fd = {
        // C: xcb_get_file_descriptor(xcb_connection) — the xcb_ffi connection
        // exposes the fd of the libxcb connection it wraps.
        use std::os::unix::io::AsRawFd;
        AsRawFd::as_raw_fd(&**conn)
    };

    loop {
        if STOP_REQUESTED.load(Ordering::SeqCst) != 0 {
            break;
        }

        // C: xcb_prep_cb — every pending X event is dispatched before the loop
        // blocks again.
        let events = live::poll_events(conn);
        if !events.is_empty() {
            xcb::xcb_prep_cb(x, &mut draw, &events);
        }

        let ipc_fd = ipc::i3_connection_fd();
        let status_fd = child::status_child(|c| c.stdin_fd);
        let ws_fd = child::ws_child(|c| c.stdin_fd);

        let mut fds: Vec<libc::pollfd> = Vec::with_capacity(4);
        fds.push(libc::pollfd { fd: x_fd, events: libc::POLLIN, revents: 0 });
        if ipc_fd > 0 {
            fds.push(libc::pollfd { fd: ipc_fd, events: libc::POLLIN, revents: 0 });
        }
        if status_fd > 0 {
            fds.push(libc::pollfd { fd: status_fd, events: libc::POLLIN, revents: 0 });
        }
        if ws_fd > 0 {
            fds.push(libc::pollfd { fd: ws_fd, events: libc::POLLIN, revents: 0 });
        }

        // C: ev_loop() sleeps in the kernel until X/IPC/child input or a
        // signal arrives; there is no periodic timer in i3bar.
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                reap_children(x, &mut draw);
                continue;
            }
            eprintln!("[i3bar] ERROR: poll() failed: {}", err);
            break;
        }

        for pfd in fds.iter() {
            if pfd.revents == 0 {
                continue;
            }
            if pfd.fd == x_fd {
                // Drained at the top of the next iteration (C: xcb_prep_cb).
            } else if pfd.fd == ipc_fd {
                ipc::got_data(x, &mut draw);
            } else if pfd.fd == status_fd {
                // C: status_child.version == (uint32_t)-1 -> first line
                if child::status_child(|c| c.version) == u32::MAX {
                    child::stdin_io_first_line_cb(x, &mut draw, pfd.fd);
                } else {
                    child::stdin_io_cb(x, &mut draw, pfd.fd);
                }
            } else if pfd.fd == ws_fd {
                child::ws_stdin_io_cb(x, &mut draw, pfd.fd);
            }
        }

        reap_children(x, &mut draw);
    }
}

/// C: the ev_child watcher — waitpid() for every exited child.
fn reap_children(x: &mut RustXConn, draw: &mut live::LiveDraw) {
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
        child::child_sig_cb(x, draw, pid, status);
    }
}
