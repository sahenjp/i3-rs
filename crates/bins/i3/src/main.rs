//! i3: the window manager binary — 1:1 port of C: src/main.c (1231 lines,
//! i3 4.25 @ 8a97b0f).
//!
//! The C file is one long main() with a fixed startup order. That order is
//! load-bearing (WM_Sn acquisition races, the ICCCM timestamp handshake, the
//! grab_server/manage_existing_windows dance), so it is kept verbatim here.
//!
//! Every comment starting with `// C:` points at the original statement.

use std::process::ExitCode;

use i3_core::config::{config_load_t, load_configuration};
use i3_core::config_directives::ConfigDirectives;

use libi3::getopt::{getopt_long, option, Getopt};

mod server;

/// C: src/main.c:60-84 — the globals, grouped exactly like the C declarations.
pub struct I3Globals {
    /// C: bool autostart
    pub autostart: bool,
    /// C: bool only_check_config
    pub only_check_config: bool,
    /// C: bool replace_wm
    pub replace_wm: bool,
    /// C: bool force_xinerama
    pub force_xinerama: bool,
    /// C: bool disable_randr15
    pub disable_randr15: bool,
    /// C: bool disable_signalhandler
    pub disable_signalhandler: bool,
    /// C: char *override_configpath
    pub override_configpath: Option<String>,
    /// C: char *layout_path
    pub layout_path: Option<String>,
    /// C: bool delete_layout_path
    pub delete_layout_path: bool,
    /// C: char *fake_outputs
    pub fake_outputs: Option<String>,
    /// C: int shmlog_size
    pub shmlog_size: usize,
    /// C: xcb_timestamp_t last_timestamp
    pub last_timestamp: u32,
    /// C: int conn_screen
    pub conn_screen: usize,
    /// C: xcb_window_t root
    pub root: u32,
    /// C: xcb_window_t wm_sn_selection_owner
    pub wm_sn_selection_owner: u32,
    /// C: xcb_atom_t wm_sn
    pub wm_sn: u32,
    /// C: uint8_t root_depth
    pub root_depth: u8,
    /// C: xcb_colormap_t colormap
    pub colormap: u32,
}

impl Default for I3Globals {
    fn default() -> I3Globals {
        I3Globals {
            // C: main.c:84 static bool autostart = true
            autostart: true,
            only_check_config: false,
            replace_wm: false,
            force_xinerama: false,
            disable_randr15: false,
            disable_signalhandler: false,
            override_configpath: None,
            layout_path: None,
            // C: main.c:87 static bool delete_layout_path = false
            delete_layout_path: false,
            fake_outputs: None,
            shmlog_size: 0,
            last_timestamp: 0,
            conn_screen: 0,
            root: 0,
            wm_sn_selection_owner: 0,
            wm_sn: 0,
            root_depth: 0,
            colormap: 0,
        }
    }
}

/// C: src/main.c:150 int main(int argc, char *argv[])
fn main() -> ExitCode {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match real_main(argv) {
        Ok(code) => code,
        Err(msg) => {
            // C: err/errx print to stderr and exit; keep the same shape.
            eprintln!("i3: {}", msg);
            ExitCode::FAILURE
        }
    }
}

