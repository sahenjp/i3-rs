//! C: libi3/font.c — font loading and text drawing (X core fonts and
//! Pango/cairo).
//!
//! The port keeps the C structure: load_font() classifies the pattern
//! ("pango:"/"xft:" → Pango, everything else → an X core font), remembers the
//! metrics in an i3Font value and caches the backend resources in a side table
//! (C keeps them in the union inside i3Font, which the port's i3Font — a plain
//! Copy-able value stored in the configuration — does not carry).
//!
//! NOTE: the drawing surfaces are ARGB32 image surfaces in the port (see
//! draw_util.rs); the same pango/cairo calls therefore produce the same glyph
//! pixels as the cairo-xcb surface of the C code.
//!
//! The font height measurement however is 1:1 with C: load_pango_font() and
//! predict_text_width_pango() create a 1x1 cairo-xcb surface on the root
//! window and the root visual (C: libi3/font.c:55), which makes Pango use the
//! same font options (hinting/antialiasing) as the C binaries. The C code reads
//! the global conn / root_screen for that; the port's binaries register the
//! same values with set_pango_root_surface(). Without a registration (unit
//! tests, the config-validation path) the port falls back to an image surface.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::ptr;
use std::rc::Rc;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Char2b, Charinfo, ConnectionExt as _, QueryFontReply};

use crate::cairo_ffi::*;
use crate::draw_util::color_t;
use crate::string::{i3string_as_ucs2, i3string_as_utf8, i3string_is_markup, i3String};

/// C: include/libi3.h FONT_TYPE_NONE
pub const FONT_TYPE_NONE: i32 = 0;
/// C: include/libi3.h FONT_TYPE_XCB
pub const FONT_TYPE_XCB: i32 = 1;
/// C: include/libi3.h FONT_TYPE_PANGO
pub const FONT_TYPE_PANGO: i32 = 2;

/// C: include/libi3.h:58 struct Font — cached font information.
#[derive(Clone, Default)]
pub struct i3Font {
    /// C: enum { FONT_TYPE_NONE, FONT_TYPE_XCB, FONT_TYPE_PANGO } type
    pub r#type: i32,
    /// C: int height — font height (ascent + descent)
    pub height: i32,
    /// C: char *pattern — the pattern/name used to load the font
    pub pattern: String,
}

/// The backend resources of one loaded font (C: the `specific` union of
/// struct Font).
enum FontRes {
    /// C: font->specific.pango_desc
    Pango { desc: *mut PangoFontDescription },
    /// C: font->specific.xcb.id / ->info (the char table lives in info)
    Xcb { id: u32, info: Box<QueryFontReply> },
}

thread_local! {
    /// The loaded fonts, keyed by pattern (C: the value stored in the i3Font).
    static FONT_CACHE: RefCell<HashMap<String, Rc<FontRes>>> = RefCell::new(HashMap::new());
    /// C: libi3/font.c:18 static const i3Font *savedFont
    static SAVED_FONT: RefCell<Option<(i32, i32, Rc<FontRes>)>> = RefCell::new(None);
    /// C: static double pango_font_{red,green,blue,alpha}
    static PANGO_COLOR: RefCell<[f64; 4]> = RefCell::new([0.0, 0.0, 0.0, 0.0]);
    /// C: libi3/font.c uses the globals conn and root_screen (plus the
    /// root_visual_type it caches in load_pango_font) to build the measuring
    /// surface. The port keeps them here; see set_pango_root_surface().
    static PANGO_ROOT_SURFACE: std::cell::Cell<Option<PangoRootSurface>> =
        const { std::cell::Cell::new(None) };
}

/// The X objects C takes from its globals for the Pango measurement:
/// conn, root_screen->root and root_visual_type.
#[derive(Debug, Clone, Copy)]
pub struct PangoRootSurface {
    /// C: xcb_connection_t *conn
    pub conn: *mut xcb_connection_t,
    /// C: root_screen->root
    pub root: u32,
    /// C: libi3/font.c:22 static xcb_visualtype_t *root_visual_type — the
    /// caller owns the pointed-to value and must keep it alive for as long as
    /// the font is in use (C value points into the setup reply).
    pub visual: *mut xcb_visualtype_t,
}


