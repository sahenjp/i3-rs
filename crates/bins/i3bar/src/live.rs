//! The real X11 half of i3bar: the x11rb connection and the drawing backend.
//!
//! C: xcb.c's global 'xcb_connection_t *xcb_connection' (init_xcb_early opens
//! it) plus every draw_util_*/draw_text call the C code makes directly on it.
//!
//! The port keeps the same split as the rest of i3bar: the control flow in
//! xcb.rs stays X-free and emits [DrawCmd] values, and this module turns them
//! into libi3 (cairo/pango) calls on the real connection. The connection is a
//! plain x11rb RustConnection: i3bar is a separate process from i3, so it gets
//! its own connection, exactly like the C binaries do.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
use x11rb::xcb_ffi::XCBConnection;

use libi3::draw_util::{self, color_t as libi3_color_t, surface_t};

use crate::outputs::outputs;
use crate::xcb::{color_t, surface_id, DrawBackend, DrawCmd, XCB_NONE};

thread_local! {
    /// C: xcb.c:45 xcb_connection_t *xcb_connection;
    static CONNECTION: RefCell<Option<Rc<XCBConnection>>> = const { RefCell::new(None) };
    /// C: xcb.c:46 xcb_screen_t *root_screen; (its root window)
    static ROOT: Cell<u32> = const { Cell::new(0) };
    /// C: xcb.c:67 uint8_t depth;
    static DEPTH: Cell<u8> = const { Cell::new(24) };
    /// C: xcb.c:36 int screen; — the screen index xcb_connect() reported.
    static SCREEN: Cell<usize> = const { Cell::new(0) };
}

/// The connection opened by [connect], if any (C: xcb_connection).
pub fn connection() -> Option<Rc<XCBConnection>> {
    CONNECTION.with(|c| c.borrow().clone())
}

/// C: xcb.c:46 xcb_screen_t *root_screen — the root window.
pub fn root() -> u32 {
    ROOT.with(|r| r.get())
}

/// C: xcb.c:67 uint8_t depth — the depth of the bar windows.
pub fn depth() -> u8 {
    DEPTH.with(|d| d.get())
}

/// Sets the depth of the bar windows: C keeps it in the one global depth that
/// init_xcb_early() picks (root depth, or 32 when -t found an ARGB32 visual).
/// The drawing surfaces and pixmaps use the same value.
pub fn set_depth(depth: u8) {
    DEPTH.with(|d| d.set(depth));
}

/// C: xcb.c:36 int screen
pub fn screen() -> usize {
    SCREEN.with(|s| s.get())
}

/// C: xcb.c:1335-1343 — xcb_connect(NULL, &screen) and the "Cannot open
/// display" error path (the caller exits).
///
/// Additionally registers the values C keeps in globals and libi3 reads:
/// * root_screen (C: init_xcb_early() sets it; libi3/dpi.c:17 uses it for the
///   Xft.dpi fallback — without it i3bar would measure fonts at dpi 0),
/// * conn / root window / root visual for the Pango height measurement
///   (C: libi3/font.c:55 cairo_xcb_surface_create(conn, root_screen->root,
///   root_visual_type, 1, 1)).
///
/// Returns the connection and the screen index it was opened on.
pub fn connect() -> Option<(Rc<XCBConnection>, usize)> {
    let (conn, screen_idx) = XCBConnection::connect(None).ok()?;
    let conn = Rc::new(conn);
    let screen = conn.setup().roots.get(screen_idx)?;
    ROOT.with(|r| r.set(screen.root));
    DEPTH.with(|d| d.set(screen.root_depth as u8));
    SCREEN.with(|s| s.set(screen_idx));
    CONNECTION.with(|c| *c.borrow_mut() = Some(Rc::clone(&conn)));

    // C: dpi.c:17 init_dpi_fallback() reads root_screen->height_in_pixels and
    // ->height_in_millimeters. i3bar calls init_dpi() after this.
    libi3::dpi::set_root_screen(Some(libi3::dpi::RootScreen {
        root: screen.root,
        width_in_pixels: screen.width_in_pixels,
        height_in_pixels: screen.height_in_pixels,
        height_in_millimeters: screen.height_in_millimeters,
    }));

    // C: libi3/font.c:22 caches root_visual_type = get_visualtype(root_screen)
    // and creates the measuring surface from the X connection. The visualtype
    // is built from the setup reply and leaked: C points into the setup reply,
    // which also lives for the whole process.
    if let Some(visual) = root_visualtype(&screen) {
        let visual: *mut libi3::cairo_ffi::xcb_visualtype_t = Box::into_raw(Box::new(visual));
        libi3::font::set_pango_root_surface(Some(libi3::font::PangoRootSurface {
            conn: conn.get_raw_xcb_connection().cast(),
            root: screen.root,
            visual,
        }));
    }
    Some((conn, screen_idx))
}

