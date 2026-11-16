//! i3-core: the X11-free tree layer of the i3 window manager, ported 1:1
//! from C (i3 4.25, commit 8a97b0f).
//!
//! Layout tree, containers, workspaces, rendering calculations, gaps,
//! scratchpad and assignments — everything that does not touch X11 directly.
//! X11/IPC side effects are injected through the
//! [ConBackend](backend::ConBackend) trait.

#![allow(dead_code)]
// The port keeps the C identifiers 1:1 so a reader can diff against src/*.c:
// types/enums stay snake_case (xcb_window_t, reply_t, CLICK_INSIDE,
// drag_result_t) and Atom fields keep their X11 spelling (A__NET_WM_NAME,
// A_WM_PROTOCOLS). Renaming them would break the 1:1 traceability.
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

pub mod data;
pub mod backend;
pub mod util;
pub mod con;
pub mod tree;
pub mod workspace;
pub mod output;
pub mod render;
pub mod gaps;
pub mod match_;
pub mod floating;
pub mod move_;
pub mod assignments;
pub mod scratchpad;
// --- Wave 2 modules (added by lead; agents fill the files) ---
#[cfg(test)]
pub mod test_lock;
pub mod log;
pub mod display_version;
pub mod x;
// C: src/main.c:840-887 + src/bindings.c (xkbcommon half)
pub mod xkb;
pub mod handlers;
pub mod click;
pub mod drag;
pub mod resize;
pub mod tiling_drag;
pub mod manage;
pub mod randr;
// C: src/fake_outputs.c
pub mod fake_outputs;
pub mod ewmh;
pub mod xcb_util;
pub mod xbackend;
pub mod config;
pub mod config_directives;
pub mod commands;
pub mod ipc;
pub mod dump;
pub mod load_layout;
pub mod restore_layout;
pub mod bindings;
pub mod sighandler;
// C: src/sync.c — the i3 sync protocol used by the testsuite
pub mod sync;
pub mod startup;
pub mod reload;

pub use backend::{ConBackend, NullBackend};
pub use data::*;
pub use tree::Tree;
pub use con::ConId;
