//! Hand written FFI declarations for the C libraries the drawing code uses.
//!
//! C: libi3/draw_util.c and libi3/font.c include <cairo/cairo.h> and
//! <pango/pangocairo.h>; the Rust port declares exactly the functions those
//! two files call (no crates are added, build.rs adds the link flags).
//!
//! NOTE on the connection type: C passes the raw xcb_connection_t to
//! cairo_xcb_surface_create(). The port's binaries own an
//! x11rb::xcb_ffi::XCBConnection, which wraps a libxcb connection and exposes
//! it through get_raw_xcb_connection(), so cairo_xcb_surface_create() is
//! declared here and used by font.rs for the font-height measurement (C:
//! libi3/font.c:43 load_pango_font measures on the root window and the root
//! visual). draw_util.rs still draws into an ARGB32 image surface and blits it
//! onto the X drawable with xcb_put_image (see draw_util_surface_flush), which
//! produces the same pixels as the cairo-xcb surface would.

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::ffi::{c_char, c_double, c_int, c_uchar, c_uint, c_void};

/* -------------------------------------------------------------------------
 * cairo  (C: cairo/cairo.h)
 * ---------------------------------------------------------------------- */

/// C: cairo_t
#[repr(C)]
pub struct cairo_t {
    _private: [u8; 0],
}

/// C: cairo_surface_t
#[repr(C)]
pub struct cairo_surface_t {
    _private: [u8; 0],
}

/// C: cairo_status_t
pub type cairo_status_t = c_int;
/// C: cairo_operator_t
pub type cairo_operator_t = c_int;
/// C: cairo_format_t
pub type cairo_format_t = c_int;

/// C: cairo_status_t CAIRO_STATUS_SUCCESS
pub const CAIRO_STATUS_SUCCESS: cairo_status_t = 0;
/// C: cairo_operator_t CAIRO_OPERATOR_SOURCE
pub const CAIRO_OPERATOR_SOURCE: cairo_operator_t = 1;
/// C: cairo_format_t CAIRO_FORMAT_ARGB32
pub const CAIRO_FORMAT_ARGB32: cairo_format_t = 0;

extern "C" {
    pub fn cairo_create(target: *mut cairo_surface_t) -> *mut cairo_t;
    pub fn cairo_destroy(cr: *mut cairo_t);
    pub fn cairo_status(cr: *mut cairo_t) -> cairo_status_t;
    pub fn cairo_status_to_string(status: cairo_status_t) -> *const c_char;

    pub fn cairo_save(cr: *mut cairo_t);
    pub fn cairo_restore(cr: *mut cairo_t);
    pub fn cairo_set_operator(cr: *mut cairo_t, op: cairo_operator_t);
    pub fn cairo_set_source_rgb(cr: *mut cairo_t, red: c_double, green: c_double, blue: c_double);
    pub fn cairo_set_source_rgba(
        cr: *mut cairo_t,
        red: c_double,
        green: c_double,
        blue: c_double,
        alpha: c_double,
    );
    pub fn cairo_set_source_surface(
        cr: *mut cairo_t,
        surface: *mut cairo_surface_t,
        x: c_double,
        y: c_double,
    );
    pub fn cairo_rectangle(cr: *mut cairo_t, x: c_double, y: c_double, width: c_double, height: c_double);
    pub fn cairo_fill(cr: *mut cairo_t);
    pub fn cairo_paint(cr: *mut cairo_t);
    pub fn cairo_move_to(cr: *mut cairo_t, x: c_double, y: c_double);
    pub fn cairo_translate(cr: *mut cairo_t, tx: c_double, ty: c_double);
    pub fn cairo_scale(cr: *mut cairo_t, sx: c_double, sy: c_double);

    pub fn cairo_surface_destroy(surface: *mut cairo_surface_t);
    pub fn cairo_surface_flush(surface: *mut cairo_surface_t);
    pub fn cairo_surface_mark_dirty(surface: *mut cairo_surface_t);

    /// C: cairo_image_surface_create (the port's replacement for
    /// cairo_xcb_surface_create, see the module comment).
    pub fn cairo_image_surface_create(
        format: cairo_format_t,
        width: c_int,
        height: c_int,
    ) -> *mut cairo_surface_t;
    pub fn cairo_image_surface_get_data(surface: *mut cairo_surface_t) -> *mut c_uchar;
    pub fn cairo_image_surface_get_stride(surface: *mut cairo_surface_t) -> c_int;
    pub fn cairo_image_surface_get_width(surface: *mut cairo_surface_t) -> c_int;
    pub fn cairo_image_surface_get_height(surface: *mut cairo_surface_t) -> c_int;
}

/* -------------------------------------------------------------------------
 * gobject  (C: glib-object.h)
 * ---------------------------------------------------------------------- */

extern "C" {
    /// C: g_object_unref
    pub fn g_object_unref(object: *mut c_void);
}