/// The body of main(), split out only so tests can call it (C: src/main.c:150).
pub fn real_main(argv: Vec<std::ffi::OsString>) -> Result<ExitCode, String> {
    let mut g = I3Globals::default();
    let argv0 = argv
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_else(|| "i3".to_string());

    // C: main.c:318 setlocale(LC_ALL, "");
    // (glibc: the C locale stays unless this is called — the log time prefix
    // is printed with %x %X and would otherwise not be localised.)
    unsafe {
        libc::setlocale(libc::LC_ALL, b"\0".as_ptr() as *const libc::c_char);
    }

    // C: main.c:327 srand(time(NULL));
    let _srand_seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // C: main.c:331 init_logging();
    i3_core::log::init_logging();

    // C: main.c:334 shmlog_size = (is_debug_build() || strstr(argv[0],
    //     "i3-with-shmlog") != NULL ? default_shmlog_size : 0);
    g.shmlog_size = if libi3::is_debug_build() || argv0.contains("i3-with-shmlog") {
        i3_core::log::default_shmlog_size()
    } else {
        0
    };
    // C: main.c:334 assigns the log.c global shmlog_size directly; the port
    // keeps the CLI copy in g.shmlog_size, so the logging layer is updated
    // here (init_logging() at main.c:545 then opens the buffer).
    i3_core::log::set_shmlog_size(g.shmlog_size);

    // C: main.c:338 while ((opt = getopt_long(argc, argv, "c:CvmaL:hld:Vr",
    //     long_options, &option_index)) != -1)
    let long_options = [
        option { name: "no-autostart", has_arg: 0, val: b'a' },
        option { name: "config", has_arg: 1, val: b'c' },
        option { name: "validate", has_arg: 0, val: b'C' },
        option { name: "version", has_arg: 0, val: b'v' },
        option { name: "moreversion", has_arg: 0, val: b'm' },
        option { name: "verbose", has_arg: 0, val: b'V' },
        option { name: "layout", has_arg: 1, val: b'L' },
        option { name: "help", has_arg: 0, val: b'h' },
        option { name: "force-xinerama", has_arg: 0, val: 0 },
        option { name: "force_xinerama", has_arg: 0, val: 0 },
        option { name: "disable-randr15", has_arg: 0, val: 0 },
        option { name: "disable_randr15", has_arg: 0, val: 0 },
        option { name: "disable-signalhandler", has_arg: 0, val: 0 },
        option { name: "get-socketpath", has_arg: 0, val: 0 },
        option { name: "get_socketpath", has_arg: 0, val: 0 },
        option { name: "shmlog-size", has_arg: 1, val: 0 },
        option { name: "shmlog_size", has_arg: 1, val: 0 },
        option { name: "restart", has_arg: 1, val: 0 },
        option { name: "fake-outputs", has_arg: 1, val: 0 },
        option { name: "fake_outputs", has_arg: 1, val: 0 },
        option { name: "replace", has_arg: 0, val: 0 },
        option { name: "force-old-config-parser-v4.4-only", has_arg: 0, val: 0 },
    ];

    let mut go = Getopt::new(argv.clone());
    loop {
        let opt = getopt_long(&mut go, "c:CvmaL:hld:Vr", &long_options);
        // C: getopt_long returns -1 when done
        if opt == -1 {
            break;
        }
        let optarg: Option<String> = go
            .optarg
            .clone()
            .map(|o| o.to_string_lossy().into_owned());
        match opt {
            // C: main.c:340 case 'a': autostart = false;
            v if v == b'a' as i32 => {
                g.autostart = false;
            }
            // C: main.c:344 case 'L': layout_path = strdup(optarg);
            v if v == b'L' as i32 => {
                g.layout_path = optarg.clone();
                g.delete_layout_path = false;
            }
            // C: main.c:349 case 'c': override_configpath = strdup(optarg);
            v if v == b'c' as i32 => {
                g.override_configpath = optarg.clone();
            }
            // C: main.c:353 case 'C': only_check_config = true;
            v if v == b'C' as i32 => {
                g.only_check_config = true;
            }
            // C: main.c:357 case 'v': printf("i3 version %s ...")
            v if v == b'v' as i32 => {
                println!(
                    "i3 version {} © 2009 Michael Stapelberg and contributors",
                    libi3::i3_version()
                );
                return Ok(ExitCode::SUCCESS);
            }
            // C: main.c:361 case 'm': binary + running version
            v if v == b'm' as i32 => {
                println!(
                    "Binary i3 version:  {} © 2009 Michael Stapelberg and contributors",
                    libi3::i3_version()
                );
                i3_core::display_version::display_running_version();
                return Ok(ExitCode::SUCCESS);
            }
            // C: main.c:366 case 'V': set_verbosity(true);
            v if v == b'V' as i32 => {
                i3_core::log::set_verbosity(true);
            }
            // C: main.c:369 case 'd': set_debug_logging(true);
            v if v == b'd' as i32 => {
                i3_core::log::set_debug_logging(true);
            }
            // C: main.c:373 case 'l': deprecated, ignored
            v if v == b'l' as i32 => {}
            // C: main.c:376 case 'r': replace_wm = true;
            v if v == b'r' as i32 => {
                g.replace_wm = true;
            }
            // C: main.c:379 case 0: long-only options
            0 => {
                let name = go.long_option_name();
                match name.as_str() {
                    // C: main.c:380-388 --force-xinerama
                    "force-xinerama" | "force_xinerama" => {
                        g.force_xinerama = true;
                        i3_core::log::errorlog(
                            "Using Xinerama instead of RandR. This option should be avoided at all cost because it does not refresh the list of screens, so you cannot configure displays at runtime. Please check if your driver really does not support RandR and disable this option as soon as you can.\n",
                        );
                    }
                    // C: main.c:389-392
                    "disable-randr15" | "disable_randr15" => {
                        g.disable_randr15 = true;
                    }
                    // C: main.c:393-395
                    "disable-signalhandler" => {
                        g.disable_signalhandler = true;
                    }
                    // C: main.c:396-409 --get-socketpath
                    "get-socketpath" | "get_socketpath" => {
                        match libi3::root_atom_contents("I3_SOCKET_PATH", None, 0) {
                            Some(path) => {
                                println!("{}", path);
                                return Ok(ExitCode::SUCCESS);
                            }
                            None => return Ok(ExitCode::FAILURE),
                        }
                    }
                    // C: main.c:410-417 --shmlog-size
                    "shmlog-size" | "shmlog_size" => {
                        g.shmlog_size = optarg
                            .as_deref()
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(0);
                        // C: main.c:412-415 — shmlog_size = atoi(optarg);
                        // init_logging();
                        i3_core::log::set_shmlog_size(g.shmlog_size);
                        i3_core::log::init_logging();
                    }
                    // C: main.c:418-422 --restart
                    "restart" => {
                        g.layout_path = optarg.clone();
                        g.delete_layout_path = true;
                    }
                    // C: main.c:423-427 --fake-outputs
                    "fake-outputs" | "fake_outputs" => {
                        g.fake_outputs = optarg.clone();
                    }
                    // C: main.c:428-431 --force-old-config-parser-v4.4-only
                    "force-old-config-parser-v4.4-only" => {
                        i3_core::log::errorlog(
                            "You are passing --force-old-config-parser-v4.4-only, but that flag was removed by now.\n",
                        );
                    }
                    // C: main.c:432 fall-through to default
                    _ => {
                        print_usage(&argv0, g.shmlog_size);
                        return Ok(ExitCode::FAILURE);
                    }
                }
            }
            // C: main.c:433 default: usage, exit(opt == 'h' ? SUCCESS : FAILURE)
            _ => {
                print_usage(&argv0, g.shmlog_size);
                return Ok(if opt == b'h' as i32 {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                });
            }
        }
    }

    // C: main.c:474 if (only_check_config) exit(load_configuration(...));
    if g.only_check_config {
        let mut d = ConfigDirectives::default();
        let ok = load_configuration(
            &mut d,
            g.override_configpath.as_deref(),
            config_load_t::C_VALIDATE,
        );
        return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE });
    }

    // C: main.c:481 if (optind < argc) — act like i3-msg
    let rest = go.remaining();
    if !rest.is_empty() {
        i3_core::log::set_verbosity(true);
        i3_core::log::log_("Additional arguments passed. Sending them as a command to i3.\n");
        let payload = rest
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        let code = send_command_via_ipc(&payload)?;
        return Ok(code);
    }

    // C: main.c:576 LOG("i3 %s starting\n", _i3_version);
    i3_core::log::log_(&format!("i3 {} starting\n", libi3::i3_version()));

    // C: main.c:578 conn = xcb_connect(NULL, &conn_screen);
    // The port opens an xcb_ffi connection: libxkbcommon-x11 takes the raw
    // xcb_connection_t *, which the pure-Rust connection cannot provide.
    let (conn, screen_num) = match i3_core::xcb_util::connect() {
        Ok(v) => v,
        // C: errx(EXIT_FAILURE, "Cannot open display");
        Err(_) => return Err("Cannot open display".to_string()),
    };
    g.conn_screen = screen_num;

    // C: main.c:597 root_screen = xcb_aux_get_screen(conn, conn_screen);
    let setup = x11rb::connection::Connection::setup(&conn);
    let root_screen = match setup.roots.get(screen_num) {
        Some(s) => s,
        None => return Err("Cannot open display".to_string()),
    };
    g.root = root_screen.root;
    g.root_depth = root_screen.root_depth;
    g.colormap = root_screen.default_colormap;

    // C: main.c:585-1231 — everything else happens in the event loop setup.
    // C: main.c:1231 — i3_restart() execvp()s the original argv (with -a and
    // --restart <layout> appended), so the whole vector is handed down.
    let restart_argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    server::run(&conn, &mut g, screen_num, &restart_argv)
}