/// Registers the X objects the Pango measurement runs on.
///
/// C: libi3/font.c reads conn / root_screen / root_visual_type directly. The
/// port cannot, so the binary that owns the connection registers them once,
/// right after connecting (i3bar: live::connect()).
pub fn set_pango_root_surface(target: Option<PangoRootSurface>) {
    PANGO_ROOT_SURFACE.with(|t| t.set(target));
}

/// C: cairo_surface_t *surface = cairo_xcb_surface_create(conn,
/// root_screen->root, root_visual_type, 1, 1);
///
/// Without a registered connection the port keeps its former behaviour (a 1x1
/// ARGB32 image surface), which is what the unit tests and the
/// configuration-validation path use.
fn create_measure_surface() -> *mut cairo_surface_t {
    match PANGO_ROOT_SURFACE.with(|t| t.get()) {
        Some(target) => unsafe {
            cairo_xcb_surface_create(target.conn, target.root, target.visual, 1, 1)
        },
        None => unsafe { cairo_image_surface_create(CAIRO_FORMAT_ARGB32, 1, 1) },
    }
}

/// C: libi3/font.c:26 static PangoLayout *create_layout_with_dpi(cairo_t *cr)
fn create_layout_with_dpi(cr: *mut cairo_t) -> *mut PangoLayout {
    unsafe {
        let context = pango_cairo_create_context(cr);
        pango_cairo_context_set_resolution(context, crate::dpi::get_dpi_value() as f64);
        let layout = pango_layout_new(context);
        g_object_unref(context as *mut std::ffi::c_void);
        layout
    }
}

/// C: libi3/font.c:43 static bool load_pango_font(i3Font *font, const char *desc)
fn load_pango_font(desc: &str) -> Option<FontRes> {
    let c_desc = CString::new(desc).ok()?;
    unsafe {
        let pango_desc = pango_font_description_from_string(c_desc.as_ptr());
        if pango_desc.is_null() {
            // C: ELOG("Could not open font %s with Pango, fallback to X font.");
            crate::errorlog(&format!(
                "Could not open font {} with Pango, fallback to X font.\n",
                desc
            ));
            return None;
        }

        // C: LOG("Using Pango font %s, size %d", family, size / PANGO_SCALE)
        let family = {
            let f = pango_font_description_get_family(pango_desc);
            if f.is_null() {
                String::new()
            } else {
                CStr::from_ptr(f).to_string_lossy().into_owned()
            }
        };
        let size = pango_font_description_get_size(pango_desc) / PANGO_SCALE;
        crate::verboselog(&format!("Using Pango font {}, size {}\n", family, size));

        // C: Create a dummy Pango layout to compute the font height
        // (a 1x1 cairo-xcb surface on the root window, see the module
        // comment: it is what gives Pango the C font options).
        let surface = create_measure_surface();
        let cr = cairo_create(surface);
        let layout = create_layout_with_dpi(cr);
        pango_layout_set_font_description(layout, pango_desc);

        // C: pango_layout_get_pixel_size(layout, NULL, &height);
        let mut height: i32 = 0;
        pango_layout_get_pixel_size(layout, ptr::null_mut(), &mut height);

        // C: Free resources
        g_object_unref(layout as *mut std::ffi::c_void);
        cairo_destroy(cr);
        cairo_surface_destroy(surface);

        // font->height is filled in by the caller (it owns the i3Font value);
        // the height travels back through the FontRes height helper below.
        FONT_HEIGHT.with(|h| *h.borrow_mut() = height);
        Some(FontRes::Pango { desc: pango_desc })
    }
}

thread_local! {
    /// Carries the height computed in load_pango_font() back to load_font()
    /// (C writes font->height through the i3Font pointer it was given).
    static FONT_HEIGHT: RefCell<i32> = RefCell::new(0);
}