/// C: libi3/get_visualtype.c:16 xcb_visualtype_t *get_visualtype(xcb_screen_t
/// *screen) — the visualtype of the screen root visual.
fn root_visualtype(
    screen: &x11rb::protocol::xproto::Screen,
) -> Option<libi3::cairo_ffi::xcb_visualtype_t> {
    for depth in &screen.allowed_depths {
        for visual in &depth.visuals {
            if visual.visual_id == screen.root_visual {
                return Some(libi3::cairo_ffi::xcb_visualtype_t {
                    visual_id: visual.visual_id,
                    class: u8::from(visual.class),
                    bits_per_rgb_value: visual.bits_per_rgb_value,
                    colormap_entries: visual.colormap_entries,
                    red_mask: visual.red_mask,
                    green_mask: visual.green_mask,
                    blue_mask: visual.blue_mask,
                    pad0: [0; 4],
                });
            }
        }
    }
    None
}

/// C: include/libi3.h color_t — the draw_util copy of the color value.
pub fn to_libi3_color(color: color_t) -> libi3_color_t {
    libi3_color_t {
        red: color.red,
        green: color.green,
        blue: color.blue,
        alpha: color.alpha,
        colorpixel: color.colorpixel,
    }
}

/// The key of a drawing surface: its output index and the surface kind.
fn surface_key(output: usize, surface: surface_id) -> (usize, u8) {
    let kind = match surface {
        surface_id::BAR => 0,
        surface_id::BUFFER => 1,
        surface_id::STATUSLINE_BUFFER => 2,
    };
    (output, kind)
}

/// The drawable a surface draws into and its size.
///
/// C: reconfig_windows() calls draw_util_surface_init(xcb_connection,
/// &walk->bar, walk->bar.id, NULL, walk->rect.w, bar_height) for the window and
/// the same with the pixmap ids for the two off-screen buffers. The port keeps
/// the ids/sizes in the i3_output, so this is the same selection.
pub fn surface_geometry(output: usize, surface: surface_id) -> Option<(u32, i32, i32)> {
    outputs(|outputs| {
        let output = outputs.get(output)?;
        let (id, width, height) = match surface {
            surface_id::BAR => (output.bar.id, output.bar.width, output.bar.height),
            surface_id::BUFFER => (output.buffer.id, output.buffer.width, output.buffer.height),
            surface_id::STATUSLINE_BUFFER => {
                (output.statusline_buffer.id, output.statusline_buffer.width, output.statusline_buffer.height)
            }
        };
        if id == XCB_NONE || width <= 0 || height <= 0 {
            return None;
        }
        Some((id, width, height))
    })
}

/// The surface and the cairo coordinates of a filled rectangle.
pub fn cmd_rect(cmd: &DrawCmd) -> Option<(surface_id, usize, f64, f64, f64, f64)> {
    match cmd {
        DrawCmd::Rectangle { surface, output, x, y, w, h, .. } => {
            Some((*surface, *output, *x as f64, *y as f64, *w as f64, *h as f64))
        }
        _ => None,
    }
}

/// The source/destination surfaces and coordinates of a surface copy.
///
/// C: draw_util_copy_surface(&walk->statusline_buffer, &walk->buffer,
/// src_x, src_y, dest_x, dest_y, width, height).
#[allow(clippy::type_complexity)]
pub fn copy_geometry(
    cmd: &DrawCmd,
) -> Option<(surface_id, usize, i32, i32, surface_id, i32, i32, i32, i32)> {
    match cmd {
        DrawCmd::CopySurface { src, dst, output, src_x, src_y, dst_x, dst_y, w, h } => {
            Some((*src, *output, *src_x, *src_y, *dst, *dst_x, *dst_y, *w, *h))
        }
        _ => None,
    }
}

