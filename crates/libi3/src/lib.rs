//! libi3: contains functions which are used by i3 *and* accompanying tools
//! such as i3-msg, i3-config-wizard, …
//!
//! C: include/libi3.h
//!
//! Every module corresponds to one C file of i3's libi3/ (see the doc
//! comments for the exact C function and line).

#![allow(clippy::needless_return)] // the C code shape is kept 1:1
// C identifiers are kept 1:1 (xcb_char2b_t, i3String, i3Font,
// placeholder_t, getopt's no_argument/required_argument); see the doc comment
// above each item for the C location.
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

pub mod boolstr;
// C: libi3/draw_util.c + libi3/font.c include <cairo/cairo.h> and
// <pango/pangocairo.h>; the FFI declarations for those live here.
pub mod cairo_ffi;
pub mod create_socket;
pub mod dpi;
pub mod draw_util;
pub mod err;
pub mod fake_configure_notify;
pub mod font;
pub mod format_placeholders;
pub mod g_utf8_make_valid;
pub mod get_colorpixel;
pub mod get_config_path;
pub mod getopt;
pub mod get_exe_path;
pub mod get_mod_mask;
pub mod get_process_filename;
pub mod get_visualtype;
pub mod ipc;
pub mod ipc_connect;
pub mod ipc_recv_message;
pub mod ipc_send_message;
pub mod is_background_set;
pub mod is_debug_build;
pub mod log;
pub mod mkdirp;
pub mod nonblock;
pub mod path_exists;
pub mod resolve_tilde;
pub mod root_atom_contents;
pub mod safewrappers;
// C: src/sd-daemon.c (i3 ships this trimmed copy of systemd's helper; only
// sd_listen_fds is used, from src/main.c:1034)
pub mod sd_daemon;
pub mod screenshot_wallpaper;
pub mod string;
pub mod strndup;
pub mod ucs2_conversion;
// C: src/main.c:840-887 <xcb/xkb.h> — the xkb extension requests
pub mod xcb_xkb_ffi;
// C: src/bindings.c:13-14 <xkbcommon/xkbcommon-x11.h> / <xkbcommon/xkbcommon.h>
pub mod xkbcommon_ffi;

// Flat re-exports so ported call sites keep the C spelling
// (libi3.h declares everything in one namespace).
pub use boolstr::boolstr;
pub use create_socket::create_socket;
pub use format_placeholders::{format_placeholders, placeholder_t};
pub use g_utf8_make_valid::g_utf8_make_valid;
pub use get_colorpixel::get_colorpixel;
pub use get_config_path::get_config_path;
pub use getopt::{getopt_long, option, Getopt};
pub use get_exe_path::get_exe_path;
pub use get_process_filename::get_process_filename;
pub use get_visualtype::get_visualtype;
pub use ipc::i3_ipc_header_t;
pub use ipc_connect::{ipc_connect, ipc_connect_impl};
pub use ipc_recv_message::ipc_recv_message;
pub use ipc_send_message::ipc_send_message;
pub use is_debug_build::is_debug_build;
pub use mkdirp::mkdirp;
pub use nonblock::set_nonblock;
pub use path_exists::path_exists;
pub use resolve_tilde::resolve_tilde;
pub use root_atom_contents::root_atom_contents;
pub use safewrappers::{strcasecmp, strcasecmp_nullable, swrite, writeall, writeall_nonblock};
pub use string::{
    i3string_as_ucs2, i3string_as_utf8, i3string_copy, i3string_escape_markup, i3string_free,
    i3string_from_markup, i3string_from_markup_with_length, i3string_from_ucs2,
    i3string_from_utf8, i3string_from_utf8_with_length, i3string_get_num_bytes,
    i3string_get_num_glyphs, i3string_is_markup, i3string_set_markup, i3String,
};
pub use ucs2_conversion::{convert_ucs2_to_utf8, convert_utf8_to_ucs2};
// C: include/libi3.h err()/errx() — process-fatal error helpers.
pub use err::{err, errx, set_errno};
// i3's own SRC logging (src/log.c) lives in i3-core; the libi3 logger is the
// shared fallback that i3-msg and friends use (C: defined per binary).
pub use log::{debuglog, errorlog, verboselog};

// XCB type aliases — the C names are kept, mapped onto x11rb's types.
// C: xcb/xproto.h

/// C: xcb_char2b_t — { byte1, byte2 } (big-endian UCS-2 glyph).
pub type xcb_char2b_t = x11rb::protocol::xproto::Char2b;
/// C: xcb_connection_t — this port uses x11rb's pure-Rust connection.
pub type xcb_connection_t = x11rb::rust_connection::RustConnection;
/// C: xcb_screen_t
pub type xcb_screen_t = x11rb::protocol::xproto::Screen;
/// C: xcb_visualtype_t
pub type xcb_visualtype_t = x11rb::protocol::xproto::Visualtype;
/// C: xcb_visualid_t
pub type xcb_visualid_t = x11rb::protocol::xproto::Visualid;
/// C: xcb_window_t
pub type xcb_window_t = x11rb::protocol::xproto::Window;
/// C: xcb_rectangle_t — { x, y, width, height }.
pub type xcb_rectangle_t = x11rb::protocol::xproto::Rectangle;

