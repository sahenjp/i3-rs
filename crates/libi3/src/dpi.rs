//! C: libi3/dpi.c — the DPI setting and the logical-pixel conversion.
//!
//! The C file reads the Xft.dpi X resource through xcb_xrm (libxcb-xrm) and
//! falls back to guessing the DPI from the root window's physical size. The
//! port reads the RESOURCE_MANAGER property with x11rb and parses the same
//! "Xft.dpi" entry out of it; the fallback and logical_px() math are 1:1.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;

/// C: libi3/dpi.c:15 static long dpi
static DPI: AtomicI64 = AtomicI64::new(0);

/// C: extern xcb_screen_t *root_screen — the parts of it that dpi.c reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootScreen {
    pub root: u32,
    /// C: root_screen->width_in_pixels — read by src/floating.c:22
    /// total_outputs_dimensions() when no output is configured.
    pub width_in_pixels: u16,
    pub height_in_pixels: u16,
    pub height_in_millimeters: u16,
}

static ROOT_SCREEN: Mutex<Option<RootScreen>> = Mutex::new(None);

/// C: root_screen = ...
pub fn set_root_screen(screen: Option<RootScreen>) {
    let mut guard = ROOT_SCREEN.lock().unwrap_or_else(|e| e.into_inner());
    *guard = screen;
}

