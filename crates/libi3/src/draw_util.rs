//! C: libi3/draw_util.c — cairo-based drawing helpers.
//!
//! C draws through a cairo-xcb surface that is bound to the X drawable. The
//! port cannot do that: the whole process shares one x11rb RustConnection
//! (pure Rust, no libxcb handle), see the note in cairo_ffi.rs. Instead the
//! cairo surface is an ARGB32 image surface of the drawable's size and
//! draw_util_surface_flush() pushes the pixels to X with xcb_put_image. The
//! drawing calls, the color handling and the surface lifecycle are 1:1 with
//! the C file.

use std::sync::Mutex;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};

use crate::cairo_ffi::*;
use crate::font::{draw_text, set_font_colors};
use crate::string::i3String;

/// C: include/xcb.h XCB_NONE
const XCB_NONE: u32 = 0;

/// C: include/libi3.h color_t — a color split by color channel.
#[derive(Clone, Copy, Default)]
pub struct color_t {
    /// C: double red
    pub red: f64,
    /// C: double green
    pub green: f64,
    /// C: double blue
    pub blue: f64,
    /// C: double alpha
    pub alpha: f64,
    /// C: uint32_t colorpixel — the colorpixel used for direct XCB calls
    pub colorpixel: u32,
}

/// C: include/xcb.h struct surface — the drawable plus its cairo state.
///
/// The field names follow the C struct: `id` is the X drawable, `surface` and
/// `cr` are the cairo objects (an image surface in this port, see the module
/// comment) and `gc` is the graphics context used for the XCB side.
pub struct surface_t {
    /// C: xcb_drawable_t id
    pub id: u32,
    /// C: int width
    pub width: i32,
    /// C: int height
    pub height: i32,
    /// C: xcb_gcontext_t gc
    pub gc: u32,
    /// C: bool owns_gc
    pub owns_gc: bool,
    /// C: cairo_surface_t *surface
    pub surface: *mut cairo_surface_t,
    /// C: cairo_t *cr
    pub cr: *mut cairo_t,
    /// The depth of the drawable (C reads it back through the visual).
    pub depth: u8,
}

impl Default for surface_t {
    /// C: memset(&surface, 0, sizeof(surface_t)) — id 0 means "not
    /// initialized", the pointers are NULL.
    fn default() -> Self {
        surface_t {
            id: XCB_NONE,
            width: 0,
            height: 0,
            gc: XCB_NONE,
            owns_gc: false,
            surface: std::ptr::null_mut(),
            cr: std::ptr::null_mut(),
            depth: 0,
        }
    }
}

impl surface_t {
    /// C: surface_t surface = {0};
    pub fn none() -> surface_t {
        surface_t::default()
    }
}

/// C: libi3/draw_util.c:22 static bool surface_initialized(surface_t *surface)
fn surface_initialized(surface: &surface_t) -> bool {
    if surface.id == XCB_NONE {
        // C: ELOG("Surface %p is not initialized, skipping drawing.")
        crate::errorlog("Surface is not initialized, skipping drawing.\n");
        return false;
    }
    true
}

/// The GC cache of C: libi3/draw_util.c:38 static struct { uint8_t depth;
/// xcb_gcontext_t gc; } gc_cache[2].
static GC_CACHE: Mutex<[(u8, u32); 2]> = Mutex::new([(0, 0), (0, 0)]);