// C: include/libi3.h XCB_BUTTON_* — mouse buttons
pub const XCB_BUTTON_CLICK_LEFT: u8 = 1;
pub const XCB_BUTTON_CLICK_MIDDLE: u8 = 2;
pub const XCB_BUTTON_CLICK_RIGHT: u8 = 3;
pub const XCB_BUTTON_SCROLL_UP: u8 = 4;
pub const XCB_BUTTON_SCROLL_DOWN: u8 = 5;
// C: xcb doesn't define constants for these.
pub const XCB_BUTTON_SCROLL_LEFT: u8 = 6;
pub const XCB_BUTTON_SCROLL_RIGHT: u8 = 7;

/// C: include/libi3.h DEFAULT_DIR_MODE
/// (S_IRWXU | S_IRGRP | S_IXGRP | S_IROTH | S_IXOTH)
pub const DEFAULT_DIR_MODE: u32 =
    (libc::S_IRWXU | libc::S_IRGRP | libc::S_IXGRP | libc::S_IROTH | libc::S_IXOTH) as u32;

/// C: config.h SYSCONFDIR — set at configure time by meson
/// (join_paths(prefix, sysconfdir)). The Rust port embeds $I3_SYSCONFDIR at
/// build time (see build.rs); the default is "/etc".
pub const SYSCONFDIR: &str = env!("I3_SYSCONFDIR");

/// C: config.h I3_VERSION — set by meson's vcs_tag with the fallback
/// "<version>-non-git" ("4.25-non-git" here, like a non-git C build).
/// $I3RS_VERSION overrides it at runtime so baseline outputs can be
/// compared byte-exactly against a C build.
pub fn i3_version() -> String {
    std::env::var("I3RS_VERSION").unwrap_or_else(|_| "4.25-non-git".to_string())
}

// C: include/libi3.h — extern xcb_connection_t *conn; extern xcb_screen_t
// *root_screen; These globals exist for the X11-dependent parts of libi3
// (get_colorpixel, aio_get_mod_mask, …) and are TODO for the i3 core port.

/// Serializes tests that mutate process environment variables.
#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    pub fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------------
    // Headless X server for the tests that need a real X connection.
    //
    // C has no equivalent inside libi3: the C test suite runs the whole i3
    // under Xvfb, started by testsuite/complete-run.pl. These unit tests start
    // a private Xvfb instead (never Xephyr: nothing may appear on the real
    // desktop).
    // ------------------------------------------------------------------

    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    /// A private headless Xvfb process; killed when the guard is dropped.
    pub struct Xvfb {
        child: Child,
        display: String,
    }

    impl Xvfb {
        /// The DISPLAY value for x11rb::connect() / xcb_connect().
        pub fn display(&self) -> &str {
            &self.display
        }
    }

    impl Drop for Xvfb {
        fn drop(&mut self) {
            // SIGTERM lets the server remove /tmp/.X<n>-lock and its socket; a
            // SIGKILLed server leaves both behind and they pile up per run.
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                match self.child.try_wait() {
                    Ok(Some(_)) => return,
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// The Xvfb binary: $I3_TEST_XVFB, a test farm root in $I3_TEST_FARM
    /// (…/usr/bin/Xvfb), or Xvfb from $PATH. The X-backed tests skip
    /// themselves when none of these can be started.
    fn xvfb_binary() -> PathBuf {
        if let Ok(value) = std::env::var("I3_TEST_XVFB") {
            return PathBuf::from(value);
        }
        if let Ok(farm) = std::env::var("I3_TEST_FARM") {
            let candidate = PathBuf::from(farm).join("usr/bin/Xvfb");
            if candidate.exists() {
                return candidate;
            }
        }
        PathBuf::from("Xvfb")
    }

    /// Starts Xvfb on a free display number. None when no headless server can
    /// be started (the calling test then skips itself).
    pub fn start_xvfb() -> Option<Xvfb> {
        let binary = xvfb_binary();
        static NEXT_DISPLAY: AtomicU32 = AtomicU32::new(0);
        let base = 60 + (std::process::id() % 100);
        for _ in 0..16u32 {
            let number = base + NEXT_DISPLAY.fetch_add(1, Ordering::SeqCst);
            let socket = PathBuf::from(format!("/tmp/.X11-unix/X{number}"));
            if socket.exists() {
                continue;
            }
            let mut command = Command::new(&binary);
            command
                .arg(format!(":{number}"))
                .args(["-screen", "0", "640x480x24", "-nolisten", "tcp"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            /* The test farm build links against its own shared libraries. */
            if let Some(libdir) = binary
                .parent()
                .and_then(|bin| bin.parent())
                .map(|prefix| prefix.join("lib64"))
            {
                if libdir.is_dir() {
                    let current = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
                    let value = if current.is_empty() {
                        libdir.display().to_string()
                    } else {
                        format!("{}:{}", libdir.display(), current)
                    };
                    command.env("LD_LIBRARY_PATH", value);
                }
            }
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(_) => return None,
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if socket.exists() {
                    return Some(Xvfb {
                        child,
                        display: format!(":{number}"),
                    });
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        None
    }
}