/// The text and the cairo coordinates of a draw_util_text call.
#[allow(clippy::type_complexity)]
pub fn text_geometry(cmd: &DrawCmd) -> Option<(surface_id, usize, String, bool, i32, i32, i32)> {
    match cmd {
        DrawCmd::Text { surface, output, text, pango_markup, x, y, max_width, .. } => {
            Some((*surface, *output, text.clone(), *pango_markup, *x, *y, *max_width))
        }
        _ => None,
    }
}

/// The cairo surface of one output, plus the pixmap behind it (the bar surface
/// draws straight into the bar window, the two buffers are pixmaps).
struct LiveSurface {
    /// C: surface_t bar / buffer / statusline_buffer
    surface: surface_t,
    /// C: the xcb_drawable_t the surface was initialized with.
    drawable: u32,
    width: i32,
    height: i32,
    depth: u8,
    /// The pixmap id, when the drawable is an off-screen buffer.
    pixmap: Option<u32>,
}

impl LiveSurface {
    /// C: in the X core font path draw_text() calls xcb_image_text_16() on the
    /// drawable, while the port's cairo surface is an image surface
    /// (draw_util.rs). Pull the pixels the X requests wrote back into the
    /// image so that the following draw_util_copy_surface() (which reads the
    /// image) sees the text.
    fn readback(&mut self, conn: &XCBConnection) {
        if self.surface.surface.is_null() || self.surface.id == XCB_NONE {
            return;
        }
        let width = self.surface.width.max(1);
        let height = self.surface.height.max(1);
        let reply = match conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.surface.id,
                0,
                0,
                width as u16,
                height as u16,
                !0,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        {
            Some(reply) => reply,
            None => return,
        };

        let row_bytes = width as usize * 4;
        if reply.data.len() < row_bytes * height as usize {
            return;
        }

        unsafe {
            libi3::cairo_ffi::cairo_surface_flush(self.surface.surface);
            let data = libi3::cairo_ffi::cairo_image_surface_get_data(self.surface.surface);
            if data.is_null() {
                return;
            }
            let stride =
                libi3::cairo_ffi::cairo_image_surface_get_stride(self.surface.surface).max(0) as usize;
            if stride < row_bytes {
                return;
            }
            for row in 0..height as usize {
                std::ptr::copy_nonoverlapping(
                    reply.data.as_ptr().add(row * row_bytes),
                    data.add(row * stride),
                    row_bytes,
                );
            }
            libi3::cairo_ffi::cairo_surface_mark_dirty(self.surface.surface);
        }
    }
}

/// The cairo/pango drawing backend of i3bar (C: the draw_util_* calls).
pub struct LiveDraw {
    conn: Rc<XCBConnection>,
    /// C: xcb.c:67 depth
    depth: u8,
    /// C: the surfaces owned by every i3_output.
    surfaces: HashMap<(usize, u8), LiveSurface>,
}

impl LiveDraw {
    /// C: i3bar's surfaces exist from the moment reconfig_windows() created
    /// them; the port creates them lazily on the first draw command.
    pub fn new(conn: Rc<XCBConnection>) -> LiveDraw {
        LiveDraw { conn, depth: depth(), surfaces: HashMap::new() }
    }