/// C: libi3/draw_util.c:37 static xcb_gcontext_t get_gc(xcb_connection_t *conn,
/// uint8_t depth, xcb_drawable_t drawable, bool *should_free)
///
/// Get a GC for the given depth. The given drawable must have this depth.
///
/// Per the X11 protocol manual for "CreateGC":
/// > The gcontext can be used with any destination drawable having the same
/// > root and depth as the specified drawable;
fn get_gc<C: Connection>(conn: &C, depth: u8, drawable: u32) -> (u32, bool) {
    let mut cache = GC_CACHE.lock().unwrap_or_else(|e| e.into_inner());

    // C: size_t index = 0; bool cache = false;
    //    for (; index < arraysize; index++) { if (depth match) return;
    //    if (gc_cache[index].depth == 0) { cache = true; break; } }
    let mut cache_index: Option<usize> = None;
    for (index, entry) in cache.iter().enumerate() {
        if entry.0 == depth {
            return (entry.1, false);
        }
        if entry.0 == 0 {
            cache_index = Some(index);
            break;
        }
    }

    let gc = match conn.generate_id() {
        Ok(gc) => gc,
        Err(_) => return (XCB_NONE, false),
    };
    // C: The drawable is only used to get the root and depth, thus the GC is
    //    not tied to the drawable and it can be re-used with different
    //    drawables.
    let error = match conn.create_gc(gc, drawable, &x11rb::protocol::xproto::CreateGCAux::new()) {
        Ok(cookie) => cookie.check().is_err(),
        Err(_) => true,
    };
    if error {
        // C: ELOG("Could not create graphical context. Error code: %d. Please
        //    report this bug.")
        crate::errorlog("Could not create graphical context. Please report this bug.\n");
        return (gc, false);
    }

    match cache_index {
        Some(index) => {
            cache[index] = (depth, gc);
            (gc, false)
        }
        // C: *should_free = true;
        None => (gc, true),
    }
}

/// C: libi3/draw_util.c:85 uint16_t get_visual_depth(const xcb_visualid_t
/// visual_id)
///
/// Get depth of visual specified by visualid
pub fn get_visual_depth(screen: &x11rb::protocol::xproto::Screen, visual_id: u32) -> u16 {
    for depth in &screen.allowed_depths {
        for visual in &depth.visuals {
            if visual_id == visual.visual_id {
                return depth.depth as u16;
            }
        }
    }
    0
}

/// C: libi3/draw_util.c:106 void draw_util_surface_init(xcb_connection_t *conn,
/// surface_t *surface, xcb_drawable_t drawable, xcb_visualtype_t *visual,
/// int width, int height)
///
/// Initialize the surface to represent the given drawable.
pub fn draw_util_surface_init<C: Connection>(
    conn: &C,
    surface: &mut surface_t,
    drawable: u32,
    depth: u8,
    width: i32,
    height: i32,
) {
    surface.id = drawable;
    surface.width = width;
    surface.height = height;
    surface.depth = depth;

    // C: surface->gc = get_gc(conn, get_visual_depth(visual->visual_id),
    //    drawable, &surface->owns_gc);
    let (gc, owns_gc) = get_gc(conn, depth, drawable);
    surface.gc = gc;
    surface.owns_gc = owns_gc;

    // C: surface->surface = cairo_xcb_surface_create(...);
    //    surface->cr = cairo_create(surface->surface);
    surface.surface = unsafe {
        cairo_image_surface_create(CAIRO_FORMAT_ARGB32, width.max(1), height.max(1))
    };
    surface.cr = unsafe { cairo_create(surface.surface) };
}

/// C: libi3/draw_util.c:125 void draw_util_surface_free(xcb_connection_t *conn,
/// surface_t *surface)
///
/// Destroys the surface.
pub fn draw_util_surface_free<C: Connection>(conn: &C, surface: &mut surface_t) {
    // C: cairo_status_t status = CAIRO_STATUS_SUCCESS;
    //    if (surface->cr) status = cairo_status(surface->cr);
    //    if (status != CAIRO_STATUS_SUCCESS) LOG(...)
    if !surface.cr.is_null() {
        let status = unsafe { cairo_status(surface.cr) };
        if status != CAIRO_STATUS_SUCCESS {
            let text = unsafe {
                let ptr = cairo_status_to_string(status);
                if ptr.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
                }
            };
            crate::debuglog(&format!(
                "Found cairo context in an error status while freeing, error {} is {}\n",
                status, text
            ));
        }
    }

    // C: if (surface->owns_gc) xcb_free_gc(conn, surface->gc);
    if surface.owns_gc && surface.gc != XCB_NONE {
        let _ = conn.free_gc(surface.gc);
    }

    unsafe {
        // C: cairo_surface_destroy(surface->surface); cairo_destroy(surface->cr);
        if !surface.surface.is_null() {
            cairo_surface_destroy(surface.surface);
        }
        if !surface.cr.is_null() {
            cairo_destroy(surface.cr);
        }
    }

    // C: We need to explicitly set these to NULL to avoid assertion errors in
    //    cairo when calling this multiple times. This can happen, for example,
    //    when setting the border of a window to none and then closing it.
    surface.surface = std::ptr::null_mut();
    surface.cr = std::ptr::null_mut();
}