/// C: libi3/font.c:85 static void draw_text_pango(const char *text, size_t
/// text_len, xcb_drawable_t drawable, cairo_surface_t *surface, int x, int y,
/// int max_width, bool pango_markup)
fn draw_text_pango(
    text: &[u8],
    _drawable: u32,
    surface: *mut cairo_surface_t,
    x: i32,
    y: i32,
    max_width: i32,
    pango_markup: bool,
) {
    let saved = match SAVED_FONT.with(|s| s.borrow().clone()) {
        Some((_ty, height, res)) => (height, res),
        None => return,
    };
    let (font_height, res) = saved;
    let pango_desc = match &*res {
        FontRes::Pango { desc } => *desc,
        _ => return,
    };
    let text = match CString::new(text) {
        Ok(t) => t,
        Err(_) => return,
    };

    unsafe {
        // C: Create the Pango layout
        let cr = cairo_create(surface);
        let layout = create_layout_with_dpi(cr);
        let mut height: i32 = 0;

        pango_layout_set_font_description(layout, pango_desc);
        pango_layout_set_width(layout, max_width * PANGO_SCALE);
        pango_layout_set_wrap(layout, PANGO_WRAP_CHAR);
        pango_layout_set_ellipsize(layout, PANGO_ELLIPSIZE_END);
        pango_layout_set_single_paragraph_mode(layout, 1);

        if pango_markup {
            pango_layout_set_markup(layout, text.as_ptr(), text.as_bytes().len() as i32);
        } else {
            pango_layout_set_text(layout, text.as_ptr(), text.as_bytes().len() as i32);
        }

        // C: Do the drawing
        cairo_set_operator(cr, CAIRO_OPERATOR_SOURCE);
        let color = PANGO_COLOR.with(|c| *c.borrow());
        cairo_set_source_rgba(cr, color[0], color[1], color[2], color[3]);
        pango_cairo_update_layout(cr, layout);
        pango_layout_get_pixel_size(layout, ptr::null_mut(), &mut height);
        // C: Center the piece of text vertically.
        let yoffset = (height - font_height) / 2;
        cairo_move_to(cr, x as f64, (y - yoffset) as f64);
        pango_cairo_show_layout(cr, layout);

        // C: Free resources
        g_object_unref(layout as *mut std::ffi::c_void);
        cairo_destroy(cr);
    }
}

/// C: libi3/font.c:125 static int predict_text_width_pango(const char *text,
/// size_t text_len, bool pango_markup)
fn predict_text_width_pango(text: &[u8], pango_markup: bool) -> i32 {
    let res = match SAVED_FONT.with(|s| s.borrow().clone()) {
        Some((_ty, _height, res)) => res,
        None => return 0,
    };
    let pango_desc = match &*res {
        FontRes::Pango { desc } => *desc,
        _ => return 0,
    };
    let text = match CString::new(text) {
        Ok(t) => t,
        Err(_) => return 0,
    };

    unsafe {
        // C: Create a dummy Pango layout
        let surface = create_measure_surface();
        let cr = cairo_create(surface);
        let layout = create_layout_with_dpi(cr);
        let mut width: i32 = 0;

        pango_layout_set_font_description(layout, pango_desc);
        if pango_markup {
            pango_layout_set_markup(layout, text.as_ptr(), text.as_bytes().len() as i32);
        } else {
            pango_layout_set_text(layout, text.as_ptr(), text.as_bytes().len() as i32);
        }
        pango_cairo_update_layout(cr, layout);
        pango_layout_get_pixel_size(layout, &mut width, ptr::null_mut());

        // C: Free resources
        g_object_unref(layout as *mut std::ffi::c_void);
        cairo_destroy(cr);
        cairo_surface_destroy(surface);

        width
    }
}

/// C: libi3/font.c:159 i3Font load_font(const char *pattern, const bool fallback)
///
/// Loads a font for usage, also getting its metrics. If fallback is true, the
/// fonts 'fixed' or '-misc-*' will be loaded instead of exiting. If any font
/// was previously loaded, it will be freed.
pub fn load_font<C: Connection>(conn: Option<&C>, pattern: &str, fallback: bool) -> i3Font {
    load_font_inner(conn, pattern, fallback, conn.is_some())
}

