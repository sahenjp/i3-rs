//! C: i3-msg/main.c:1-16 — Utility which sends messages to a running
//! i3-instance using IPC via UNIX domain sockets.
//!
//! This (in combination with libi3/ipc_send_message.c and
//! libi3/ipc_recv_message.c) serves as an example for how to send your own
//! messages to i3.
//!
//! Additionally, it's even useful sometimes :-).

// C identifiers are kept 1:1 (reply_t from i3-msg/main.c:31, the process
// static exit_code); see reply.rs for the C locations.
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

// C: libc getopt_long(3) — now shared via libi3 (same libc function the C
// build links into every binary).
use libi3::getopt;
mod reply;

use libi3::err::{err, errx};
use libi3::ipc::{
    I3_IPC_EVENT_MASK, I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG, I3_IPC_MESSAGE_TYPE_GET_BINDING_MODES,
    I3_IPC_MESSAGE_TYPE_GET_BINDING_STATE, I3_IPC_MESSAGE_TYPE_GET_CONFIG,
    I3_IPC_MESSAGE_TYPE_GET_MARKS, I3_IPC_MESSAGE_TYPE_GET_OUTPUTS, I3_IPC_MESSAGE_TYPE_GET_TREE,
    I3_IPC_MESSAGE_TYPE_GET_VERSION, I3_IPC_MESSAGE_TYPE_GET_WORKSPACES,
    I3_IPC_MESSAGE_TYPE_RUN_COMMAND, I3_IPC_MESSAGE_TYPE_SEND_TICK, I3_IPC_MESSAGE_TYPE_SUBSCRIBE,
    I3_IPC_REPLY_TYPE_COMMAND, I3_IPC_REPLY_TYPE_CONFIG, I3_IPC_REPLY_TYPE_SUBSCRIBE,
};
use libi3::{ipc_connect, ipc_recv_message, ipc_send_message, i3_version, strcasecmp};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicI32, Ordering};

use libi3::getopt::{getopt_long, option, Getopt};

/// C: i3-msg/main.c:60 static int exit_code — a file static in C; one
/// instance per process is exactly what a static owns here.
static exit_code: AtomicI32 = AtomicI32::new(0);

fn main() {
    // C: int main(int argc, char *argv[])
    let mut go = Getopt::new(std::env::args_os().collect());
    let exit = i3msg_main(&mut go);
    std::process::exit(exit);
}