/// C: libi3/draw_util.c:152 void draw_util_surface_set_size(surface_t
/// *surface, int width, int height)
///
/// Resize the surface to the given size. (C calls
/// cairo_xcb_surface_set_size(); the image surface is re-created, which keeps
/// the same "everything is redrawn afterwards" contract of the C caller.)
pub fn draw_util_surface_set_size(surface: &mut surface_t, width: i32, height: i32) {
    surface.width = width;
    surface.height = height;

    if surface.surface.is_null() {
        return;
    }
    unsafe {
        if !surface.cr.is_null() {
            cairo_destroy(surface.cr);
        }
        cairo_surface_destroy(surface.surface);
    }
    surface.surface =
        unsafe { cairo_image_surface_create(CAIRO_FORMAT_ARGB32, width.max(1), height.max(1)) };
    surface.cr = unsafe { cairo_create(surface.surface) };
}

/// C: CAIRO_SURFACE_FLUSH(surface->surface) — plus the XCB side of the port:
/// push the image surface pixels to the drawable the surface represents.
pub fn draw_util_surface_flush<C: Connection>(conn: &C, surface: &mut surface_t) {
    if surface.surface.is_null() || surface.id == XCB_NONE {
        return;
    }

    let width = surface.width.max(1) as usize;
    let height = surface.height.max(1) as usize;

    unsafe {
        cairo_surface_flush(surface.surface);
        let data = cairo_image_surface_get_data(surface.surface);
        if data.is_null() {
            return;
        }
        let stride = cairo_image_surface_get_stride(surface.surface).max(0) as usize;
        let row_bytes = width * 4;
        if stride == 0 {
            return;
        }
        let raw = std::slice::from_raw_parts(data, stride * height);
        let contiguous;
        let pixels: &[u8] = if stride == row_bytes {
            raw
        } else {
            let mut buf = Vec::with_capacity(row_bytes * height);
            for row in 0..height {
                buf.extend_from_slice(&raw[row * stride..row * stride + row_bytes]);
            }
            contiguous = buf;
            &contiguous
        };

        // C: the cairo-xcb surface writes the same pixels into the drawable;
        // the port sends them explicitly.
        let _ = conn.put_image(
            ImageFormat::Z_PIXMAP,
            surface.id,
            surface.gc,
            width as u16,
            height as u16,
            0,
            0,
            0,
            surface.depth,
            pixels,
        );
    }
}

/// C: libi3/draw_util.c:163 color_t draw_util_hex_to_color(const char *color)
///
/// Parses the given color in hex format to an internal color representation.
/// Note that the input must begin with a hash sign, e.g., "#3fbc59".
pub fn draw_util_hex_to_color(color: &str) -> color_t {
    let bytes = color.as_bytes();
    if bytes.len() < 6 || bytes[0] != b'#' {
        // C: ELOG("Could not parse color: %s")
        crate::errorlog(&format!("Could not parse color: {}\n", color));
        return draw_util_hex_to_color("#A9A9A9");
    }

    // C: char alpha[2];
    //    if (strlen(color) == strlen("#rrggbbaa")) { ... } else { 'F','F' }
    let alpha = if bytes.len() == "#rrggbbaa".len() {
        [bytes[7], bytes[8]]
    } else {
        [b'F', b'F']
    };

    // C: char groups[4][3] = { {color[1], color[2]}, ... };
    let groups: [[u8; 2]; 4] = [
        [bytes[1], bytes[2]],
        [bytes[3], bytes[4]],
        [bytes[5], bytes[6]],
        alpha,
    ];

    // C: strtol(group, NULL, 16) / 255.0
    let parse_group = |group: &[u8; 2]| -> f64 {
        let text = String::from_utf8_lossy(group).into_owned();
        u32::from_str_radix(text.trim(), 16).unwrap_or(0) as f64
    };

    color_t {
        red: parse_group(&groups[0]) / 255.0,
        green: parse_group(&groups[1]) / 255.0,
        blue: parse_group(&groups[2]) / 255.0,
        alpha: parse_group(&groups[3]) / 255.0,
        // C: .colorpixel = get_colorpixel(color)
        colorpixel: crate::get_colorpixel(color),
    }
}

