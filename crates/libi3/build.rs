//! Build script: embeds SYSCONFDIR the way i3's meson does at configure time,
//! and emits the link flags for the C libraries the drawing code binds to.
//!
//! C: meson.build:47 cdata.set_quoted('SYSCONFDIR', join_paths(prefix, sysconfdir))
//! The Rust port embeds $I3_SYSCONFDIR (default "/etc") at build time; see
//! SYSCONFDIR in src/lib.rs.
//!
//! The libraries below are the same ones meson's dependency() calls pull in for
//! libi3 (draw_util.c/font.c/dpi.c): cairo, pango, pangocairo and gobject
//! (pango's own dependency). They are declared by hand in src/cairo_ffi.rs.

fn main() {
    let sysconfdir = std::env::var("I3_SYSCONFDIR").unwrap_or_else(|_| "/etc".to_string());
    println!("cargo:rustc-env=I3_SYSCONFDIR={}", sysconfdir);
    println!("cargo:rerun-if-env-changed=I3_SYSCONFDIR");

    // C: libi3/meson.build — cairo, pangocairo (which brings pango and
    // gobject-2.0).
    for lib in ["cairo", "pango-1.0", "pangocairo-1.0", "gobject-2.0", "glib-2.0"] {
        println!("cargo:rustc-link-lib=dylib={}", lib);
    }

    // C: src/bindings.c links xkbcommon and xkbcommon-x11 (meson's
    // xkbcommon-x11 dependency). libxkbcommon has a plain .so symlink, the
    // -x11 library only ships the versioned soname, so a linker script with the
    // real file is generated in OUT_DIR and put on the library search path.
    // src/xkbcommon_ffi.rs declares the symbols by hand.
    // rustc-link-lib/rustc-link-search (unlike rustc-link-arg) also apply to
    // the binaries that link libi3, which is where the symbols are resolved.
    println!("cargo:rustc-link-lib=dylib=xkbcommon");
    // C: meson.build:309 xcb_dep — libi3/font.c passes the (xcb_connection_t *)
    // to cairo_xcb_surface_create(); the Rust port's binaries link libxcb
    // through x11rb already, the explicit flag makes the dependency of
    // cairo_ffi.rs (cairo_xcb_surface_create) visible in the build script.
    println!("cargo:rustc-link-lib=dylib=xcb");
    // C: src/main.c links xcb-xkb (xcb_xkb_select_events /
    // xcb_xkb_per_client_flags); src/xcb_xkb_ffi.rs declares them by hand.
    println!("cargo:rustc-link-lib=dylib=xcb-xkb");

    let libdir = std::env::var("OUT_DIR").unwrap();
    let libdir = std::path::Path::new(&libdir).join("xkbcommon-x11-link");
    std::fs::create_dir_all(&libdir).expect("create the xkbcommon-x11 link dir");
    let mut real: Option<std::path::PathBuf> = None;
    for dir in ["/usr/lib64", "/lib64", "/usr/lib", "/lib"] {
        let dir = std::path::Path::new(dir);
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("libxkbcommon-x11.so") && entry.path().is_file() {
                real = Some(entry.path());
                break;
            }
        }
        if real.is_some() {
            break;
        }
    }
    match real {
        Some(path) => {
            std::fs::write(
                libdir.join("libxkbcommon-x11.so"),
                format!("INPUT({})
", path.display()),
            )
            .expect("write the xkbcommon-x11 linker script");
            println!("cargo:rustc-link-search=native={}", libdir.display());
            println!("cargo:rustc-link-lib=dylib=xkbcommon-x11");
        }
        // Without the library the port cannot translate keysyms; failing here
        // is the build-time equivalent of load_keymap() failing at runtime.
        None => panic!("libxkbcommon-x11 was not found (looked in /usr/lib64, /lib64, /usr/lib, /lib)"),
    }
}