/// The body of C: i3-msg/main.c:150 int main() — split out only so tests
/// can invoke the same logic; the C structure is kept 1:1.
fn i3msg_main(go: &mut Getopt) -> i32 {
    let mut socket_path: Option<String> = None;
    // C: uint32_t message_type = I3_IPC_MESSAGE_TYPE_RUN_COMMAND;
    let mut message_type: u32 = I3_IPC_MESSAGE_TYPE_RUN_COMMAND;
    let mut payload: Option<Vec<u8>> = None;
    let mut quiet = false;
    let mut monitor = false;
    let mut raw_reply = false;

    // C: i3-msg/main.c:159 static struct option long_options[]
    let long_options = vec![
        option { name: "socket", has_arg: getopt::required_argument, val: b's' },
        option { name: "type", has_arg: getopt::required_argument, val: b't' },
        option { name: "version", has_arg: getopt::no_argument, val: b'v' },
        option { name: "quiet", has_arg: getopt::no_argument, val: b'q' },
        option { name: "monitor", has_arg: getopt::no_argument, val: b'm' },
        option { name: "help", has_arg: getopt::no_argument, val: b'h' },
        option { name: "raw", has_arg: getopt::no_argument, val: b'r' },
    ];

    // C: char *options_string = "s:t:vhqmr";
    let options_string = "s:t:vhqmr";

    // C: while ((o = getopt_long(...)) != -1)
    loop {
        let o = getopt_long(go, options_string, &long_options);
        if o == -1 {
            break;
        }
        if o == b's' as i32 {
            // C: free(socket_path); socket_path = sstrdup(optarg);
            socket_path = go.optarg.as_ref().map(|a| a.to_string_lossy().into_owned());
        } else if o == b't' as i32 {
            let optarg = go
                .optarg
                .as_ref()
                .map(|a| a.to_string_lossy().into_owned())
                .unwrap_or_default();
            // C: i3-msg/main.c:176-202 — the case-insensitive message type
            // name resolution chain.
            if strcasecmp(&optarg, "command") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_RUN_COMMAND;
            } else if strcasecmp(&optarg, "run_command") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_RUN_COMMAND;
            } else if strcasecmp(&optarg, "get_workspaces") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_WORKSPACES;
            } else if strcasecmp(&optarg, "get_outputs") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_OUTPUTS;
            } else if strcasecmp(&optarg, "get_tree") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_TREE;
            } else if strcasecmp(&optarg, "get_marks") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_MARKS;
            } else if strcasecmp(&optarg, "get_bar_config") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG;
            } else if strcasecmp(&optarg, "get_binding_modes") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_BINDING_MODES;
            } else if strcasecmp(&optarg, "get_binding_state") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_BINDING_STATE;
            } else if strcasecmp(&optarg, "get_version") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_VERSION;
            } else if strcasecmp(&optarg, "get_config") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_GET_CONFIG;
            } else if strcasecmp(&optarg, "send_tick") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_SEND_TICK;
            } else if strcasecmp(&optarg, "subscribe") == 0 {
                message_type = I3_IPC_MESSAGE_TYPE_SUBSCRIBE;
            } else {
                // C: i3-msg/main.c:203-205
                println!("Unknown message type");
                println!("Known types: run_command, get_workspaces, get_outputs, get_tree, get_marks, get_bar_config, get_binding_modes, get_binding_state, get_version, get_config, send_tick, subscribe");
                std::process::exit(1);
            }
        } else if o == b'q' as i32 {
            quiet = true;
        } else if o == b'm' as i32 {
            monitor = true;
        } else if o == b'v' as i32 {
            // C: printf("i3-msg " I3_VERSION "\n"); return 0;
            println!("i3-msg {}", i3_version());
            return 0;
        } else if o == b'h' as i32 {
            // C: i3-msg/main.c:214-217
            println!("i3-msg {}", i3_version());
            println!("i3-msg [-s <socket>] [-t <type>] [-m] <message>");
            return 0;
        } else if o == b'?' as i32 {
            std::process::exit(1);
        } else if o == b'r' as i32 {
            raw_reply = true;
        }
    }

    // C: i3-msg/main.c:225-228
    if monitor && message_type != I3_IPC_MESSAGE_TYPE_SUBSCRIBE {
        eprintln!("The monitor option -m is used with -t SUBSCRIBE exclusively.");
        std::process::exit(1);
    }

    // C: i3-msg/main.c:230-243 — Use all arguments, separated by
    // whitespace, as payload. This way, you don't have to do
    // i3-msg 'mark foo', you can use i3-msg mark foo
    for arg in go.remaining() {
        match &mut payload {
            None => payload = Some(arg.as_os_str().as_bytes().to_vec()),
            Some(p) => {
                // C: sasprintf(&both, "%s %s", payload, argv[optind]);
                p.push(b' ');
                p.extend_from_slice(arg.as_os_str().as_bytes());
            }
        }
    }

    // C: i3-msg/main.c:245-247
    if payload.is_none() {
        payload = Some(Vec::new());
    }
    let payload = payload.unwrap();

    // C: i3-msg/main.c:249
    let sockfd = ipc_connect(socket_path.as_deref());
    // C: i3-msg/main.c:250-252
    if ipc_send_message(sockfd, payload.len() as u32, message_type, &payload) == -1 {
        err(1, "IPC: write()");
    }
    drop(payload);

    // C: i3-msg/main.c:255-258
    let mut reply_type: u32 = 0;
    let mut reply_length: u32 = 0;
    let mut reply: Vec<u8> = Vec::new();
    let ret = ipc_recv_message(sockfd, Some(&mut reply_type), &mut reply_length, &mut reply);
    if ret != 0 {
        if ret == -1 {
            err(1, "IPC: read()");
        }
        std::process::exit(1);
    }
    // C: i3-msg/main.c:265-267
    if reply_type != message_type {
        errx(
            1,
            &format!("IPC: Received reply of type {} but expected {}", reply_type, message_type),
        );
    }

    // C: i3-msg/main.c:268-269 — For the reply of commands, have a look if
    // that command was successful. If not, nicely format the error message.
    if reply_type == I3_IPC_REPLY_TYPE_COMMAND {
        // C: i3-msg/main.c:271-283
        if !raw_reply {
            // C: yajl_parse with reply_callbacks; the walk invokes the
            // same callbacks in document order (see reply.rs).
            if !reply::yajl_parse_reply(&reply) {
                errx(1, "IPC: Could not parse JSON reply.");
            }
        }

        // C: i3-msg/main.c:285-287
        if !quiet || raw_reply {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&reply);
            let _ = stdout.write_all(b"\n");
        }
    } else if reply_type == I3_IPC_REPLY_TYPE_CONFIG {
        // C: i3-msg/main.c:288-303
        if raw_reply {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&reply);
            let _ = stdout.write_all(b"\n");
        } else if !reply::yajl_parse_config(&reply) {
            errx(1, "IPC: Could not parse JSON reply.");
        }
    } else if reply_type == I3_IPC_REPLY_TYPE_SUBSCRIBE {
        // C: i3-msg/main.c:304-322 — do { ... } while (monitor);
        loop {
            // C: free(reply);
            reply.clear();
            let ret =
                ipc_recv_message(sockfd, Some(&mut reply_type), &mut reply_length, &mut reply);
            if ret != 0 {
                if ret == -1 {
                    err(1, "IPC: read()");
                }
                std::process::exit(1);
            }

            // C: if (!(reply_type & I3_IPC_EVENT_MASK))
            if (reply_type & I3_IPC_EVENT_MASK) == 0 {
                errx(
                    1,
                    &format!("IPC: Received reply of type {} but expected an event", reply_type),
                );
            }

            if !quiet {
                let mut stdout = std::io::stdout();
                let _ = stdout.write_all(&reply);
                let _ = stdout.write_all(b"\n");
                // C: fflush(stdout);
                let _ = stdout.flush();
            }

            if !monitor {
                break;
            }
        }
    } else {
        // C: i3-msg/main.c:323-327
        if !quiet {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&reply);
            let _ = stdout.write_all(b"\n");
        }
    }

    // C: i3-msg/main.c:329-331 free(reply); close(sockfd);
    drop(reply);
    unsafe { libc::close(sockfd) };

    // C: i3-msg/main.c:333 return exit_code;
    exit_code.load(Ordering::SeqCst)
}