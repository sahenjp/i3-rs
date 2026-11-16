//! C: src/display_version.c (207 lines) — prints the running i3 version, used
//! by `i3 --moreversion`.

#![allow(dead_code)]

/// C: display_version.c:53 static void print_config_path(path, role)
fn print_config_path(path: &str, role: &str) {
    print!("  {} ({})", path, role);
    match std::fs::metadata(path) {
        Err(_) => {
            // C: ELOG("Cannot stat config file \"%s\"\n", path);
            println!();
            crate::log::errorlog(&format!("Cannot stat config file \"{}\"\n", path));
        }
        Ok(md) => {
            // C: strftime(mtime, ..., "%c", localtime(&st_mtime)); time(&now);
            //     printf(" (last modified: %s, %.f seconds ago)\n", ...)
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as libc::time_t)
                .unwrap_or(0);
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&mtime, &mut tm) };
            let mut buf = [0i8; 64];
            unsafe {
                libc::strftime(
                    buf.as_mut_ptr(),
                    buf.len(),
                    b"%c\0".as_ptr() as *const libc::c_char,
                    &tm,
                );
            }
            let formatted = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as f64)
                .unwrap_or(0.0);
            // C: printf(" (last modified: %s, %.f seconds ago)\n", mtime,
            //            difftime(now, sb.st_mtime));
            println!(
                " (last modified: {}, {:.0} seconds ago)",
                formatted,
                now - mtime as f64
            );
        }
    }
}

/// C: display_version.c:81 void display_running_version(void)
///
/// The output shape is:
///   Running i3 version: 4.2-202-gb8e782c (2012-08-12, branch "next") (pid 14804)
///   The i3 binary you just called: /home/michael/i3/i3
///   The i3 binary you are running: /home/michael/i3/i3
// C: main.c:version handler — errx() never returns, so the "Could not parse
// my own reply" path is unreachable after it; the unreachable!() mirrors the
// C control flow and stays 1:1.
#[allow(unreachable_code)]
pub fn display_running_version() {
    // C: if (getenv("DISPLAY") == NULL) { the two hints + exit(FAILURE) }
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!();
        eprintln!("Your DISPLAY environment variable is not set.");
        eprintln!("Are you running i3 via SSH or on a virtual console?");
        eprintln!("Try DISPLAY=:0 i3 --moreversion");
        std::process::exit(1);
    }

    // C: char *pid_from_atom = root_atom_contents("I3_PID", conn, conn_screen);
    let pid_from_atom = match libi3::root_atom_contents("I3_PID", None, 0) {
        Some(p) => p,
        None => {
            // C: If I3_PID is not set, the running version is older than 4.2-200.
            println!();
            println!("Running version: < 4.2-200");
            std::process::exit(0);
        }
    };

    // C: printf("(Getting version from running i3, press ctrl-c to abort…)");
    print!("(Getting version from running i3, press ctrl-c to abort…)");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    // C: int sockfd = ipc_connect(NULL);
    let sockfd = libi3::ipc_connect(None);
    // C: ipc_send_message(sockfd, 0, I3_IPC_MESSAGE_TYPE_GET_VERSION, "")
    const I3_IPC_MESSAGE_TYPE_GET_VERSION: u32 = 7;
    if libi3::ipc_send_message(sockfd, 0, I3_IPC_MESSAGE_TYPE_GET_VERSION, b"") == -1 {
        libi3::err(1, "IPC: write()");
    }

    // C: ipc_recv_message(sockfd, &reply_type, &reply_length, &reply)
    let mut reply_type: u32 = 0;
    let mut reply_length: u32 = 0;
    let mut reply: Vec<u8> = Vec::new();
    let ret = libi3::ipc_recv_message(sockfd, Some(&mut reply_type), &mut reply_length, &mut reply);
    if ret != 0 {
        if ret == -1 {
            libi3::err(1, "IPC: read()");
        }
        std::process::exit(1);
    }

    // C: if (reply_type != GET_VERSION) errx(...)
    if reply_type != I3_IPC_MESSAGE_TYPE_GET_VERSION {
        libi3::errx(
            1,
            &format!(
                "Got reply type {}, but expected {} (GET_VERSION)",
                reply_type, I3_IPC_MESSAGE_TYPE_GET_VERSION
            ),
        );
    }

    // C: yajl_parse the reply; version_string/version_map_key pick the three
    // keys out of the JSON object.
    let text = String::from_utf8_lossy(&reply).into_owned();
    let json: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            libi3::errx(
                1,
                &format!(
                    "Could not parse my own reply. That's weird. reply is {}",
                    text
                ),
            );
            // C: libi3 errx() never returns; kept so the arm still diverges
            // like the C code path (main.c:version handler).
            unreachable!()
        }
    };
    let human_readable_version = json
        .get("human_readable")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let loaded_config_file_name = json
        .get("loaded_config_file_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let included_config_file_names: Vec<String> = json
        .get("included_config_file_names")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // C: printf("\r\x1b[K"); printf("Running i3 version: %s (pid %s)\n", ...)
    print!("\r\u{1b}[K");
    println!(
        "Running i3 version: {} (pid {})",
        human_readable_version.unwrap_or_default(),
        pid_from_atom
    );

    // C: if (loaded_config_file_name) { the main config + the included files }
    if let Some(main) = loaded_config_file_name {
        println!("Loaded i3 config:");
        print_config_path(&main, "main");
        for file in &included_config_file_names {
            print_config_path(file, "included");
        }
    }

    // C: display_version.c:141-202 — the /proc/<pid>/exe comparison (Linux).
    let destpath = std::fs::read_link("/proc/self/exe")
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    println!();
    println!("The i3 binary you just called: {}", destpath);

    let running = std::fs::read_link(format!("/proc/{}/exe", pid_from_atom))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    // C: if (strstr(destpath, "(deleted)") != NULL)
    if running.contains("(deleted)") {
        println!("RUNNING BINARY DIFFERENT FROM BINARY ON DISK!");
    }
    // C: read /proc/<pid>/cmdline instead of trusting the "(deleted)" suffix
    let cmdline = std::fs::read(format!("/proc/{}/cmdline", pid_from_atom))
        .map(|bytes| {
            let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        })
        .unwrap_or_else(|_| running.clone());
    println!("The i3 binary you are running: {}", cmdline);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_of_a_missing_file_reports_to_stderr() {
        // C: stat() == -1 → newline + ELOG. We only assert it does not panic.
        print_config_path("/definitely/not/here", "main");
    }
}