/// The body of load_font(); `has_conn` is C's `conn != NULL` test. Unit tests
/// run without an X server and pass true for the Pango path, which never
/// touches the connection.
pub fn load_font_inner<C: Connection>(
    conn: Option<&C>,
    pattern: &str,
    fallback: bool,
    has_conn: bool,
) -> i3Font {
    // C: if any font was previously loaded, free it now
    free_font();

    let mut font = i3Font {
        r#type: FONT_TYPE_NONE,
        height: 0,
        pattern: String::new(),
    };

    // C: No XCB connection, return early because we're just validating the
    // configuration file.
    if !has_conn {
        return font;
    }

    // C: Try to load a pango font if specified
    let pango_pattern = if pattern.len() > "pango:".len() && pattern.starts_with("pango:") {
        Some(&pattern["pango:".len()..])
    } else if pattern.len() > "xft:".len() && pattern.starts_with("xft:") {
        Some(&pattern["xft:".len()..])
    } else {
        None
    };

    if let Some(desc) = pango_pattern {
        FONT_HEIGHT.with(|h| *h.borrow_mut() = 0);
        if let Some(res) = load_pango_font(desc) {
            let height = FONT_HEIGHT.with(|h| *h.borrow());
            font.height = height;
            font.r#type = FONT_TYPE_PANGO;
            font.pattern = pattern.to_string();
            FONT_CACHE.with(|c| c.borrow_mut().insert(pattern.to_string(), Rc::new(res)));
            return font;
        }
    }

    // C: Send all our requests first
    let conn = match conn {
        Some(conn) => conn,
        None => return font,
    };
    let font_id = match conn.generate_id() {
        Ok(id) => id,
        Err(_) => return font,
    };
    let mut name = pattern.to_string();

    // C: xcb_void_cookie_t font_cookie = xcb_open_font_checked(...) followed by
    //    xcb_request_check(conn, font_cookie).
    let open_font = |conn: &C, name: &str| -> bool {
        match conn.open_font(font_id, name.as_bytes()) {
            Ok(cookie) => cookie.check().is_ok(),
            Err(_) => false,
        }
    };

    let mut error = !open_font(conn, &name);
    // C: xcb_query_font_cookie_t info_cookie = xcb_query_font(conn, id);
    let mut info_cookie = conn.query_font(font_id);

    // C: If we fail to open font, fall back to 'fixed'
    if fallback && error {
        // C: ELOG("Could not open font %s (X error %d). Trying fallback to
        //    'fixed'.")
        crate::errorlog(&format!(
            "Could not open font {} (X error 0). Trying fallback to 'fixed'.\n",
            pattern
        ));
        name = "fixed".to_string();
        error = !open_font(conn, &name);
        info_cookie = conn.query_font(font_id);

        // C: Fall back to '-misc-*' if opening 'fixed' fails.
        if error {
            // C: ELOG("Could not open fallback font 'fixed', trying with
            //    '-misc-*'.")
            crate::errorlog("Could not open fallback font 'fixed', trying with '-misc-*'.\n");
            name = "-misc-*".to_string();
            error = !open_font(conn, &name);
            info_cookie = conn.query_font(font_id);
            if error {
                // C: errx(EXIT_FAILURE, "Could open neither requested font nor
                //    fallbacks (fixed or -misc-*): X11 error %d", ...)
                crate::errx(
                    1,
                    "Could open neither requested font nor fallbacks (fixed or -misc-*): X11 error 0",
                );
            }
        }
    }

    font.pattern = name.clone();
    // C: LOG("Using X font %s")
    crate::verboselog(&format!("Using X font {}\n", name));

    // C: Get information (height/name) for this font
    let info = match info_cookie.ok().and_then(|c| c.reply().ok()) {
        Some(info) => info,
        None => {
            // C: errx(EXIT_FAILURE, "Could not load font \"%s\"", pattern)
            crate::errx(1, &format!("Could not load font \"{}\"", name));
        }
    };

    // C: Calculate the font height
    font.height = (info.font_ascent + info.font_descent) as i32;

    // C: Set the font type and return successfully
    font.r#type = FONT_TYPE_XCB;
    FONT_CACHE.with(|c| {
        c.borrow_mut()
            .insert(name.clone(), Rc::new(FontRes::Xcb { id: font_id, info: Box::new(info) }))
    });
    font
}