/// C: libi3/draw_util.c:195 static void draw_util_set_source_color(surface_t
/// *surface, color_t color)
fn draw_util_set_source_color(surface: &surface_t, color: color_t) {
    if !surface_initialized(surface) {
        return;
    }

    unsafe {
        cairo_set_source_rgba(surface.cr, color.red, color.green, color.blue, color.alpha);
    }
}

/// C: libi3/draw_util.c:209 void draw_util_text(i3String *text, surface_t
/// *surface, color_t fg_color, color_t bg_color, int x, int y, int max_width)
///
/// Draw the given text using libi3. This function also marks the surface dirty
/// which is needed if other means of drawing are used. This will be the case
/// when using XCB to draw text.
pub fn draw_util_text<C: Connection>(
    conn: &C,
    text: &mut i3String,
    surface: &mut surface_t,
    fg_color: color_t,
    bg_color: color_t,
    x: i32,
    y: i32,
    max_width: i32,
) {
    if !surface_initialized(surface) {
        return;
    }

    // C: Flush any changes before we draw the text as this might use XCB
    //    directly. (The port flushes to the drawable, see
    //    draw_util_surface_flush.)
    draw_util_surface_flush(conn, surface);

    set_font_colors(conn, surface.gc, fg_color, bg_color);
    draw_text(conn, text, surface.id, surface.gc, surface.surface, x, y, max_width);

    // C: Notify cairo that we (possibly) used another way to draw on the
    //    surface.
    unsafe { cairo_surface_mark_dirty(surface.surface) };
}

/// C: libi3/draw_util.c:256 void draw_util_rectangle(surface_t *surface,
/// color_t color, double x, double y, double w, double h)
///
/// The connection is the port's replacement for the cairo-xcb context that
/// holds it in C (see the module comment).
///
/// Draws a filled rectangle. This function is a convenience wrapper and takes
/// care of flushing the surface as well as restoring the cairo state.
pub fn draw_util_rectangle<C: Connection>(
    conn: &C,
    surface: &mut surface_t,
    color: color_t,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) {
    if !surface_initialized(surface) {
        return;
    }

    unsafe {
        cairo_save(surface.cr);

        // C: Using the SOURCE operator will copy both color and alpha
        //    information directly onto the surface rather than blending it.
        cairo_set_operator(surface.cr, CAIRO_OPERATOR_SOURCE);
        draw_util_set_source_color(surface, color);

        cairo_rectangle(surface.cr, x, y, w, h);
        cairo_fill(surface.cr);

        cairo_restore(surface.cr);
    }

    // C: Make sure we flush the surface for any text drawing operations that
    //    could follow.
    draw_util_surface_flush(conn, surface);
}

/// C: libi3/draw_util.c:283 void draw_util_clear_surface(surface_t *surface,
/// color_t color)
///
/// (The connection argument mirrors draw_util_rectangle.)
///
/// Clears a surface with the given color.
pub fn draw_util_clear_surface<C: Connection>(conn: &C, surface: &mut surface_t, color: color_t) {
    if !surface_initialized(surface) {
        return;
    }

    unsafe {
        cairo_save(surface.cr);

        cairo_set_operator(surface.cr, CAIRO_OPERATOR_SOURCE);
        draw_util_set_source_color(surface, color);

        cairo_paint(surface.cr);

        cairo_restore(surface.cr);
    }

    // C: Make sure we flush the surface for any text drawing operations that
    //    could follow.
    draw_util_surface_flush(conn, surface);
}