    /// Make sure the surface of the given output exists and matches the size
    /// the output currently has. A resized/recreated output drops the old
    /// pixmap first (C: xcb_free_pixmap + draw_util_surface_init).
    fn ensure(&mut self, output: usize, surface: surface_id) -> Option<&mut LiveSurface> {
        let (drawable, width, height) = surface_geometry(output, surface)?;
        let key = surface_key(output, surface);

        let stale = match self.surfaces.get(&key) {
            Some(current) => {
                current.drawable != drawable
                    || current.width != width
                    || current.height != height
                    || current.depth != self.depth
            }
            None => true,
        };

        if stale {
            if let Some(mut old) = self.surfaces.remove(&key) {
                draw_util::draw_util_surface_free(&*self.conn, &mut old.surface);
                if let Some(pixmap) = old.pixmap {
                    let _ = self.conn.free_pixmap(pixmap);
                }
            }

            let pixmap = if surface == surface_id::BAR {
                // C: surface_init(conn, &walk->bar, bar_id, ...) — the window.
                None
            } else {
                // C: xcb_create_pixmap(conn, depth, buffer_id, bar_id,
                //     walk->rect.w, bar_height)
                let parent = outputs(|outputs| {
                    outputs
                        .get(output)
                        .map(|o| o.bar.id)
                        .filter(|id| *id != XCB_NONE)
                        .unwrap_or_else(root)
                });
                let width = width.max(1) as u16;
                let height = height.max(1) as u16;
                let _ = self.conn.create_pixmap(self.depth, drawable, parent, width, height);
                Some(drawable)
            };

            let mut surface_value = surface_t::none();
            draw_util::draw_util_surface_init(
                &*self.conn,
                &mut surface_value,
                drawable,
                self.depth,
                width,
                height,
            );

            self.surfaces.insert(
                key,
                LiveSurface { surface: surface_value, drawable, width, height, depth: self.depth, pixmap },
            );
        }

        self.surfaces.get_mut(&key)
    }

    fn clear_surface(&mut self, output: usize, surface: surface_id, color: color_t) {
        let conn = Rc::clone(&self.conn);
        if let Some(target) = self.ensure(output, surface) {
            draw_util::draw_util_clear_surface(&*conn, &mut target.surface, to_libi3_color(color));
        }
    }

    fn rectangle(&mut self, output: usize, surface: surface_id, color: color_t, x: f64, y: f64, w: f64, h: f64) {
        let conn = Rc::clone(&self.conn);
        if let Some(target) = self.ensure(output, surface) {
            draw_util::draw_util_rectangle(&*conn, &mut target.surface, to_libi3_color(color), x, y, w, h);
        }
    }

    fn text(
        &mut self,
        output: usize,
        surface: surface_id,
        text: &str,
        pango_markup: bool,
        fg: color_t,
        bg: color_t,
        x: i32,
        y: i32,
        max_width: i32,
    ) {
        let conn = Rc::clone(&self.conn);
        if let Some(target) = self.ensure(output, surface) {
            // C: the i3String the block carries; the commands keep the text
            // itself plus whether it is pango markup.
            let mut string = if pango_markup {
                libi3::i3string_from_markup(text)
            } else {
                libi3::i3string_from_utf8(text)
            };
            draw_util::draw_util_text(
                &*conn,
                &mut string,
                &mut target.surface,
                to_libi3_color(fg),
                to_libi3_color(bg),
                x,
                y,
                max_width,
            );
            if !libi3::font::font_is_pango() {
                target.readback(&conn);
            }
        }
    }

    fn copy_surface(
        &mut self,
        output: usize,
        src: surface_id,
        dst: surface_id,
        src_x: i32,
        src_y: i32,
        dst_x: i32,
        dst_y: i32,
        w: i32,
        h: i32,
    ) {
        let conn = Rc::clone(&self.conn);
        let src_key = surface_key(output, src);
        let dst_key = surface_key(output, dst);

        if self.ensure(output, src).is_none() || self.ensure(output, dst).is_none() {
            return;
        }

        // Drop both surfaces out of the map so that the two mutable borrows
        // the cairo copy needs do not overlap.
        let mut src_surface = self.surfaces.remove(&src_key);
        let mut dst_surface = self.surfaces.remove(&dst_key);

        if let (Some(mut source), Some(mut destination)) = (src_surface.take(), dst_surface.take()) {
            draw_util::draw_util_copy_surface(
                &*conn,
                &mut source.surface,
                &mut destination.surface,
                src_x as f64,
                src_y as f64,
                dst_x as f64,
                dst_y as f64,
                w as f64,
                h as f64,
            );
            self.surfaces.insert(src_key, source);
            self.surfaces.insert(dst_key, destination);
        } else {
            if let Some(source) = src_surface {
                self.surfaces.insert(src_key, source);
            }
            if let Some(destination) = dst_surface {
                self.surfaces.insert(dst_key, destination);
            }
        }
    }
}