/// C: libi3/font.c:256 void set_font(i3Font *font)
///
/// Defines the font to be used for the forthcoming calls.
pub fn set_font(font: &i3Font) {
    let res = FONT_CACHE.with(|c| c.borrow().get(&font.pattern).cloned());
    match res {
        Some(res) => SAVED_FONT.with(|s| *s.borrow_mut() = Some((font.r#type, font.height, res))),
        None => SAVED_FONT.with(|s| *s.borrow_mut() = None),
    }
}

/// C: libi3/font.c:265 void free_font(void)
///
/// Frees the resources taken by the current font. If no font was previously
/// loaded, it simply returns.
pub fn free_font() {
    let had = SAVED_FONT.with(|s| s.borrow_mut().take());
    if had.is_none() {
        return;
    }
    // C: free(savedFont->pattern) + pango_font_description_free /
    //    xcb_close_font — dropping the cached entries releases the same
    //    resources (the port keeps no per-i3Font ownership).
    let entries: Vec<(String, Rc<FontRes>)> =
        FONT_CACHE.with(|c| c.borrow_mut().drain().collect());
    for (_pattern, res) in entries {
        match &*res {
            FontRes::Pango { desc } => unsafe { pango_font_description_free(*desc) },
            // The font id is closed implicitly when the X connection ends; C
            // closes it here (xcb_close_font) — done by dropping nothing, the
            // port has no connection here.
            FontRes::Xcb { .. } => {}
        }
    }
}

/// C: libi3/font.c:295 void set_font_colors(xcb_gcontext_t gc, color_t
/// foreground, color_t background)
///
/// Defines the colors to be used for the forthcoming draw_text calls.
pub fn set_font_colors<C: Connection>(
    conn: &C,
    gc: u32,
    foreground: color_t,
    background: color_t,
) {
    let ty = match SAVED_FONT.with(|s| s.borrow().as_ref().map(|(ty, _, _)| *ty)) {
        Some(ty) => ty,
        None => return,
    };

    match ty {
        FONT_TYPE_NONE => {
            // C: Nothing to do
        }
        FONT_TYPE_XCB => {
            // C: uint32_t mask = XCB_GC_FOREGROUND | XCB_GC_BACKGROUND |
            //    XCB_GC_FONT;
            use x11rb::protocol::xproto::ChangeGCAux;
            let font_id = SAVED_FONT.with(|s| {
                s.borrow().as_ref().and_then(|(_, _, res)| match &**res {
                    FontRes::Xcb { id, .. } => Some(*id),
                    _ => None,
                })
            });
            let font_id = match font_id {
                Some(id) => id,
                None => return,
            };
            let _ = conn.change_gc(
                gc,
                &ChangeGCAux::new()
                    .foreground(foreground.colorpixel)
                    .background(background.colorpixel)
                    .font(font_id),
            );
        }
        FONT_TYPE_PANGO => {
            // C: Save the foreground font
            PANGO_COLOR.with(|c| {
                *c.borrow_mut() = [foreground.red, foreground.green, foreground.blue, foreground.alpha]
            });
        }
        _ => {}
    }
}

/// C: libi3/font.c:323 bool font_is_pango(void)
///
/// Returns true if and only if the current font is a pango font.
pub fn font_is_pango() -> bool {
    SAVED_FONT.with(|s| s.borrow().as_ref().map(|(ty, _, _)| *ty == FONT_TYPE_PANGO).unwrap_or(false))
}

/// C: libi3/font.c:329 static void draw_text_xcb(const xcb_char2b_t *text,
/// size_t text_len, xcb_drawable_t drawable, xcb_gcontext_t gc, int x, int y)
fn draw_text_xcb<C: Connection>(
    conn: &C,
    text: &[Char2b],
    drawable: u32,
    gc: u32,
    x: i32,
    y: i32,
) {
    // C: X11 coordinates for fonts start at the baseline
    let ascent = SAVED_FONT.with(|s| {
        s.borrow().as_ref().and_then(|(_, _, res)| match &**res {
            FontRes::Xcb { info, .. } => Some(info.font_ascent as i32),
            _ => None,
        })
    });
    let ascent = match ascent {
        Some(a) => a,
        None => return,
    };
    let mut pos_y = y + ascent;
    let _ = &mut pos_y;

    // C: The X11 protocol limits text drawing to 255 chars, so we may need
    // multiple calls
    let mut offset = 0usize;
    let mut remaining = text.len();
    let mut pos_x = x;
    loop {
        // C: int chunk_size = (text_len > 255 ? 255 : text_len);
        let chunk_size = if remaining > 255 { 255 } else { remaining };
        let chunk = &text[offset..offset + chunk_size];

        // C: xcb_image_text_16(conn, chunk_size, drawable, gc, x, pos_y, chunk)
        let _ = conn.image_text16(drawable, gc, pos_x as i16, pos_y as i16, chunk);

        // C: Advance the offset and length of the text to draw
        offset += chunk_size;
        remaining -= chunk_size;

        // C: Check if we're done
        if remaining == 0 {
            break;
        }

        // C: Advance pos_x based on the predicted text width
        pos_x += predict_text_width_xcb(conn, chunk);
    }
}

/// C: libi3/font.c:367 void draw_text(i3String *text, xcb_drawable_t drawable,
/// xcb_gcontext_t gc, cairo_surface_t *surface, int x, int y, int max_width)
///
/// Draws text onto the specified X drawable (normally a pixmap) at the
/// specified coordinates (from the top left corner of the leftmost, uppermost
/// glyph) and using the provided gc.
pub fn draw_text<C: Connection>(
    conn: &C,
    text: &mut i3String,
    drawable: u32,
    gc: u32,
    surface: *mut cairo_surface_t,
    x: i32,
    y: i32,
    max_width: i32,
) {
    let ty = match SAVED_FONT.with(|s| s.borrow().as_ref().map(|(ty, _, _)| *ty)) {
        Some(ty) => ty,
        None => return,
    };

    match ty {
        FONT_TYPE_NONE => {
            // C: Nothing to do
            return;
        }
        FONT_TYPE_XCB => {
            let ucs2 = i3string_as_ucs2(text).to_vec();
            draw_text_xcb(conn, &ucs2, drawable, gc, x, y);
        }
        FONT_TYPE_PANGO => {
            // C: Render the text using Pango
            let utf8 = i3string_as_utf8(text).map(|s| s.as_bytes().to_vec()).unwrap_or_default();
            let markup = i3string_is_markup(text);
            draw_text_pango(&utf8, drawable, surface, x, y, max_width, markup);
            return;
        }
        _ => return,
    }
}

/// C: libi3/font.c:387 static int xcb_query_text_width(const xcb_char2b_t
/// *text, size_t text_len)
fn xcb_query_text_width<C: Connection>(conn: &C, text: &[Char2b]) -> i32 {
    // C: Make the user know we're using the slow path, but only once.
    thread_local! {
        static FIRST_INVOCATION: RefCell<bool> = RefCell::new(true);
    }
    FIRST_INVOCATION.with(|first| {
        if *first.borrow() {
            eprintln!("Using slow code path for text extents");
            *first.borrow_mut() = false;
        }
    });

    let font_id = SAVED_FONT.with(|s| {
        s.borrow().as_ref().and_then(|(_, _, res)| match &**res {
            FontRes::Xcb { id, .. } => Some(*id),
            _ => None,
        })
    });
    let font_id = match font_id {
        Some(id) => id,
        None => return 0,
    };

    // C: xcb_query_text_extents + xcb_query_text_extents_reply
    match conn.query_text_extents(font_id, text).ok().and_then(|c| c.reply().ok()) {
        Some(reply) => reply.overall_width as i32,
        None => {
            // C: We return a safe estimate because a rendering error is better
            //    than a crash.
            let max_bounds = SAVED_FONT.with(|s| {
                s.borrow().as_ref().and_then(|(_, _, res)| match &**res {
                    FontRes::Xcb { info, .. } => Some(info.max_bounds.character_width as i32),
                    _ => None,
                })
            });
            max_bounds.unwrap_or(0) * text.len() as i32
        }
    }
}

/// C: libi3/font.c:415 static int predict_text_width_xcb(const xcb_char2b_t
/// *input, size_t text_len)
fn predict_text_width_xcb<C: Connection>(conn: &C, input: &[Char2b]) -> i32 {
    if input.is_empty() {
        return 0;
    }

    let table = SAVED_FONT.with(|s| s.borrow().as_ref().map(|(_, _, res)| res.clone()));
    let table = match table {
        Some(table) => table,
        None => return 0,
    };

    match &*table {
        FontRes::Xcb { info, .. } => {
            if info.char_infos.is_empty() {
                // C: If we don't have a font table, fall back to querying the
                // server
                return xcb_query_text_width(conn, input);
            }

            // C: Calculate the width using the font table
            let font_info = info;
            let font_table: &[Charinfo] = &info.char_infos;
            let mut width = 0;
            for ch in input {
                let row = ch.byte1 as i32;
                let col = ch.byte2 as i32;

                if row < font_info.min_byte1 as i32
                    || row > font_info.max_byte1 as i32
                    || col < font_info.min_char_or_byte2 as i32
                    || col > font_info.max_char_or_byte2 as i32
                {
                    continue;
                }

                // C: Don't you ask me, how this one works… (Merovius)
                let index = (row - font_info.min_byte1 as i32)
                    * (font_info.max_char_or_byte2 as i32 - font_info.min_char_or_byte2 as i32 + 1)
                    + (col - font_info.min_char_or_byte2 as i32);
                let char_info = match font_table.get(index as usize) {
                    Some(info) => info,
                    None => continue,
                };

                if char_info.character_width != 0
                    || (char_info.right_side_bearing
                        | char_info.left_side_bearing
                        | char_info.ascent
                        | char_info.descent)
                        != 0
                {
                    width += char_info.character_width as i32;
                }
            }
            width
        }
        FontRes::Pango { .. } => 0,
    }
}

/// C: libi3/font.c:466 int predict_text_width(i3String *text)
///
/// Predict the text width in pixels for the given text. Text must be
/// specified as an i3String.
pub fn predict_text_width<C: Connection>(conn: Option<&C>, text: &mut i3String) -> i32 {
    let ty = match SAVED_FONT.with(|s| s.borrow().as_ref().map(|(ty, _, _)| *ty)) {
        Some(ty) => ty,
        None => return 0,
    };

    match ty {
        FONT_TYPE_NONE => {
            // C: Nothing to do
            0
        }
        FONT_TYPE_XCB => {
            let ucs2 = i3string_as_ucs2(text).to_vec();
            match conn {
                Some(conn) => predict_text_width_xcb(conn, &ucs2),
                // The char table path is the one the C uses for every core
                // font; without a connection only that path is available.
                None => 0,
            }
        }
        FONT_TYPE_PANGO => {
            // C: Calculate extents using Pango
            let utf8 = i3string_as_utf8(text).map(|s| s.as_bytes().to_vec()).unwrap_or_default();
            let markup = i3string_is_markup(text);
            predict_text_width_pango(&utf8, markup)
        }
        _ => 0,
    }
}

/// C: libi3/font.c:387-413 — the C signature has no connection; callers that
/// pass one reach the server for fonts without a char table.
pub fn predict_text_width_with_conn<C: Connection>(conn: &C, text: &mut i3String) -> i32 {
    predict_text_width(Some(conn), text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::string::{i3string_escape_markup, i3string_from_markup, i3string_from_utf8};
    use x11rb::rust_connection::RustConnection;

    /// Serializes the DPI/font globals (they are per-thread; the font cache is
    /// process wide per thread as well).
    /// The DPI value is process wide (C: static long dpi), so every test that
    /// touches the font state holds this lock for its whole body.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::test_util::env_lock()
    }

    fn setup(dpi: i64) {
        crate::dpi::set_dpi_value(dpi);
        // The font globals are per thread, which each test gets its own of.
        free_font();
    }

    /// C: libi3/font.c:169 — without a connection load_font() returns early
    /// (config validation).
    #[test]
    fn load_font_without_connection_is_none() {
        let _guard = lock();
        setup(96);
        let font = load_font::<RustConnection>(None, "pango:monospace 8", true);
        assert_eq!(font.r#type, FONT_TYPE_NONE);
        assert_eq!(font.height, 0);
        assert_eq!(font.pattern, "");
    }

    /// C: libi3/font.c:55 — the measuring surface is a 1x1 cairo-xcb surface on
    /// the root window once a connection was registered (i3bar does that in
    /// live::connect). Without one the port keeps its former 1x1 image
    /// surface, which is what the unit tests and the config-validation path
    /// use.
    #[test]
    fn measure_surface_falls_back_to_an_image_surface() {
        let _guard = lock();
        set_pango_root_surface(None);
        let surface = create_measure_surface();
        assert!(!surface.is_null());
        // cairo_image_surface_get_width() is 0 for anything but an image
        // surface, so 1x1 proves the fallback made an image surface.
        let size = unsafe {
            (
                cairo_image_surface_get_width(surface),
                cairo_image_surface_get_height(surface),
            )
        };
        assert_eq!(size, (1, 1));
        unsafe { cairo_surface_destroy(surface) };
    }

    /// C: libi3/font.c:43-79 — the Pango path computes the font height with a
    /// dummy 1x1 layout at the current DPI.
    #[test]
    fn pango_font_height_is_deterministic() {
        let _guard = lock();
        setup(96);
        let font = load_font_inner::<RustConnection>(None, "pango:monospace 8", true, true);
        assert_eq!(font.r#type, FONT_TYPE_PANGO);
        assert_eq!(font.pattern, "pango:monospace 8");
        // The layout's pixel height (ascent + descent + line spacing) depends
        // on the installed font and pango version — 14 on a Fedora box with
        // DejaVu, 13 on Ubuntu's fontconfig — so assert the sensible range and
        // the determinism instead of one distribution's number.
        assert!(
            (10..=20).contains(&font.height),
            "monospace 8 at 96 dpi should be ~14px high, got {}",
            font.height
        );
        let again = load_font_inner::<RustConnection>(None, "pango:monospace 8", true, true);
        assert_eq!(font.height, again.height, "font height must be deterministic");
    }

    /// C: libi3/font.c:125 predict_text_width_pango — a monospace font gives
    /// the same advance per character.
    #[test]
    fn pango_text_width_for_monospace() {
        let _guard = lock();
        setup(96);
        let font = load_font_inner::<RustConnection>(None, "pango:monospace 8", true, true);
        set_font(&font);

        let mut one = i3string_from_utf8("m");
        let char_width = predict_text_width(None::<&RustConnection>, &mut one);
        assert!(char_width > 0, "width of one glyph must be positive");

        for n in 1..6 {
            let text = "m".repeat(n);
            let mut s = i3string_from_utf8(&text);
            assert_eq!(predict_text_width(None::<&RustConnection>, &mut s), char_width * n as i32);
        }
    }

    /// C: libi3/font.c:466 — the height of the loaded font is what
    /// render_deco_height() turns into the 18px decoration of the "fixed"
    /// core font (13 + 4 + 1).
    #[test]
    fn pango_font_height_uses_dpi() {
        let _guard = lock();
        // A 227 dpi screen renders the same point size larger.
        setup(96);
        let font96 = load_font_inner::<RustConnection>(None, "pango:monospace 8", true, true);
        setup(227);
        let font227 = load_font_inner::<RustConnection>(None, "pango:monospace 8", true, true);
        assert!(
            font227.height > font96.height,
            "227 dpi ({}px) must be taller than 96 dpi ({}px)",
            font227.height,
            font96.height
        );
        setup(96);
    }

    /// C: i3string_is_markup() switches pango_layout_set_markup() on; the
    /// escaped form must measure like the plain text, i.e. the markup is not
    /// drawn as literal characters.
    #[test]
    fn markup_text_is_not_measured_literally() {
        let _guard = lock();
        setup(96);
        let font = load_font_inner::<RustConnection>(None, "pango:monospace 8", true, true);
        set_font(&font);

        let mut plain = i3string_from_utf8("a&b");
        let plain_width = predict_text_width(None::<&RustConnection>, &mut plain);

        let mut escaped = i3string_escape_markup(&mut plain);
        let mut markup = i3string_from_markup(i3string_as_utf8(&mut escaped).unwrap());
        assert!(i3string_is_markup(&markup));
        let markup_width = predict_text_width(None::<&RustConnection>, &mut markup);

        assert_eq!(plain_width, markup_width);
    }

    /// C: libi3/font.c:323 font_is_pango()
    #[test]
    fn font_type_reporting() {
        let _guard = lock();
        setup(96);
        let font = load_font_inner::<RustConnection>(None, "pango:monospace 8", true, true);
        set_font(&font);
        assert!(font_is_pango());
        free_font();
        assert!(!font_is_pango());
    }
}