/// C: src/main.c:434-470 — the usage text, byte for byte.
fn print_usage(argv0: &str, shmlog_size: usize) {
    eprintln!("Usage: {} [-c configfile] [-d all] [-a] [-v] [-V] [-C]", argv0);
    eprintln!();
    eprintln!("\t-a          disable autostart ('exec' lines in config)");
    eprintln!("\t-c <file>   use the provided configfile instead");
    eprintln!("\t-C          validate configuration file and exit");
    eprintln!("\t-d all      enable debug output");
    eprintln!("\t-L <file>   path to the serialized layout during restarts");
    eprintln!("\t-v          display version and exit");
    eprintln!("\t-V          enable verbose mode");
    eprintln!();
    eprintln!("\t--force-xinerama");
    eprintln!("\tUse Xinerama instead of RandR.");
    eprintln!("\tThis option should only be used if you are stuck with the");
    eprintln!("\told nVidia closed source driver (older than 302.17), which does");
    eprintln!("\tnot support RandR.");
    eprintln!();
    eprintln!("\t--get-socketpath");
    eprintln!("\tRetrieve the i3 IPC socket path from X11, print it, then exit.");
    eprintln!();
    eprintln!("\t--shmlog-size <limit>");
    eprintln!("\tLimits the size of the i3 SHM log to <limit> bytes. Setting this");
    eprintln!("\tto 0 disables SHM logging entirely.");
    eprintln!("\tThe default is {} bytes.", shmlog_size);
    eprintln!();
    eprintln!("\t--replace");
    eprintln!("\tReplace an existing window manager.");
    eprintln!();
    eprintln!("If you pass plain text arguments, i3 will interpret them as a command");
    eprintln!("to send to a currently running i3 (like i3-msg). This allows you to");
    eprintln!("use nice and logical commands, such as:");
    eprintln!();
    eprintln!("\ti3 border none");
    eprintln!("\ti3 floating toggle");
    eprintln!("\ti3 kill window");
    eprintln!();
}