impl DrawBackend for LiveDraw {
    fn draw(&mut self, cmd: &DrawCmd) {
        match cmd {
            DrawCmd::ClearSurface { surface, output, color } => {
                self.clear_surface(*output, *surface, *color);
            }
            DrawCmd::Rectangle { color, .. } => {
                if let Some((surface, output, x, y, w, h)) = cmd_rect(cmd) {
                    self.rectangle(output, surface, *color, x, y, w, h);
                }
            }
            DrawCmd::Text { fg, bg, .. } => {
                if let Some((surface, output, text, pango_markup, x, y, max_width)) = text_geometry(cmd) {
                    self.text(output, surface, &text, pango_markup, *fg, *bg, x, y, max_width);
                }
            }
            DrawCmd::CopySurface { .. } => {
                if let Some((src, output, src_x, src_y, dst, dst_x, dst_y, w, h)) = copy_geometry(cmd) {
                    self.copy_surface(output, src, dst, src_x, src_y, dst_x, dst_y, w, h);
                }
            }
        }
    }
}

/// C: xcb_prep_cb — drain the pending X events and hand them to xcb.rs.
pub fn poll_events(conn: &XCBConnection) -> Vec<crate::xcb::XEvent> {
    use x11rb::protocol::Event;

    let mut events = Vec::new();
    loop {
        let event = match conn.poll_for_event() {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(_) => break,
        };

        match event {
            Event::VisibilityNotify(event) => events.push(crate::xcb::XEvent::VisibilityNotify {
                window: event.window,
                state: u8::from(event.state),
            }),
            Event::Expose(event) => events.push(crate::xcb::XEvent::Expose {
                window: event.window,
                count: event.count as u32,
            }),
            Event::ButtonPress(event) => events.push(crate::xcb::XEvent::Button {
                event: button_event(&event, true),
            }),
            Event::ButtonRelease(event) => events.push(crate::xcb::XEvent::Button {
                event: button_event(&event, false),
            }),
            Event::ClientMessage(event) => events.push(crate::xcb::XEvent::ClientMessage {
                event: crate::xcb::xcb_client_message_event_t {
                    window: event.window,
                    r#type: event.type_,
                    format: event.format,
                    data32: event.data.as_data32(),
                },
            }),
            Event::DestroyNotify(event) => {
                events.push(crate::xcb::XEvent::DestroyNotify { window: event.window })
            }
            Event::UnmapNotify(event) => {
                events.push(crate::xcb::XEvent::UnmapNotify { window: event.window })
            }
            Event::MapNotify(event) => events.push(crate::xcb::XEvent::MapNotify { window: event.window }),
            Event::PropertyNotify(event) => events.push(crate::xcb::XEvent::PropertyNotify {
                window: event.window,
                atom: event.atom,
                state: u8::from(event.state),
            }),
            Event::ConfigureRequest(event) => events.push(crate::xcb::XEvent::ConfigureRequest {
                window: event.window,
            }),
            Event::ResizeRequest(event) => {
                events.push(crate::xcb::XEvent::ResizeRequest { window: event.window })
            }
            _ => {}
        }
    }
    events
}