/// C: root_screen == NULL ?
pub fn root_screen() -> Option<RootScreen> {
    *ROOT_SCREEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// C: libi3/dpi.c:17 static long init_dpi_fallback(void)
fn init_dpi_fallback() -> i64 {
    match root_screen() {
        // C: (double)root_screen->height_in_pixels * 25.4 /
        //    (double)root_screen->height_in_millimeters
        // (the C would dereference NULL here; the port keeps 0 instead)
        Some(screen) => {
            (screen.height_in_pixels as f64 * 25.4 / screen.height_in_millimeters as f64) as i64
        }
        None => 0,
    }
}

/// C: libi3/dpi.c:26 void init_dpi(void)
pub fn init_dpi<C: Connection>(conn: Option<&C>) {
    let mut dpi: i64 = 0;

    if let Some(conn) = conn {
        // C: xcb_xrm_database_from_default(conn) + xcb_xrm_resource_get_string
        //    (database, "Xft.dpi", NULL, &resource)
        match xft_dpi_resource(conn) {
            None => {
                // C: DLOG("Resource Xft.dpi not specified, skipping.");
            }
            Some(resource) => match parse_dpi(&resource) {
                Some(value) => {
                    dpi = value;
                    // C: DLOG("Found Xft.dpi = %ld.", dpi);
                }
                None => {
                    // C: ELOG("Xft.dpi = %s is an invalid number and
                    //    couldn't be parsed."); dpi = 0;
                    crate::errorlog(&format!(
                        "Xft.dpi = {} is an invalid number and couldn't be parsed.\n",
                        resource
                    ));
                    dpi = 0;
                }
            },
        }
    }

    if dpi == 0 {
        // C: DLOG("Using fallback for calculating DPI.");
        dpi = init_dpi_fallback();
        // C: DLOG("Using dpi = %ld", dpi);
    }

    DPI.store(dpi, Ordering::Relaxed);
}

/// C: libi3/dpi.c:46 strtod(resource, &endptr) plus the validity checks.
fn parse_dpi(resource: &str) -> Option<i64> {
    let trimmed = resource.trim_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r');
    if trimmed.is_empty() {
        // C: endptr == resource
        return None;
    }
    // C: *endptr != 0 is an error — only a full number is accepted.
    let value: f64 = trimmed.parse().ok()?;
    // C: in_dpi == HUGE_VAL || in_dpi < 0
    if value.is_infinite() || value < 0.0 {
        return None;
    }
    // C: lround(in_dpi)
    Some(value.round() as i64)
}

/// C: xcb_xrm_resource_get_string(database, "Xft.dpi", NULL, &resource)
fn xft_dpi_resource<C: Connection>(conn: &C) -> Option<String> {
    let root = match root_screen() {
        Some(screen) => screen.root,
        None => {
            let setup = conn.setup();
            setup.roots.first()?.root
        }
    };

    let atom = conn
        .intern_atom(false, b"RESOURCE_MANAGER")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let reply = conn
        .get_property(false, root, atom, x11rb::protocol::xproto::AtomEnum::STRING, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;

    let text = String::from_utf8_lossy(&reply.value).into_owned();
    for line in text.lines() {
        let (key, value) = match line.split_once(':') {
            Some(parts) => parts,
            None => continue,
        };
        let name = key.trim().trim_start_matches('*').trim_start_matches('.');
        if name == "Xft.dpi" {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// C: libi3/dpi.c:75 long get_dpi_value(void)
pub fn get_dpi_value() -> i64 {
    DPI.load(Ordering::Relaxed)
}

/// Overrides the DPI value without an X server (C has no counterpart: the
/// static dpi is only written by init_dpi). Unit tests use it to get
/// deterministic font metrics.
pub fn set_dpi_value(dpi: i64) {
    DPI.store(dpi, Ordering::Relaxed);
}

/// C: libi3/dpi.c:85 int logical_px(const int logical)
pub fn logical_px(logical: i32) -> i32 {
    if root_screen().is_none() {
        // C: Dpi info may not be available when parsing a config without an X
        // server, such as for config file validation.
        return logical;
    }

    let dpi = get_dpi_value() as f64;
    if (dpi / 96.0) < 1.25 {
        return logical;
    }
    (dpi / 96.0 * logical as f64).ceil() as i32
}

/// C: libi3/dpi.c — logical_px() is the only such helper in the C file; the
/// alias keeps call sites that phrase it as "if needed" readable.
pub fn logical_px_if_needed(logical: i32) -> i32 {
    logical_px(logical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_dpi(dpi: i64, screen: Option<RootScreen>, f: impl FnOnce()) {
        // The DPI value is process wide (C: static long dpi), so the tests
        // that touch it must not run concurrently.
        let _guard = crate::test_util::env_lock();
        let old_dpi = get_dpi_value();
        let old_screen = root_screen();
        set_dpi_value(dpi);
        set_root_screen(screen);
        f();
        set_dpi_value(old_dpi);
        set_root_screen(old_screen);
    }

    fn screen() -> Option<RootScreen> {
        Some(RootScreen {
            root: 0,
            width_in_pixels: 640,
            height_in_pixels: 480,
            height_in_millimeters: 127,
        })
    }

    #[test]
    /// C: libi3/dpi.c:85 — without root_screen the logical value is returned.
    fn logical_px_without_x_returns_logical() {
        with_dpi(226, None, || {
            assert_eq!(logical_px(2), 2);
            assert_eq!(logical_px(0), 0);
        });
    }

    #[test]
    /// C: (dpi / 96.0) < 1.25 means unchanged.
    fn logical_px_normal_dpi() {
        with_dpi(96, screen(), || assert_eq!(logical_px(5), 5));
        // 119 / 96 = 1.239 < 1.25
        with_dpi(119, screen(), || assert_eq!(logical_px(5), 5));
    }

    #[test]
    /// C: ceil((dpi / 96.0) * logical)
    fn logical_px_retina_dpi() {
        // 227 / 96 * 2 = 4.72 rounded up = 5
        with_dpi(227, screen(), || assert_eq!(logical_px(2), 5));
        // 192 / 96 * 5 = 10
        with_dpi(192, screen(), || assert_eq!(logical_px(5), 10));
        // 120 / 96 = 1.25, not below the threshold: ceil(1.25 * 4) = 5
        with_dpi(120, screen(), || assert_eq!(logical_px(4), 5));
    }

    #[test]
    /// C: libi3/dpi.c:17 init_dpi_fallback — height_px * 25.4 / height_mm
    fn dpi_fallback_math() {
        let s = screen().unwrap();
        let expected = (s.height_in_pixels as f64 * 25.4 / s.height_in_millimeters as f64) as i64;
        with_dpi(0, Some(s), || {
            assert_eq!(init_dpi_fallback(), expected);
            // 480 / 127 * 25.4 = 96.0
            assert_eq!(expected, 96);
        });
    }

    #[test]
    /// C: libi3/dpi.c:47-52 — the strtod validation of the Xft.dpi value.
    fn parse_xft_dpi_resource() {
        assert_eq!(parse_dpi("96"), Some(96));
        assert_eq!(parse_dpi(" 227 "), Some(227));
        assert_eq!(parse_dpi("96.5"), Some(97)); // lround
        assert_eq!(parse_dpi("96.4"), Some(96));
        assert_eq!(parse_dpi(""), None);
        assert_eq!(parse_dpi("96x"), None); // trailing garbage
        assert_eq!(parse_dpi("-1"), None); // in_dpi < 0
        assert_eq!(parse_dpi("inf"), None); // HUGE_VAL
    }
}