/* -------------------------------------------------------------------------
 * pango / pangocairo  (C: pango/pangocairo.h)
 * ---------------------------------------------------------------------- */

/// C: PangoLayout
#[repr(C)]
pub struct PangoLayout {
    _private: [u8; 0],
}
/// C: PangoContext
#[repr(C)]
pub struct PangoContext {
    _private: [u8; 0],
}
/// C: PangoFontDescription
#[repr(C)]
pub struct PangoFontDescription {
    _private: [u8; 0],
}
/// C: PangoWrapMode
pub type PangoWrapMode = c_int;
/// C: PangoEllipsizeMode
pub type PangoEllipsizeMode = c_int;

/// C: PANGO_SCALE
pub const PANGO_SCALE: c_int = 1024;
/// C: PangoWrapMode PANGO_WRAP_CHAR
pub const PANGO_WRAP_CHAR: PangoWrapMode = 1;
/// C: PangoEllipsizeMode PANGO_ELLIPSIZE_END
pub const PANGO_ELLIPSIZE_END: PangoEllipsizeMode = 3;

extern "C" {
    pub fn pango_layout_new(context: *mut PangoContext) -> *mut PangoLayout;
    pub fn pango_layout_set_text(layout: *mut PangoLayout, text: *const c_char, length: c_int);
    pub fn pango_layout_set_markup(layout: *mut PangoLayout, markup: *const c_char, length: c_int);
    pub fn pango_layout_set_font_description(
        layout: *mut PangoLayout,
        desc: *const PangoFontDescription,
    );
    pub fn pango_layout_set_width(layout: *mut PangoLayout, width: c_int);
    pub fn pango_layout_set_wrap(layout: *mut PangoLayout, wrap: PangoWrapMode);
    pub fn pango_layout_set_ellipsize(layout: *mut PangoLayout, ellipsize: PangoEllipsizeMode);
    pub fn pango_layout_set_single_paragraph_mode(layout: *mut PangoLayout, setting: c_int);
    pub fn pango_layout_get_pixel_size(layout: *mut PangoLayout, width: *mut c_int, height: *mut c_int);
    pub fn pango_layout_get_pixel_extents(
        layout: *mut PangoLayout,
        ink_rect: *mut PangoRectangle,
        logical_rect: *mut PangoRectangle,
    );

    pub fn pango_cairo_create_layout(cr: *mut cairo_t) -> *mut PangoLayout;
    pub fn pango_cairo_update_layout(cr: *mut cairo_t, layout: *mut PangoLayout);
    pub fn pango_cairo_show_layout(cr: *mut cairo_t, layout: *mut PangoLayout);
    pub fn pango_cairo_create_context(cr: *mut cairo_t) -> *mut PangoContext;
    pub fn pango_cairo_context_set_resolution(context: *mut PangoContext, dpi: c_double);

    pub fn pango_font_description_from_string(str: *const c_char) -> *mut PangoFontDescription;
    pub fn pango_font_description_free(desc: *mut PangoFontDescription);
    pub fn pango_font_description_get_family(desc: *const PangoFontDescription) -> *const c_char;
    pub fn pango_font_description_get_size(desc: *const PangoFontDescription) -> c_int;
}

/// C: PangoRectangle { int x, y, width, height }
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PangoRectangle {
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
}

/* -------------------------------------------------------------------------
 * xcb + cairo-xcb  (C: <xcb/xcb.h>, <cairo/cairo-xcb.h>)
 * ---------------------------------------------------------------------- */

/// C: xcb_connection_t — opaque, owned by x11rb's XCBConnection.
#[repr(C)]
pub struct xcb_connection_t {
    _private: [u8; 0],
}

/// C: xproto.h xcb_visualtype_t (the fields cairo reads; the struct is
/// 24 bytes, the trailing pad0 completes it).
///
/// The port builds this value from the x11rb setup reply instead of handing
/// cairo the pointer into libxcb's setup buffer: cairo only reads visual_id,
/// the class and the channel masks, and the copy stays valid for the process
/// (see font.rs set_pango_root_surface).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct xcb_visualtype_t {
    pub visual_id: u32,
    pub class: u8,
    pub bits_per_rgb_value: u8,
    pub colormap_entries: u16,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub pad0: [u8; 4],
}

extern "C" {
    /// C: cairo-xcb.h cairo_xcb_surface_create — used by font.rs to measure
    /// the Pango font height on the root window, exactly like libi3/font.c:43.
    pub fn cairo_xcb_surface_create(
        conn: *mut xcb_connection_t,
        drawable: u32,
        visual: *mut xcb_visualtype_t,
        width: c_int,
        height: c_int,
    ) -> *mut cairo_surface_t;
}

// The unused-import lint would fire for c_uint in some cfgs; keep the type
// aliases referenced so the module stays warning free.
const _: Option<c_uint> = None;