/// C: the xcb_button_press_event_t fields the click handler reads.
fn button_event(event: &x11rb::protocol::xproto::ButtonPressEvent, press: bool) -> crate::xcb::xcb_button_press_event_t {
    crate::xcb::xcb_button_press_event_t {
        response_type: if press { 4 } else { 5 },
        event: event.event,
        root_x: event.root_x as i32,
        root_y: event.root_y as i32,
        event_x: event.event_x as i32,
        event_y: event.event_y as i32,
        state: u32::from(event.state),
        detail: event.detail as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::rect;
    use crate::outputs::{init_outputs, i3_output};

    /// One active 800x600 output with the three surfaces a bar owns.
    fn setup_output() -> u32 {
        init_outputs();
        let mut output = i3_output {
            name: Some("eDP-1".to_string()),
            active: true,
            primary: true,
            rect: rect { x: 0, y: 0, w: 800, h: 600 },
            ..Default::default()
        };
        output.bar.id = 0x400001;
        output.bar.width = 800;
        output.bar.height = 20;
        output.buffer.id = 0x400002;
        output.buffer.width = 800;
        output.buffer.height = 20;
        output.statusline_buffer.id = 0x400003;
        output.statusline_buffer.width = 800;
        output.statusline_buffer.height = 20;
        let ptr_id = output.ptr_id;
        outputs(|outputs| {
            outputs.clear();
            outputs.push(output);
        });
        ptr_id
    }

    /// The bar draws into its window; the buffers draw into their pixmaps.
    #[test]
    fn surface_geometry_selects_window_and_pixmaps() {
        setup_output();
        assert_eq!(surface_geometry(0, surface_id::BAR), Some((0x400001, 800, 20)));
        assert_eq!(surface_geometry(0, surface_id::BUFFER), Some((0x400002, 800, 20)));
        assert_eq!(
            surface_geometry(0, surface_id::STATUSLINE_BUFFER),
            Some((0x400003, 800, 20))
        );
        // No such output (or no surface allocated yet) means "nothing to draw".
        assert_eq!(surface_geometry(1, surface_id::BAR), None);

        outputs(|outputs| outputs[0].bar.id = XCB_NONE);
        assert_eq!(surface_geometry(0, surface_id::BAR), None);
    }

    /// C: draw_util_rectangle takes doubles; the C layout maths works in ints.
    #[test]
    fn cmd_rect_keeps_the_integer_geometry() {
        let color = color_t::default();
        let cmd = DrawCmd::Rectangle {
            surface: surface_id::STATUSLINE_BUFFER,
            output: 2,
            color,
            x: -12,
            y: 1,
            w: 30,
            h: 18,
        };
        // The clipped (negative) x of the statusline has to survive unchanged.
        assert_eq!(
            cmd_rect(&cmd),
            Some((surface_id::STATUSLINE_BUFFER, 2, -12.0, 1.0, 30.0, 18.0))
        );
        assert_eq!(cmd_rect(&DrawCmd::ClearSurface { surface: surface_id::BUFFER, output: 0, color }), None);
    }

    /// The statusline copy keeps the source and destination coordinates.
    #[test]
    fn copy_geometry_maps_source_and_destination() {
        let cmd = DrawCmd::CopySurface {
            src: surface_id::STATUSLINE_BUFFER,
            dst: surface_id::BUFFER,
            output: 0,
            src_x: 0,
            src_y: 0,
            dst_x: 700,
            dst_y: 0,
            w: 100,
            h: 20,
        };
        assert_eq!(
            copy_geometry(&cmd),
            Some((surface_id::STATUSLINE_BUFFER, 0, 0, 0, surface_id::BUFFER, 700, 0, 100, 20))
        );
    }

    /// Text keeps x/y/max_width and whether it is pango markup.
    #[test]
    fn text_geometry_keeps_markup_and_clip_width() {
        let cmd = DrawCmd::Text {
            surface: surface_id::BUFFER,
            output: 1,
            text: "<b>hi</b>".to_string(),
            pango_markup: true,
            fg: color_t::default(),
            bg: color_t::default(),
            x: 5,
            y: 7,
            max_width: 40,
        };
        assert_eq!(
            text_geometry(&cmd),
            Some((surface_id::BUFFER, 1, "<b>hi</b>".to_string(), true, 5, 7, 40))
        );
    }

    /// The i3bar color_t and libi3's draw_util color_t carry the same channels.
    #[test]
    fn colors_convert_channel_by_channel() {
        let color = color_t { red: 0.1, green: 0.2, blue: 0.3, alpha: 0.4, colorpixel: 0x123456 };
        let converted = to_libi3_color(color);
        assert_eq!(converted.red, 0.1);
        assert_eq!(converted.green, 0.2);
        assert_eq!(converted.blue, 0.3);
        assert_eq!(converted.alpha, 0.4);
        assert_eq!(converted.colorpixel, 0x123456);
    }
}