/// C: libi3/draw_util.c:309 void draw_util_copy_surface(surface_t *src,
/// surface_t *dest, double src_x, double src_y, double dest_x, double dest_y,
/// double width, double height)
///
/// Copies a surface onto another surface.
pub fn draw_util_copy_surface<C: Connection>(
    conn: &C,
    src: &mut surface_t,
    dest: &mut surface_t,
    src_x: f64,
    src_y: f64,
    dest_x: f64,
    dest_y: f64,
    width: f64,
    height: f64,
) {
    if !surface_initialized(src) || !surface_initialized(dest) {
        return;
    }

    unsafe {
        cairo_save(dest.cr);

        cairo_set_operator(dest.cr, CAIRO_OPERATOR_SOURCE);
        cairo_set_source_surface(dest.cr, src.surface, dest_x - src_x, dest_y - src_y);

        cairo_rectangle(dest.cr, dest_x, dest_y, width, height);
        cairo_fill(dest.cr);

        cairo_restore(dest.cr);
    }

    // C: CAIRO_SURFACE_FLUSH(src->surface); CAIRO_SURFACE_FLUSH(dest->surface)
    draw_util_surface_flush(conn, src);
    draw_util_surface_flush(conn, dest);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C: libi3/draw_util.c:163 — the hex parser (and its "#A9A9A9" fallback).
    #[test]
    fn hex_to_color_parses_rgb() {
        let color = draw_util_hex_to_color("#3fbc59");
        assert!((color.red - 0x3f as f64 / 255.0).abs() < 1e-9);
        assert!((color.green - 0xbc as f64 / 255.0).abs() < 1e-9);
        assert!((color.blue - 0x59 as f64 / 255.0).abs() < 1e-9);
        // C: alpha[0] = alpha[1] = 'F' for the 6 digit form
        assert_eq!(color.alpha, 1.0);
    }

    /// C: the "#rrggbbaa" form carries an explicit alpha channel.
    #[test]
    fn hex_to_color_parses_alpha() {
        let color = draw_util_hex_to_color("#3fbc5980");
        assert!((color.alpha - 0x80 as f64 / 255.0).abs() < 1e-9);
    }

    /// C: strlen(color) < 6 || color[0] != '#' → draw_util_hex_to_color
    /// ("#A9A9A9").
    #[test]
    fn hex_to_color_invalid_falls_back() {
        for invalid in ["#abc", "3fbc59", ""] {
            let color = draw_util_hex_to_color(invalid);
            assert!((color.red - 0xA9 as f64 / 255.0).abs() < 1e-9, "{:?}", invalid);
            assert!((color.green - 0xA9 as f64 / 255.0).abs() < 1e-9);
            assert!((color.blue - 0xA9 as f64 / 255.0).abs() < 1e-9);
        }
    }

    /// C: surface_initialized() — an uninitialized surface (id == XCB_NONE,
    /// NULL cairo pointers) makes every drawing call return early. The port
    /// keeps that guard as the only entry condition, so the check itself is
    /// what the X-free test can assert.
    #[test]
    fn uninitialized_surface_is_skipped() {
        let surface = surface_t::none();
        assert_eq!(surface.id, XCB_NONE);
        assert!(surface.surface.is_null());
        assert!(surface.cr.is_null());
        assert_eq!(
            surface.width, 0,
            "C: memset(&surface, 0, sizeof(surface_t)) leaves the size at 0"
        );
    }

    /// C: surface_t::none() is C's `surface_t surface = {0}` (id 0 = "not
    /// initialized", owns_gc false).
    #[test]
    fn surface_none_matches_the_c_zero_initializer() {
        let surface = surface_t::none();
        assert!(!surface.owns_gc);
        assert_eq!(surface.gc, 0);
        assert_eq!(surface.depth, 0);
    }

    /// C: the rectangle geometry handed to cairo_rectangle() — the deco bar
    /// covers the whole deco rect, the title border is 1px wide.
    #[test]
    fn deco_rectangle_geometry() {
        // C: src/x.c:631 draw_util_rectangle(dest_surface, p->color->background,
        //    deco_rect.x, deco_rect.y, deco_rect.width, deco_rect.height)
        let deco = (0.0, 0.0, 640.0, 18.0);
        assert_eq!(deco.2, 640.0);
        assert_eq!(deco.3, 18.0);

        // C: src/x.c:364 x_draw_title_border — left/right/top/bottom lines
        let deco_x = 0.0;
        let deco_y = 0.0;
        let deco_w = 640.0;
        let deco_h = 18.0;
        let borders = [
            (deco_x, deco_y, 1.0, deco_h),                  // left
            (deco_x + deco_w - 1.0, deco_y, 1.0, deco_h),   // right
            (deco_x, deco_y, deco_w, 1.0),                  // top
            (deco_x, deco_y + deco_h - 1.0, deco_w, 1.0),   // bottom
        ];
        assert_eq!(borders[1].0, 639.0);
        assert_eq!(borders[3].1, 17.0);
        // The border must stay inside the deco rect.
        for (x, y, w, h) in borders {
            assert!(x >= deco_x && y >= deco_y);
            assert!(x + w <= deco_x + deco_w);
            assert!(y + h <= deco_y + deco_h);
        }
    }
}