/// C: src/main.c:481-545 — send the remaining argv as an IPC RUN_COMMAND and
/// print the reply, exactly like i3-msg does.
fn send_command_via_ipc(payload: &str) -> Result<ExitCode, String> {
    // C: char *socket_path = root_atom_contents("I3_SOCKET_PATH", NULL, 0);
    let socket_path = match libi3::root_atom_contents("I3_SOCKET_PATH", None, 0) {
        Some(p) => p,
        None => {
            // C: ELOG("Could not get i3 IPC socket path\n"); return 1;
            i3_core::log::errorlog("Could not get i3 IPC socket path\n");
            return Ok(ExitCode::FAILURE);
        }
    };

    // C: socket(AF_LOCAL, SOCK_STREAM, 0) + connect()
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd == -1 {
        return Err("Could not create socket".to_string());
    }
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, b) in socket_path.as_bytes().iter().enumerate() {
        if i + 1 >= addr.sun_path.len() {
            break;
        }
        addr.sun_path[i] = *b as libc::c_char;
    }
    // C: if (connect(...) < 0) err(EXIT_FAILURE, "Could not connect to i3");
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        unsafe { libc::close(fd) };
        return Err("Could not connect to i3".to_string());
    }

    // C: ipc_send_message(sockfd, strlen(payload), RUN_COMMAND, payload)
    if libi3::ipc_send_message(fd, payload.len() as u32, 0, payload.as_bytes()) == -1 {
        unsafe { libc::close(fd) };
        return Err("IPC: write()".to_string());
    }

    // C: ipc_recv_message(...) then print the reply
    let mut msg_type: u32 = 0;
    let mut reply_length: u32 = 0;
    let mut reply: Vec<u8> = Vec::new();
    if libi3::ipc_recv_message(fd, Some(&mut msg_type), &mut reply_length, &mut reply) != 0 {
        unsafe { libc::close(fd) };
        return Err("IPC: read()".to_string());
    }
    unsafe { libc::close(fd) };

    if let Ok(s) = String::from_utf8(reply) {
        print!("{}", s);
        if !s.ends_with('\n') {
            println!();
        }
    }
    let _ = msg_type;
    Ok(ExitCode::SUCCESS)
}