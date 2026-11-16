#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! 1:1 port of i3bar/src/xcb.c (2252 lines) — everything that talks to X, plus
//! the bar layout.
//!
//! C: i3bar/src/xcb.c
//!
//! The port splits the C file along one seam, as required by the task:
//!
//!  * the layout/rendering decisions keep the C shape (predict_block_length,
//!    switch_block_to_short, adjust_statusline_length, draw_statusline,
//!    draw_button, draw_bars, ...) and emit [DrawCmd] values through the
//!    [DrawBackend] trait. [RecordingBackend] records them, which is what the
//!    unit tests assert on;
//!  * every X request goes through i3-core's XConn trait (the C file uses
//!    xcb_connection_t directly), so the X-facing code is unit-testable with
//!    i3_core::xcb_util::RecordingConn.
//!
//! Not yet ported (see the TODO section in the final report): cairo/pango
//! drawing (libi3 draw_util/font), xcursor, the colormap for RGBA visuals,
//! fake_configure_notify.

use std::cell::{Cell, RefCell};

use i3_core::backend::{ConfigWindow, PropMode, StackMode, XConn};

use libi3::string::{i3string_as_utf8, i3string_get_num_glyphs, i3String};
use libi3::get_colorpixel;

use crate::common::{blockalign_t, rect, status_block, statusline};
use crate::config::{self, bar_display_mode_t, hidden_state_t, position_t, xcb_color_strings_t};
use crate::mode::mode;
use crate::outputs::i3_output;
use crate::util::{MAX, MIN};
use crate::{DLOG, ELOG};

/// The X11 connection abstraction (i3-core's XConn), named so that a call site
/// reads like the C one.
pub trait XConnLike: XConn {}
impl<T: XConn + ?Sized> XConnLike for T {}

/// C: xcb.h:20-27 tray protocol constants
pub const _NET_SYSTEM_TRAY_ORIENTATION_HORZ: u32 = 0;
pub const _NET_SYSTEM_TRAY_ORIENTATION_VERT: u32 = 1;
pub const SYSTEM_TRAY_REQUEST_DOCK: u32 = 0;
pub const SYSTEM_TRAY_BEGIN_MESSAGE: u32 = 1;
pub const SYSTEM_TRAY_CANCEL_MESSAGE: u32 = 2;
pub const XEMBED_MAPPED: u32 = 1 << 0;
pub const XEMBED_EMBEDDED_NOTIFY: u32 = 0;

/// C: xcb.h XCB_CURRENT_TIME
pub const XCB_CURRENT_TIME: u32 = 0;
/// C: XCB_NONE — no window / no atom.
pub const XCB_NONE: u32 = 0;
/// C: XCB_ATOM_* predefined atoms used by i3bar.
pub const XCB_ATOM_STRING: u32 = 31;
pub const XCB_ATOM_WM_CLASS: u32 = 67;
pub const XCB_ATOM_WM_NAME: u32 = 39;
pub const XCB_ATOM_CARDINAL: u32 = 6;
pub const XCB_ATOM_VISUALID: u32 = 32;
pub const XCB_ATOM_ATOM: u32 = 4;
pub const XCB_GET_PROPERTY_TYPE_ANY: u32 = 0;
/// C: XCB_EVENT_MASK_NO_EVENT
pub const XCB_EVENT_MASK_NO_EVENT: u32 = 0;
/// C: XCB_EVENT_MASK_STRUCTURE_NOTIFY = 131072 (1 << 17); 1 << 19 is
/// SUBSTRUCTURE_NOTIFY.
pub const XCB_EVENT_MASK_STRUCTURE_NOTIFY: u32 = 1 << 17;
pub const XCB_EVENT_MASK_PROPERTY_CHANGE: u32 = 1 << 18;
pub const XCB_EVENT_MASK_RESIZE_REDIRECT: u32 = 1 << 17;
/// C: XCB_VISIBILITY_FULLY_OBSCURED
pub const XCB_VISIBILITY_FULLY_OBSCURED: u8 = 2;
/// C: XCB_BUTTON_SCROLL_UP/LEFT/DOWN/RIGHT
pub const XCB_BUTTON_SCROLL_UP: u32 = 4;
pub const XCB_BUTTON_SCROLL_DOWN: u32 = 5;
pub const XCB_BUTTON_SCROLL_LEFT: u32 = 6;
pub const XCB_BUTTON_SCROLL_RIGHT: u32 = 7;
/// C: XCB_XKB_STATE_NOTIFY
pub const XCB_XKB_STATE_NOTIFY: u8 = 2;

// C: xcb.c:26-30 — the atoms, indexed by this enum (xcb_atoms.def).
pub const ATOM_NAMES: [&str; 12] = [
    "_NET_WM_WINDOW_TYPE",
    "_NET_WM_WINDOW_TYPE_DOCK",
    "_NET_WM_STRUT_PARTIAL",
    "I3_SOCKET_PATH",
    "MANAGER",
    "_NET_SYSTEM_TRAY_ORIENTATION",
    "_NET_SYSTEM_TRAY_VISUAL",
    "_NET_SYSTEM_TRAY_OPCODE",
    "_NET_SYSTEM_TRAY_COLORS",
    "_XEMBED_INFO",
    "_XEMBED",
    "I3_SYNC",
];

/// C: enum { ATOM_DO(name) name, ... NUM_ATOMS } — index into the atom table.
pub mod atom {
    pub const _NET_WM_WINDOW_TYPE: usize = 0;
    pub const _NET_WM_WINDOW_TYPE_DOCK: usize = 1;
    pub const _NET_WM_STRUT_PARTIAL: usize = 2;
    pub const I3_SOCKET_PATH: usize = 3;
    pub const MANAGER: usize = 4;
    pub const _NET_SYSTEM_TRAY_ORIENTATION: usize = 5;
    pub const _NET_SYSTEM_TRAY_VISUAL: usize = 6;
    pub const _NET_SYSTEM_TRAY_OPCODE: usize = 7;
    pub const _NET_SYSTEM_TRAY_COLORS: usize = 8;
    pub const _XEMBED_INFO: usize = 9;
    pub const _XEMBED: usize = 10;
    pub const I3_SYNC: usize = 11;
    pub const NUM_ATOMS: usize = 12;
}

thread_local! {
    /// C: xcb.c:32 xcb_atom_t atoms[NUM_ATOMS];
    static ATOMS: RefCell<[u32; atom::NUM_ATOMS]> = const { RefCell::new([0; atom::NUM_ATOMS]) };
    /// C: xcb.c:38 static xcb_window_t selwin = XCB_NONE;
    static SELWIN: Cell<u32> = const { Cell::new(XCB_NONE) };
    /// C: xcb.c:39 static xcb_intern_atom_reply_t *tray_reply = NULL; (the atom)
    static TRAY_REPLY: Cell<u32> = const { Cell::new(XCB_NONE) };
    /// C: xcb.c:59 static i3Font font — only the height is ported so far.
    static FONT_HEIGHT: Cell<i32> = const { Cell::new(0) };
    /// C: xcb.c:62 int icon_size;
    static ICON_SIZE: Cell<i32> = const { Cell::new(0) };
    /// C: xcb.c:69 int bar_height;
    static BAR_HEIGHT: Cell<i32> = const { Cell::new(0) };
    /// C: xcb.c:66 xcb_visualtype_t *visual_type (its visual_id)
    static VISUAL_ID: Cell<u32> = const { Cell::new(0) };
    /// C: xcb.c:67 uint8_t depth;
    static DEPTH: Cell<u16> = const { Cell::new(0) };
    /// C: xcb.c:68 xcb_colormap_t colormap;
    static COLORMAP: Cell<u32> = const { Cell::new(0) };
    /// C: xcb.c:72 int xkb_base;
    static XKB_BASE: Cell<i32> = const { Cell::new(-1) };
    /// C: xcb.c:73 bool mod_pressed = 0;
    static MOD_PRESSED: Cell<bool> = const { Cell::new(false) };
    /// C: xcb.c:88 static mode binding;
    static BINDING: RefCell<mode> = RefCell::new(mode { name: None, name_width: 0 });
    /// C: xcb.c:91 bool activated_mode = false;
    static ACTIVATED_MODE: Cell<bool> = const { Cell::new(false) };
    /// C: xcb.c:94 static i3_output *output_for_tray;
    static OUTPUT_FOR_TRAY: Cell<Option<u32>> = const { Cell::new(None) };
    /// C: xcb.c:151 struct xcb_colors_t colors;
    static COLORS: RefCell<xcb_colors_t> = RefCell::new(xcb_colors_t::default());
    /// C: xcb.c:132 int separator_symbol_width;
    static SEPARATOR_SYMBOL_WIDTH: Cell<i32> = const { Cell::new(0) };
}

/// C: xcb.c:96-125 struct xcb_colors_t — the parsed colors.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct xcb_colors_t {
    pub bar_fg: color_t,
    pub bar_bg: color_t,
    pub sep_fg: color_t,
    pub focus_bar_fg: color_t,
    pub focus_bar_bg: color_t,
    pub focus_sep_fg: color_t,
    pub active_ws_fg: color_t,
    pub active_ws_bg: color_t,
    pub active_ws_border: color_t,
    pub inactive_ws_fg: color_t,
    pub inactive_ws_bg: color_t,
    pub inactive_ws_border: color_t,
    pub urgent_ws_bg: color_t,
    pub urgent_ws_fg: color_t,
    pub urgent_ws_border: color_t,
    pub focus_ws_bg: color_t,
    pub focus_ws_fg: color_t,
    pub focus_ws_border: color_t,
    pub binding_mode_bg: color_t,
    pub binding_mode_fg: color_t,
    pub binding_mode_border: color_t,
}

/// C: include/libi3.h color_t. Defined here (instead of reusing
/// libi3::draw_util::color_t) because the libi3 copy carries no PartialEq,
/// which the recording tests need.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct color_t {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
    pub colorpixel: u32,
}

/// C: xcb.c:139 static const int ws_hoff_px = 4;
pub const ws_hoff_px: i32 = 4;
/// C: xcb.c:142 static const int ws_voff_px = 3;
pub const ws_voff_px: i32 = 3;
/// C: xcb.c:145 static const int ws_spacing_px = 1;
pub const ws_spacing_px: i32 = 1;
/// C: xcb.c:149 static const int sb_hoff_px = 4;
pub const sb_hoff_px: i32 = 4;
/// C: xcb.c:153 static const int tray_loff_px = 2;
pub const tray_loff_px: i32 = 2;
/// C: xcb.c:157 static const int sep_voff_px = 4;
pub const sep_voff_px: i32 = 4;

/// C: libi3/dpi.c:85 int logical_px(const int logical)
///
/// TODO(X11 port): libi3::dpi::logical_px is unimplemented (it needs the
/// Xft.dpi resource), so this returns the unscaled value (dpi 96).
pub fn logical_px(logical: i32) -> i32 {
    logical
}

/// C: libi3/font.c:466 int predict_text_width(i3String *text)
///
/// With a live X connection this is libi3's measurement against the loaded
/// font (C: the savedFont). Without a connection (unit tests, -C validation)
/// every glyph counts as MONOSPACE_ADVANCE px, which keeps the layout maths
/// deterministic in exactly the C arithmetic.
pub const MONOSPACE_ADVANCE: i32 = 7;

/// C: main.c / xcb.c:1446 — the font i3bar falls back to when the bar
/// configuration has no font of its own.
pub const DEFAULT_FONT: &str = "-misc-fixed-medium-r-normal--13-120-75-75-C-70-iso10646-1";

pub fn predict_text_width(text: &mut i3String) -> i32 {
    if let Some(conn) = crate::live::connection() {
        return libi3::font::predict_text_width(Some(&*conn), text);
    }
    i3string_get_num_glyphs(text) as i32 * MONOSPACE_ADVANCE
}

/// The height of a font that cannot be loaded (no connection): the default X
/// core font i3bar falls back to is 13px high; pango patterns are assumed to
/// be 16px high.
fn placeholder_font_height(fontname: Option<&str>) -> i32 {
    match fontname {
        Some(name) if name.starts_with("-misc-fixed") => 13,
        Some(_) => 16,
        None => 13,
    }
}

/// C: xcb.c:132 extern int separator_symbol_width
pub fn separator_symbol_width() -> i32 {
    SEPARATOR_SYMBOL_WIDTH.with(|w| w.get())
}

/// C: xcb.c:69 int bar_height
pub fn bar_height() -> i32 {
    BAR_HEIGHT.with(|h| h.get())
}

/// C: xcb.c:62 int icon_size
pub fn icon_size() -> i32 {
    ICON_SIZE.with(|s| s.get())
}

/// C: xcb.c:99 struct xcb_colors_t colors (read access)
pub fn colors<R>(f: impl FnOnce(&xcb_colors_t) -> R) -> R {
    COLORS.with(|c| f(&c.borrow()))
}

fn colors_mut<R>(f: impl FnOnce(&mut xcb_colors_t) -> R) -> R {
    COLORS.with(|c| f(&mut c.borrow_mut()))
}

// ---------------------------------------------------------------------------
// drawing commands / backends
// ---------------------------------------------------------------------------

/// C: the three surfaces every output owns (bar, off-screen buffer,
/// statusline buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum surface_id {
    BAR,
    BUFFER,
    STATUSLINE_BUFFER,
}

/// One draw_util_* call of the C code, with the resolved arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum DrawCmd {
    /// C: draw_util_clear_surface()
    ClearSurface { surface: surface_id, output: usize, color: color_t },
    /// C: draw_util_rectangle()
    Rectangle { surface: surface_id, output: usize, color: color_t, x: i32, y: i32, w: i32, h: i32 },
    /// C: draw_util_text()
    Text {
        surface: surface_id,
        output: usize,
        text: String,
        pango_markup: bool,
        fg: color_t,
        bg: color_t,
        x: i32,
        y: i32,
        max_width: i32,
    },
    /// C: draw_util_copy_surface()
    CopySurface {
        src: surface_id,
        dst: surface_id,
        output: usize,
        src_x: i32,
        src_y: i32,
        dst_x: i32,
        dst_y: i32,
        w: i32,
        h: i32,
    },
}

/// The sink for [DrawCmd]s. The real cairo/pango binding plugs in here once it
/// exists; [RecordingBackend] is the test double.
pub trait DrawBackend {
    fn draw(&mut self, cmd: &DrawCmd);
}

/// Records every command in order — the "recording mock" the tests assert on.
#[derive(Debug, Default)]
pub struct RecordingBackend {
    pub cmds: Vec<DrawCmd>,
}

impl RecordingBackend {
    pub fn new() -> RecordingBackend {
        RecordingBackend::default()
    }

    pub fn texts(&self) -> Vec<String> {
        self.cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}

impl DrawBackend for RecordingBackend {
    fn draw(&mut self, cmd: &DrawCmd) {
        self.cmds.push(cmd.clone());
    }
}

/// C: libi3/draw_util.c:163 color_t draw_util_hex_to_color(const char *color)
///
/// TODO(X11 port): libi3::draw_util::draw_util_hex_to_color is unimplemented;
/// this is its (cairo-free) behaviour.
pub fn draw_util_hex_to_color(color: &str) -> color_t {
    if color.len() < 7 {
        return color_t::default();
    }
    let get = |r: std::ops::Range<usize>| -> f64 {
        u8::from_str_radix(&color[r], 16).unwrap_or(0) as f64 / 255.0
    };
    color_t {
        red: get(1..3),
        green: get(3..5),
        blue: get(5..7),
        alpha: if color.len() >= 9 { get(7..9) } else { 1.0 },
        colorpixel: get_colorpixel(color),
    }
}

// ---------------------------------------------------------------------------
// layout maths (the pure part of the C file)
// ---------------------------------------------------------------------------

/// C: xcb.c:163 static uint32_t get_sep_offset(struct status_block *block)
pub fn get_sep_offset(block: &status_block) -> u32 {
    if !block.no_separator && block.sep_block_width > 0 {
        return block.sep_block_width / 2 + block.sep_block_width % 2;
    }
    0
}

/// C: xcb.c:171 trayclient
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct trayclient {
    /// C: xcb_window_t win
    pub win: u32,
    /// C: bool mapped
    pub mapped: bool,
    /// C: int xe_version
    pub xe_version: i32,
    /// C: char *class_class
    pub class_class: Option<String>,
    /// C: char *class_instance
    pub class_instance: Option<String>,
}

/// C: xcb.c:172 static int get_tray_width(struct tc_head *trayclients)
pub fn get_tray_width(trayclients: &[trayclient]) -> i32 {
    let mut tray_width = 0;
    // C: TAILQ_FOREACH_REVERSE
    for trayclient in trayclients.iter().rev() {
        if !trayclient.mapped {
            continue;
        }
        tray_width += icon_size() + logical_px(config::config(|c| c.tray_padding));
    }
    if tray_width > 0 {
        tray_width += logical_px(tray_loff_px);
    }
    tray_width
}

/// C: xcb.c:262 static void draw_separator(i3_output *output, uint32_t x,
/// struct status_block *block, bool use_focus_colors)
fn draw_separator(index: usize, x: u32, block: &status_block, use_focus_colors: bool, cmds: &mut Vec<DrawCmd>) {
    let (sep_fg, bar_bg) = colors(|c| {
        if use_focus_colors {
            (c.focus_sep_fg, c.focus_bar_bg)
        } else {
            (c.sep_fg, c.bar_bg)
        }
    });

    let sep_offset = get_sep_offset(block);
    if sep_offset == 0 {
        return;
    }

    let center_x = x.wrapping_sub(sep_offset);
    let separator = config::config(|c| c.separator_symbol.is_some());
    let bar_h = bar_height();

    if !separator {
        // Draw a classic one pixel, vertical separator.
        cmds.push(DrawCmd::Rectangle {
            surface: surface_id::STATUSLINE_BUFFER,
            output: index,
            color: sep_fg,
            x: center_x as i32,
            y: logical_px(sep_voff_px),
            w: logical_px(1),
            h: bar_h - 2 * logical_px(sep_voff_px),
        });
    } else {
        // Draw a custom separator.
        let separator_x = MAX(
            x.wrapping_sub(block.sep_block_width),
            center_x.wrapping_sub((separator_symbol_width() / 2) as u32),
        );
        let mut text = config::config_mut(|c| c.separator_symbol.take());
        if let Some(symbol) = text.as_mut() {
            let utf8 = i3string_as_utf8(symbol).unwrap_or("").to_string();
            let markup = libi3::i3string_is_markup(symbol);
            cmds.push(DrawCmd::Text {
                surface: surface_id::STATUSLINE_BUFFER,
                output: index,
                text: utf8,
                pango_markup: markup,
                fg: sep_fg,
                bg: bar_bg,
                x: separator_x as i32,
                y: bar_h / 2 - font_height() / 2,
                max_width: x.wrapping_sub(separator_x) as i32,
            });
        }
        // Put it back (the C code never takes it out of the config).
        config::config_mut(|c| c.separator_symbol = text);
    }
}

pub fn font_height() -> i32 {
    FONT_HEIGHT.with(|h| h.get())
}

/// C: xcb.c:284 static void predict_block_length(struct status_block *block)
pub fn predict_block_length(block: &mut status_block) {
    let use_short = block.use_short && block.short_text.is_some();

    let num_bytes = if use_short {
        match block.short_text.as_mut() {
            Some(t) => libi3::i3string_get_num_bytes(t),
            None => 0,
        }
    } else {
        block.full_text_num_bytes()
    };

    if num_bytes == 0 {
        block.render_length = 0;
        return;
    }

    let mut render = if use_short { block.short_render } else { block.full_render };

    render.width = {
        let text = if use_short { block.short_text.as_mut() } else { block.full_text.as_mut() };
        match text {
            Some(t) => predict_text_width(t) as u32,
            None => 0,
        }
    };

    if block.border.is_some() {
        render.width += logical_px((block.border_left + block.border_right) as i32) as u32;
    }

    // Compute offset and append for text alignment in min_width.
    if block.min_width <= render.width {
        render.x_offset = 0;
        render.x_append = 0;
    } else {
        let padding_width = block.min_width - render.width;
        match block.align {
            blockalign_t::ALIGN_LEFT => render.x_append = padding_width,
            blockalign_t::ALIGN_RIGHT => render.x_offset = padding_width,
            blockalign_t::ALIGN_CENTER => {
                render.x_offset = padding_width / 2;
                render.x_append = padding_width / 2 + padding_width % 2;
            }
        }
    }

    if use_short {
        block.short_render = render;
    } else {
        block.full_render = render;
    }

    block.render_length = render.width + render.x_offset + render.x_append;
}

/// C: xcb.c:329 static uint32_t predict_statusline_length(void)
pub fn predict_statusline_length(head: &mut [status_block]) -> u32 {
    let mut width = 0u32;
    let len = head.len();

    for idx in 0..len {
        predict_block_length(&mut head[idx]);
        let block_width = head[idx].render_length;
        if block_width == 0 {
            continue;
        }

        width += block_width;

        // If this is not the last block, add some pixels for a separator.
        if idx + 1 != len {
            width += head[idx].sep_block_width;
        }
    }

    width
}

/// C: xcb.c:349 static uint32_t switch_block_to_short(struct status_block *block)
pub fn switch_block_to_short(block: &mut status_block) -> u32 {
    // Skip blocks that have no short form or are already in short form.
    if block.short_text.is_none() || block.use_short {
        return 0;
    }
    let full = block.render_length;
    block.use_short = true;
    predict_block_length(block);
    full.saturating_sub(block.render_length)
}

/// C: xcb.c:359 static uint32_t adjust_statusline_length(uint32_t max_length)
pub fn adjust_statusline_length(head: &mut [status_block], max_length: u32) -> u32 {
    let mut width = predict_statusline_length(head);
    let len = head.len();

    // Progressively switch the blocks to short mode.
    for idx in 0..len {
        if width < max_length {
            break;
        }
        width = width.saturating_sub(switch_block_to_short(&mut head[idx]));

        // Provide support for representing a single logical block using multiple
        // JSON blocks: if one block is shortened, ensure that all other blocks
        // with the same name are also shortened such that the entire logical
        // block uses the short form text.
        let name = head[idx].name.clone();
        if let Some(name) = name {
            for other in 0..len {
                let same = head[other].name.as_deref() == Some(name.as_str());
                if same {
                    width = width.saturating_sub(switch_block_to_short(&mut head[other]));
                }
            }
        }
    }

    width
}

/// C: xcb.c:397 static void draw_statusline(i3_output *output, uint32_t
/// clip_left, bool use_focus_colors)
fn draw_statusline(index: usize, clip_left: u32, use_focus_colors: bool, cmds: &mut Vec<DrawCmd>) {
    let bar_color = colors(|c| if use_focus_colors { c.focus_bar_bg } else { c.bar_bg });

    cmds.push(DrawCmd::ClearSurface {
        surface: surface_id::STATUSLINE_BUFFER,
        output: index,
        color: bar_color,
    });

    // Use unsigned integer wraparound to clip off the left side. Drawing at
    // such an x position is a no-op which XCB and Cairo safely ignore; once x
    // moves up by clip_left and wraps around, content is rendered again.
    let mut x = 0u32.wrapping_sub(clip_left);

    let bar_h = bar_height();

    let block_count = statusline::head(|head| head.len());
    for idx in 0..block_count {
        // Read the block's effective text/render data.
        let (text_bytes, text_utf8, markup, urgent, color, background, border, border_left,
             border_right, border_top, border_bottom, spec_offset) = statusline::head(|head| {
            let block = &mut head[idx];
            let use_short = block.use_short && block.short_text.is_some();
            let (text_bytes, text_utf8, markup) = {
                let text = if use_short { block.short_text.as_mut() } else { block.full_text.as_mut() };
                match text {
                    Some(t) => (
                        libi3::i3string_get_num_bytes(t),
                        i3string_as_utf8(t).unwrap_or("").to_string(),
                        libi3::i3string_is_markup(t),
                    ),
                    None => (0, String::new(), false),
                }
            };
            let spec_offset = if use_short { block.short_render } else { block.full_render };
            (
                text_bytes,
                text_utf8,
                markup,
                block.urgent,
                block.color.clone(),
                block.background.clone(),
                block.border.clone(),
                block.border_left,
                block.border_right,
                block.border_top,
                block.border_bottom,
                spec_offset,
            )
        });

        if text_bytes == 0 {
            continue;
        }

        let fg_color = if urgent {
            colors(|c| c.urgent_ws_fg)
        } else if let Some(color) = color.as_deref() {
            draw_util_hex_to_color(color)
        } else if use_focus_colors {
            colors(|c| c.focus_bar_fg)
        } else {
            colors(|c| c.bar_fg)
        };

        let mut bg_color = bar_color;

        let full_render_width = (spec_offset.width + spec_offset.x_offset + spec_offset.x_append) as i32;
        let has_border = border.is_some();
        let block_urgent = urgent;
        let block_border = border.clone();
        let block_background = background.clone();

        if border.is_some() || background.is_some() || urgent {
            // Let's determine the colors first.
            let mut border_color = bar_color;
            if block_urgent {
                border_color = colors(|c| c.urgent_ws_border);
                bg_color = colors(|c| c.urgent_ws_bg);
            } else {
                if let Some(b) = block_border.as_deref() {
                    border_color = draw_util_hex_to_color(b);
                }
                if let Some(b) = block_background.as_deref() {
                    bg_color = draw_util_hex_to_color(b);
                }
            }

            // Draw the border.
            cmds.push(DrawCmd::Rectangle {
                surface: surface_id::STATUSLINE_BUFFER,
                output: index,
                color: border_color,
                x: x as i32,
                y: logical_px(1),
                w: full_render_width,
                h: bar_h - logical_px(2),
            });

            // Draw the background.
            cmds.push(DrawCmd::Rectangle {
                surface: surface_id::STATUSLINE_BUFFER,
                output: index,
                color: bg_color,
                x: (x as i32) + (has_border as i32) * logical_px(border_left as i32),
                y: logical_px(1) + (has_border as i32) * logical_px(border_top as i32),
                w: full_render_width - (has_border as i32) * logical_px((border_right + border_left) as i32),
                h: bar_h
                    - (has_border as i32) * logical_px((border_bottom + border_top) as i32)
                    - logical_px(2),
            });
        }

        cmds.push(DrawCmd::Text {
            surface: surface_id::STATUSLINE_BUFFER,
            output: index,
            text: text_utf8,
            pango_markup: markup,
            fg: fg_color,
            bg: bg_color,
            x: (x as i32) + spec_offset.x_offset as i32 + (has_border as i32) * logical_px(border_left as i32),
            y: bar_h / 2 - font_height() / 2,
            max_width: spec_offset.width as i32
                - (has_border as i32) * logical_px((border_left + border_right) as i32),
        });

        x = x.wrapping_add(full_render_width as u32);

        // If this is not the last block, draw a separator.
        if idx + 1 != block_count {
            let sep_block_width = statusline::head(|head| head[idx].sep_block_width);
            x = x.wrapping_add(sep_block_width);
            let block = statusline::head(|head| head[idx].deep_copy());
            draw_separator(index, x, &block, use_focus_colors, cmds);
        }
    }
}

/// C: xcb.c:521 static void hide_bars(void)
pub fn hide_bars(x: &mut dyn XConnLike) {
    let (mode, hidden_state) = config::config(|c| (c.hide_on_modifier, c.hidden_state));
    if mode == bar_display_mode_t::M_DOCK
        || (hidden_state == hidden_state_t::S_SHOW && mode == bar_display_mode_t::M_HIDE)
    {
        return;
    }

    let bar_windows = crate::outputs::outputs(|outputs| {
        outputs
            .iter()
            .filter(|o| o.active)
            .map(|o| o.bar.id)
            .collect::<Vec<u32>>()
    });
    for id in bar_windows {
        x.unmap_window(id);
    }
    crate::child::stop_children();
}

/// C: xcb.c:540 static void unhide_bars(void)
pub fn unhide_bars(x: &mut dyn XConnLike) {
    if config::config(|c| c.hide_on_modifier) != bar_display_mode_t::M_HIDE {
        return;
    }

    crate::child::cont_children();

    let position = config::config(|c| c.position);
    let bar_h = bar_height();
    let bars = crate::outputs::outputs(|outputs| {
        outputs
            .iter()
            .filter(|o| o.bar.id != XCB_NONE)
            .map(|o| (o.bar.id, o.rect))
            .collect::<Vec<(u32, rect)>>()
    });

    for (bar_id, out_rect) in bars {
        let values = [
            (ConfigWindow::X, out_rect.x as u32),
            (
                ConfigWindow::Y,
                (if position == position_t::POS_TOP {
                    out_rect.y
                } else {
                    out_rect.y + out_rect.h - bar_h
                }) as u32,
            ),
            (ConfigWindow::WIDTH, out_rect.w as u32),
            (ConfigWindow::HEIGHT, bar_h as u32),
            (ConfigWindow::STACK_MODE, StackMode::ABOVE as u32),
        ];
        DLOG!("Reconfiguring window for output to {},{}", values[0].1, values[1].1);
        x.configure_window(bar_id, &values);
        x.map_window(bar_id);
    }
}

// ---------------------------------------------------------------------------
// colors and custom commands
// ---------------------------------------------------------------------------

/// C: xcb.c:582 void init_colors(const struct xcb_color_strings_t *new_colors)
pub fn init_colors(new_colors: &xcb_color_strings_t, x: &mut dyn XConnLike) {
    macro_rules! parse_color {
        ($name:ident, $def:expr) => {
            colors_mut(|c| c.$name = draw_util_hex_to_color(
                new_colors.$name.as_deref().unwrap_or($def),
            ))
        };
    }
    parse_color!(bar_fg, "#FFFFFF");
    parse_color!(bar_bg, "#000000");
    parse_color!(sep_fg, "#666666");
    parse_color!(active_ws_fg, "#FFFFFF");
    parse_color!(active_ws_bg, "#333333");
    parse_color!(active_ws_border, "#333333");
    parse_color!(inactive_ws_fg, "#888888");
    parse_color!(inactive_ws_bg, "#222222");
    parse_color!(inactive_ws_border, "#333333");
    parse_color!(urgent_ws_fg, "#FFFFFF");
    parse_color!(urgent_ws_bg, "#900000");
    parse_color!(urgent_ws_border, "#2f343a");
    parse_color!(focus_ws_fg, "#FFFFFF");
    parse_color!(focus_ws_bg, "#285577");
    parse_color!(focus_ws_border, "#4c7899");

    // For the binding mode indicator colors, we don't hardcode a default.
    // Instead, we fall back to urgent_ws_* colors.
    colors_mut(|c| {
        c.binding_mode_fg = match new_colors.binding_mode_fg.as_deref() {
            Some(s) => draw_util_hex_to_color(s),
            None => c.urgent_ws_fg,
        };
        c.binding_mode_bg = match new_colors.binding_mode_bg.as_deref() {
            Some(s) => draw_util_hex_to_color(s),
            None => c.urgent_ws_bg,
        };
        c.binding_mode_border = match new_colors.binding_mode_border.as_deref() {
            Some(s) => draw_util_hex_to_color(s),
            None => c.urgent_ws_border,
        };
        // Similarly, for unspecified focused bar colors, we fall back to the
        // regular bar colors.
        c.focus_bar_fg = match new_colors.focus_bar_fg.as_deref() {
            Some(s) => draw_util_hex_to_color(s),
            None => c.bar_fg,
        };
        c.focus_bar_bg = match new_colors.focus_bar_bg.as_deref() {
            Some(s) => draw_util_hex_to_color(s),
            None => c.bar_bg,
        };
        c.focus_sep_fg = match new_colors.focus_sep_fg.as_deref() {
            Some(s) => draw_util_hex_to_color(s),
            None => c.sep_fg,
        };
    });

    init_tray_colors(x);
    x.flush();
}

/// C: xcb.c:629 static bool execute_custom_command(xcb_keycode_t input_code,
/// bool event_is_release)
pub fn execute_custom_command(input_code: u32, event_is_release: bool) -> bool {
    let command = config::config(|c| {
        c.bindings
            .iter()
            .find(|b| b.input_code as u32 == input_code && b.release == event_is_release)
            .and_then(|b| b.command.clone())
    });

    if let Some(command) = command {
        crate::ipc::i3_send_msg(crate::ipc::I3_IPC_RUN_COMMAND, Some(&command));
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// clicks
// ---------------------------------------------------------------------------

/// The subset of xcb_button_press_event_t the C code reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct xcb_button_press_event_t {
    pub response_type: u8,
    /// C: xcb_window_t event
    pub event: u32,
    /// C: int16_t root_x / root_y (the C code reads them as int)
    pub root_x: i32,
    pub root_y: i32,
    /// C: int16_t event_x / event_y
    pub event_x: i32,
    pub event_y: i32,
    /// C: uint16_t state
    pub state: u32,
    /// C: uint8_t detail
    pub detail: u32,
}

/// C: xcb.c:649 static void child_handle_button(xcb_button_press_event_t *event,
/// i3_output *output, uint32_t statusline_x, uint32_t statusline_y)
pub fn child_handle_button(
    x: &mut dyn XConnLike,
    draw: &mut dyn DrawBackend,
    event: &xcb_button_press_event_t,
    index: usize,
    statusline_x: u32,
    statusline_y: u32,
) {
    let (statusline_width, out_rect_y) = crate::outputs::outputs(|outputs| {
        (outputs[index].statusline_width, outputs[index].rect.y)
    });

    if statusline_x > statusline_width as u32 {
        return;
    }

    // x of the start of the current block relative to the statusline.
    let mut last_block_x = 0u32;
    let block_count = statusline::head(|head| head.len());

    for idx in 0..block_count {
        let (num_bytes, use_short_text, full_render_width, sep_block_width, name, instance) =
            statusline::head(|head| {
                let block = &mut head[idx];
                let use_short = block.use_short && block.short_text.is_some();
                let render = if use_short { block.short_render } else { block.full_render };
                let num_bytes = {
                    let text = if use_short { block.short_text.as_mut() } else { block.full_text.as_mut() };
                    match text {
                        Some(t) => libi3::i3string_get_num_bytes(t),
                        None => 0,
                    }
                };
                (
                    num_bytes,
                    use_short,
                    render.width + render.x_offset + render.x_append,
                    block.sep_block_width,
                    block.name.clone(),
                    block.instance.clone(),
                )
            });
        let _ = use_short_text;

        if num_bytes == 0 {
            continue;
        }

        // Include the whole block in our calculations: when min_width is
        // specified, we have to take padding width into account.
        // x of the click event relative to the current block.
        let relative_x = statusline_x.wrapping_sub(last_block_x);
        if relative_x <= full_render_width {
            let output_x = event.event_x as u32;
            let output_y = statusline_y.wrapping_add(event.event_y as u32);
            let _ = out_rect_y;

            crate::child::send_block_clicked(
                draw,
                event.detail as i32,
                name.as_deref(),
                instance.as_deref(),
                event.root_x,
                event.root_y,
                relative_x as i32,
                event.event_y,
                output_x as i32,
                output_y as i32,
                full_render_width as i32,
                bar_height(),
                event.state,
            );
            let _ = x;
            return;
        }

        last_block_x = last_block_x
            .wrapping_add(full_render_width)
            .wrapping_add(sep_block_width);
        if last_block_x > statusline_x {
            // Click was on a separator.
            return;
        }
    }
}

/// C: xcb.c:712 static int predict_button_width(int name_width)
pub fn predict_button_width(name_width: i32) -> i32 {
    MAX(
        name_width + 2 * logical_px(ws_hoff_px) + 2 * logical_px(1),
        logical_px(config::config(|c| c.ws_min_width)),
    )
}

/// C: xcb.c:723 static char *quote_workspace_name(const char *in)
pub fn quote_workspace_name(input: &str) -> String {
    // To properly handle workspace names with double quotes in them, we need to
    // escape the double quotes.
    let mut out = String::from("workspace \"");
    for c in input.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// C: xcb.c:751 static void focus_workspace(i3_ws *ws)
///
/// The C code passes the workspace struct; the Rust port passes the two fields
/// it reads (i3_ws owns an i3String and therefore is not Clone).
pub fn focus_workspace(id: u64, canonical_name: Option<&str>) {
    if id != 0 {
        // Workspace ID has higher precedence since the workspace_command is
        // allowed to change workspace names as long as it provides a valid ID.
        let buffer = format!("[con_id={}] focus workspace", id);
        crate::ipc::i3_send_msg(crate::ipc::I3_IPC_RUN_COMMAND, Some(&buffer));
        return;
    }

    let canonical_name = match canonical_name {
        Some(n) => n,
        None => return,
    };

    let buffer = quote_workspace_name(canonical_name);
    crate::ipc::i3_send_msg(crate::ipc::I3_IPC_RUN_COMMAND, Some(&buffer));
}

/// C: xcb.c:771 static void handle_button(xcb_button_press_event_t *event)
pub fn handle_button(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, event: &xcb_button_press_event_t) {
    // Determine, which bar was clicked.
    let bar = event.event;
    let clicked = crate::outputs::outputs(|outputs| outputs.iter().position(|o| o.bar.id == bar));

    let index = match clicked {
        Some(idx) => idx,
        None => {
            DLOG!("Unknown bar clicked!");
            return;
        }
    };

    DLOG!("Got button {}", event.detail);

    // During button release events, only check for custom commands.
    let event_is_release = (event.response_type & !0x80) == crate::ipc::XCB_BUTTON_RELEASE_EVENT;

    let x_pos = (if event.event_x >= 0 { event.event_x } else { 0 })
        - logical_px(config::config(|c| c.padding.x as i32));
    if x_pos < 0 {
        // Ignore clicks in padding.
        return;
    }

    let mut workspace_width = 0i32;
    let mut cur_ws: Option<usize> = None;
    let mut clicked_ws: Option<usize> = None;

    crate::outputs::outputs(|outputs| {
        let output = &outputs[index];
        for (i, ws_walk) in output.workspaces.iter().enumerate() {
            let w = predict_button_width(ws_walk.name_width);
            if x_pos >= workspace_width && x_pos <= workspace_width + w {
                clicked_ws = Some(i);
            }
            if ws_walk.visible {
                cur_ws = Some(i);
            }
            workspace_width += w;
            if i + 1 != output.workspaces.len() {
                workspace_width += logical_px(ws_spacing_px);
            }
        }
    });

    if crate::child::child_want_click_events() && x_pos > workspace_width {
        let (tray_width, rect_w) = crate::outputs::outputs(|outputs| {
            (
                get_tray_width(&outputs[index].trayclients),
                outputs[index].rect.w,
            )
        });
        // Calculate the horizontal coordinate (x) of the start of the
        // statusline by subtracting its width and the width of the tray from
        // the bar width.
        let statusline_width = crate::outputs::outputs(|outputs| outputs[index].statusline_width);
        let offset_x = rect_w
            - statusline_width
            - tray_width
            - logical_px((tray_width > 0) as i32 * sb_hoff_px);
        if x_pos >= offset_x {
            // Click was after the start of the statusline, return to avoid
            // executing any other actions even if a click event is not produced
            // eventually.
            if !event_is_release {
                // x of the click event relative to the start of the statusline.
                let output_rect_y = crate::outputs::outputs(|outputs| outputs[index].rect.y);
                let statusline_x = (x_pos - offset_x) as u32;
                let statusline_y = (event.root_y - output_rect_y - event.event_y) as u32;
                child_handle_button(x, draw, event, index, statusline_x, statusline_y);
            }
            return;
        }
    }

    // If a custom command was specified for this mouse button, it overrides the
    // default behavior.
    if execute_custom_command(event.detail, event_is_release) || event_is_release {
        return;
    }

    let cur_ws = match cur_ws {
        Some(idx) => idx,
        None => {
            DLOG!("No workspace active?");
            return;
        }
    };

    let target_ws = match event.detail {
        XCB_BUTTON_SCROLL_UP | XCB_BUTTON_SCROLL_LEFT => {
            // Mouse wheel up. We select the previous ws, if any. If there is no
            // more workspace, don't even send the workspace command, otherwise
            // (with workspace auto_back_and_forth) we'd end up on the wrong
            // workspace.
            if cur_ws == 0 {
                return;
            }
            cur_ws - 1
        }
        XCB_BUTTON_SCROLL_DOWN | XCB_BUTTON_SCROLL_RIGHT => {
            let len = crate::outputs::outputs(|outputs| outputs[index].workspaces.len());
            if cur_ws + 1 == len {
                return;
            }
            cur_ws + 1
        }
        1 => {
            // if no workspace was clicked, focus our currently visible workspace
            // if it is not already focused.
            let mut ws = clicked_ws;
            if ws.is_none() {
                ws = crate::outputs::outputs(|outputs| {
                    outputs[index]
                        .workspaces
                        .iter()
                        .position(|w| w.visible && !w.focused)
                });
            }
            // if there is nothing to focus, we are done.
            match ws {
                Some(idx) => idx,
                None => return,
            }
        }
        _ => return,
    };

    let (id, canonical_name) = crate::outputs::outputs(|outputs| {
        let ws = &outputs[index].workspaces[target_ws];
        (ws.id, ws.canonical_name.clone())
    });
    focus_workspace(id, canonical_name.as_deref());
}

/// C: xcb.c:892 static void handle_visibility_notify(xcb_visibility_notify_event_t *event)
pub fn handle_visibility_notify(window: u32, state: u8) {
    let visible = state != XCB_VISIBILITY_FULLY_OBSCURED;
    let mut num_visible = 0;

    let found = crate::outputs::outputs(|outputs| {
        let mut found = false;
        for output in outputs.iter_mut() {
            if !output.active {
                continue;
            }
            if output.bar.id == window {
                output.visible = visible;
                found = true;
            }
            if output.visible {
                num_visible += 1;
            }
        }
        found
    });
    let _ = found;

    if num_visible == 0 {
        crate::child::stop_children();
    } else {
        crate::child::cont_children();
    }
}

// ---------------------------------------------------------------------------
// tray
// ---------------------------------------------------------------------------

/// C: xcb.c:927 static int reorder_trayclients_cmp(const void *_a, const void *_b)
pub fn reorder_trayclients_cmp(a: &trayclient, b: &trayclient) -> std::cmp::Ordering {
    let result = libi3::strcasecmp_nullable(a.class_class.as_deref(), b.class_class.as_deref());
    if result != 0 {
        result.cmp(&0)
    } else {
        libi3::strcasecmp_nullable(a.class_instance.as_deref(), b.class_instance.as_deref()).cmp(&0)
    }
}

/// C: xcb.c:940 static void configure_trayclients(void)
pub fn configure_trayclients(x: &mut dyn XConnLike) {
    let tray_padding = logical_px(config::config(|c| c.tray_padding));

    let configure: Vec<(u32, u32)> = crate::outputs::outputs(|outputs| {
        let mut configure = Vec::new();
        for output in outputs.iter_mut() {
            if !output.active {
                continue;
            }

            let mut clients: Vec<trayclient> =
                output.trayclients.iter().filter(|c| c.mapped).cloned().collect();
            clients.sort_by(reorder_trayclients_cmp);

            let mut x_pos = output.rect.w as u32;
            for client in clients.iter().rev() {
                x_pos -= (icon_size() + tray_padding) as u32;
                configure.push((client.win, x_pos));
            }
        }
        configure
    });

    for (win, x_pos) in configure {
        DLOG!("Configuring tray window {:08x} to x={}", win, x_pos);
        x.configure_window(win, &[(ConfigWindow::X, x_pos)]);
    }
}

/// C: xcb.c:989 static trayclient *trayclient_and_output_from_window(xcb_window_t win, i3_output **output)
pub fn trayclient_and_output_from_window(win: u32, output_index: Option<&mut usize>) -> Option<trayclient> {
    crate::outputs::outputs(|outputs| {
        for (idx, o_walk) in outputs.iter().enumerate() {
            if !o_walk.active {
                continue;
            }
            for client in o_walk.trayclients.iter() {
                if client.win == win {
                    if let Some(out) = output_index {
                        *out = idx;
                    }
                    return Some(client.clone());
                }
            }
        }
        None
    })
}

/// C: xcb.c:1010 static trayclient *trayclient_from_window(xcb_window_t win)
pub fn trayclient_from_window(win: u32) -> Option<trayclient> {
    trayclient_and_output_from_window(win, None)
}

/// C: xcb.c:1014 static void trayclient_update_class(trayclient *client)
pub fn trayclient_update_class(x: &mut dyn XConnLike, client: &mut trayclient) {
    let prop = x.get_property(client.win, XCB_ATOM_WM_CLASS, XCB_ATOM_STRING, 0, 32);
    let prop = match prop {
        Some(p) if p.value_len() > 0 => p,
        _ => {
            DLOG!("WM_CLASS not set.");
            return;
        }
    };

    // We cannot use asprintf here since this property contains two
    // null-terminated strings (for compatibility reasons). Instead, we use
    // strdup() on both strings.
    let prop_length = prop.value_len();
    let new_class = &prop.value;
    let class_class_index = new_class
        .iter()
        .position(|&b| b == 0)
        .map(|p| p + 1)
        .unwrap_or(prop_length);

    client.class_instance = Some(
        String::from_utf8_lossy(&new_class[..class_class_index.saturating_sub(1).min(prop_length)])
            .into_owned(),
    );
    if class_class_index < prop_length {
        client.class_class = Some(
            String::from_utf8_lossy(&new_class[class_class_index..])
                .trim_end_matches('\0')
                .to_string(),
        );
    } else {
        client.class_class = None;
    }
    DLOG!(
        "WM_CLASS changed to {:?} (instance), {:?} (class)",
        client.class_instance,
        client.class_class
    );
}

/// The subset of xcb_client_message_event_t the C code reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct xcb_client_message_event_t {
    pub window: u32,
    /// C: xcb_atom_t type
    pub r#type: u32,
    pub format: u8,
    pub data32: [u32; 5],
}

/// C: xcb.c:1041 static void handle_client_message(xcb_client_message_event_t *event)
pub fn handle_client_message(
    x: &mut dyn XConnLike,
    draw: &mut dyn DrawBackend,
    event: &xcb_client_message_event_t,
) {
    let atoms = ATOMS.with(|a| *a.borrow());
    let opcode_atom = atoms[atom::_NET_SYSTEM_TRAY_OPCODE];

    if event.r#type == atoms[atom::I3_SYNC] {
        let window = event.data32[0];
        let rnd = event.data32[1];
        // Forward the request to i3 via the IPC interface so that all pending
        // IPC messages are guaranteed to be handled.
        let payload = format!("{{\"rnd\":{}, \"window\":{}}}", rnd, window);
        crate::ipc::i3_send_msg(crate::ipc::I3_IPC_MESSAGE_TYPE_SYNC, Some(&payload));
    } else if event.r#type == opcode_atom && event.format == 32 {
        DLOG!("_NET_SYSTEM_TRAY_OPCODE received");
        // event->data.data32[0] is the timestamp
        let op = event.data32[1];
        if op == SYSTEM_TRAY_REQUEST_DOCK {
            let client = event.data32[2];

            // Needed to get the most recent value of XEMBED_MAPPED, for
            // UnmapNotify events and because some tray applications (e.g. VLC)
            // use override_redirect which causes no ConfigureRequest to be sent.
            x.change_window_attributes(
                client,
                &[(
                    i3_core::backend::XCB_CW_EVENT_MASK,
                    XCB_EVENT_MASK_PROPERTY_CHANGE
                        | XCB_EVENT_MASK_STRUCTURE_NOTIFY
                        | XCB_EVENT_MASK_RESIZE_REDIRECT,
                )],
            );

            // Request the _XEMBED_INFO property. The XEMBED specification (which
            // is referred by the tray specification) says this has to be set,
            // but VLC does not set it...
            let mut map_it = true;
            let mut xe_version = 1;
            let xembedr = x.get_property(client, atoms[atom::_XEMBED_INFO], XCB_GET_PROPERTY_TYPE_ANY, 0, 2 * 32);
            if let Some(reply) = xembedr {
                if reply.value_len() != 0 {
                    let words = reply.words32();
                    if words.len() >= 2 {
                        map_it = (words[1] & XEMBED_MAPPED) == XEMBED_MAPPED;
                        xe_version = words[0] as i32;
                        if xe_version > 1 {
                            xe_version = 1;
                        }
                    }
                } else {
                    ELOG!("Window {:08x} violates the XEMBED protocol, _XEMBED_INFO not set", client);
                }
            }

            DLOG!("X window {:08x} requested docking", client);

            let tray_output = OUTPUT_FOR_TRAY.with(|o| o.get());
            let tray_output = match tray_output {
                Some(id) => id,
                None => {
                    ELOG!("No output found for tray");
                    return;
                }
            };
            let output_index = match crate::outputs::output_index_by_ptr(tray_output) {
                Some(idx) => idx,
                None => {
                    ELOG!("No output found for tray");
                    return;
                }
            };

            let (bar_id, rect_w) = crate::outputs::outputs(|outputs| {
                (outputs[output_index].bar.id, outputs[output_index].rect.w)
            });
            let tray_padding = logical_px(config::config(|c| c.tray_padding));

            x.reparent_window(
                client,
                bar_id,
                (rect_w - icon_size() - tray_padding) as i16,
                tray_padding as i16,
            );

            // We reconfigure the window to use a reasonable size.
            x.configure_window(
                client,
                &[(ConfigWindow::WIDTH, icon_size() as u32), (ConfigWindow::HEIGHT, icon_size() as u32)],
            );

            // send the XEMBED_EMBEDDED_NOTIFY message
            let _ = (XCB_CURRENT_TIME, XEMBED_EMBEDDED_NOTIFY, xe_version);
            x.send_client_message(
                client,
                false,
                XCB_EVENT_MASK_NO_EVENT,
                atoms[atom::_XEMBED],
                [
                    XCB_CURRENT_TIME,
                    XEMBED_EMBEDDED_NOTIFY,
                    bar_id,
                    xe_version as u32,
                    0,
                ],
            );

            // Put the client inside the save set. Upon termination of i3bar,
            // these clients will be correctly reparented to their most closest
            // living ancestor.
            x.change_save_set(0 /* XCB_SET_MODE_INSERT */, client);

            let mut tc = trayclient {
                win: client,
                xe_version,
                mapped: false,
                ..Default::default()
            };
            trayclient_update_class(x, &mut tc);
            crate::outputs::outputs(|outputs| outputs[output_index].trayclients.push(tc));

            if map_it {
                DLOG!("Mapping dock client");
                x.map_window(client);
            } else {
                DLOG!("Not mapping dock client yet");
            }
            // Trigger an update to copy the statusline text to the appropriate
            // position.
            configure_trayclients(x);
            draw_bars(x, draw, false);
        }
    }
}

/// C: xcb.c:1169 static void handle_destroy_notify(xcb_destroy_notify_event_t *event)
pub fn handle_destroy_notify(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, window: u32) {
    let mut output_index = 0usize;
    let client = trayclient_and_output_from_window(window, Some(&mut output_index));
    if client.is_none() {
        DLOG!("WARNING: Could not find corresponding tray window.");
        return;
    }

    DLOG!("Removing tray client with window ID {:08x}", window);
    crate::outputs::outputs(|outputs| {
        let list = &mut outputs[output_index].trayclients;
        if let Some(pos) = list.iter().position(|c| c.win == window) {
            list.remove(pos);
        }
    });

    // Trigger an update, we now have more space for the statusline.
    configure_trayclients(x);
    draw_bars(x, draw, false);
}

/// C: xcb.c:1197 static void handle_map_notify(xcb_map_notify_event_t *event)
pub fn handle_map_notify(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, window: u32) {
    DLOG!("MapNotify for window = {:08x}", window);

    if trayclient_from_window(window).is_none() {
        DLOG!("WARNING: Could not find corresponding tray window.");
        return;
    }

    DLOG!("Tray client mapped (window ID {:08x}). Adjusting tray.", window);
    crate::outputs::outputs(|outputs| {
        for output in outputs.iter_mut() {
            for client in output.trayclients.iter_mut() {
                if client.win == window {
                    client.mapped = true;
                }
            }
        }
    });

    // Trigger an update, we now have one extra tray client.
    configure_trayclients(x);
    draw_bars(x, draw, false);
}

/// C: xcb.c:1217 static void handle_unmap_notify(xcb_unmap_notify_event_t *event)
pub fn handle_unmap_notify(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, window: u32) {
    DLOG!("UnmapNotify for window = {:08x}", window);

    if trayclient_from_window(window).is_none() {
        DLOG!("WARNING: Could not find corresponding tray window.");
        return;
    }

    DLOG!("Tray client unmapped (window ID {:08x}). Adjusting tray.", window);
    crate::outputs::outputs(|outputs| {
        for output in outputs.iter_mut() {
            for client in output.trayclients.iter_mut() {
                if client.win == window {
                    client.mapped = false;
                }
            }
        }
    });

    // Trigger an update, we now have more space for the statusline.
    configure_trayclients(x);
    draw_bars(x, draw, false);
}

/// C: xcb.c:1237 static void handle_property_notify(xcb_property_notify_event_t *event)
pub fn handle_property_notify(x: &mut dyn XConnLike, window: u32, property_atom: u32, state: u8) {
    let atoms = ATOMS.with(|a| *a.borrow());

    if property_atom == atoms[atom::_XEMBED_INFO] && state == 0 /* XCB_PROPERTY_NEW_VALUE */ {
        // _XEMBED_INFO property tells us whether a dock client should be mapped
        // or unmapped.
        DLOG!("xembed_info updated");

        let client = match trayclient_from_window(window) {
            Some(c) => c,
            None => {
                ELOG!("PropertyNotify received for unknown window {:08x}", window);
                return;
            }
        };

        let xembedr = x.get_property(client.win, atoms[atom::_XEMBED_INFO], XCB_GET_PROPERTY_TYPE_ANY, 0, 2 * 32);
        let reply = match xembedr {
            Some(r) if r.value_len() != 0 => r,
            _ => {
                DLOG!("xembed_info unset");
                return;
            }
        };

        let words = reply.words32();
        if words.len() < 2 {
            return;
        }
        let map_it = (words[1] & XEMBED_MAPPED) == XEMBED_MAPPED;
        DLOG!("map state now {}", map_it);
        if client.mapped && !map_it {
            // need to unmap the window
            x.unmap_window(client.win);
        } else if !client.mapped && map_it {
            // need to map the window
            x.map_window(client.win);
        }
    } else if property_atom == XCB_ATOM_WM_CLASS {
        if let Some(mut client) = trayclient_from_window(window) {
            trayclient_update_class(x, &mut client);
            // Write the updated class back into the output's client list.
            crate::outputs::outputs(|outputs| {
                for output in outputs.iter_mut() {
                    for existing in output.trayclients.iter_mut() {
                        if existing.win == client.win {
                            existing.class_class = client.class_class.clone();
                            existing.class_instance = client.class_instance.clone();
                        }
                    }
                }
            });
        }
    }
}

/// C: xcb.c:1305 static void handle_configuration_change(xcb_window_t window)
pub fn handle_configuration_change(x: &mut dyn XConnLike, window: u32) {
    let tray_padding = logical_px(config::config(|c| c.tray_padding));

    let fake = crate::outputs::outputs(|outputs| {
        for output in outputs.iter_mut() {
            if !output.active {
                continue;
            }

            let mut clients = 0;
            // C: TAILQ_FOREACH_REVERSE
            for trayclient in output.trayclients.iter().rev() {
                if !trayclient.mapped {
                    continue;
                }
                clients += 1;

                if trayclient.win != window {
                    continue;
                }

                let rect = rect {
                    x: output.rect.w - (clients * (icon_size() + tray_padding)),
                    y: tray_padding,
                    w: icon_size(),
                    h: icon_size(),
                };
                return Some(rect);
            }
        }
        None
    });

    match fake {
        Some(rect) => {
            DLOG!("This is a tray window. x = {}", rect.x);
            // C: fake_configure_notify(xcb_connection, fake, window, 0) — the
            // tray client learns its real geometry from the synthetic event.
            let rectangle = libi3::xcb_rectangle_t {
                x: rect.x as i16,
                y: rect.y as i16,
                width: rect.w as u16,
                height: rect.h as u16,
            };
            libi3::fake_configure_notify::fake_configure_notify(
                &mut i3_core::xcb_util::Libi3Conn(x),
                rectangle,
                window,
                0,
            );
        }
        None => {
            DLOG!("WARNING: Could not find corresponding tray window.");
        }
    }
}

/// C: xcb.c:1337 static void handle_configure_request(xcb_configure_request_event_t *event)
pub fn handle_configure_request(x: &mut dyn XConnLike, window: u32) {
    DLOG!("ConfigureRequest for window = {:08x}", window);
    handle_configuration_change(x, window);
}

/// C: xcb.c:1342 static void handle_resize_request(xcb_resize_request_event_t *event)
pub fn handle_resize_request(x: &mut dyn XConnLike, window: u32) {
    DLOG!("ResizeRequest for window = {:08x}", window);
    handle_configuration_change(x, window);
}

// ---------------------------------------------------------------------------
// xkb (modifier grabbing)
// ---------------------------------------------------------------------------

/// C: xcb.c:1416 static void register_xkb_keyevents(void)
pub fn register_xkb_keyevents(x: &mut dyn XConnLike) {
    // TODO(X11 port): xcb_xkb_use_extension / xcb_xkb_select_events are not part
    // of the XConn trait, so the XKB registration itself is not issued yet. The
    // event handling below is ported, and xkb_base is left at -1 (no XKB).
    //
    // C:
    //   extreply = xcb_get_extension_data(conn, &xcb_xkb_id);
    //   if (!extreply->present) { ELOG("xkb is not present on this server\n"); exit(EXIT_FAILURE); }
    //   xcb_xkb_use_extension(conn, XCB_XKB_MAJOR_VERSION, XCB_XKB_MINOR_VERSION);
    //   xcb_xkb_select_events(conn, XCB_XKB_ID_USE_CORE_KBD,
    //                         XCB_XKB_EVENT_TYPE_STATE_NOTIFY, 0,
    //                         XCB_XKB_EVENT_TYPE_STATE_NOTIFY, 0xff, 0xff, NULL);
    //   xkb_base = extreply->first_event;
    DLOG!("initializing xcb-xkb (TODO(X11 port))");
    let _ = x;
    XKB_BASE.with(|b| b.set(-1));
}

/// C: xcb.c:1434 static void deregister_xkb_keyevents(void)
pub fn deregister_xkb_keyevents(x: &mut dyn XConnLike) {
    // TODO(X11 port): xcb_xkb_select_events(conn, XCB_XKB_ID_USE_CORE_KBD, 0, 0,
    // 0, 0xff, 0xff, NULL);
    let _ = x;
}

/// C: xcb.c:1370-1400 — the xkb branch of xcb_prep_cb.
///
/// Returns true when the bar visibility was re-evaluated.
pub fn xkb_state_notify(x: &mut dyn XConnLike, event_type: u8, state_mods: u16) -> bool {
    DLOG!("received an xkb event");

    let modifier = config::config(|c| c.modifier);
    let mod_mask = modifier & 0xFFFF;
    let new_mod_pressed = mod_mask != 0 && (state_mods as u32 & mod_mask) == mod_mask;

    let mut reevaluated = false;
    if new_mod_pressed != MOD_PRESSED.with(|m| m.get()) {
        MOD_PRESSED.with(|m| m.set(new_mod_pressed));
        if event_type == XCB_XKB_STATE_NOTIFY && modifier != XCB_NONE {
            if new_mod_pressed {
                ACTIVATED_MODE.with(|a| a.set(false));
                unhide_bars(x);
            } else if !ACTIVATED_MODE.with(|a| a.get()) {
                hide_bars(x);
            }
            reevaluated = true;
        }
    }
    reevaluated
}

pub fn mod_pressed() -> bool {
    MOD_PRESSED.with(|m| m.get())
}

// ---------------------------------------------------------------------------
// X initialization
// ---------------------------------------------------------------------------

/// C: xcb.c:1445 char *init_xcb_early(void)
///
/// The connection itself is owned by the caller (main.rs) in the Rust port, so
/// the function only performs the requests that follow xcb_connect().
pub fn init_xcb_early(x: &mut dyn XConnLike) -> Option<String> {
    // C: init_xcb_early() passes &screen to xcb_connect(); the socket name,
    // the tray selection and the strut work all use it.
    set_screen(crate::live::screen() as i32);

    let root_screen = x.root_screen();
    let mut depth = root_screen.root_depth;
    let mut colormap = root_screen.default_colormap;

    // C: xcb.c:1352-1367 —
    //   depth = root_screen->root_depth;
    //   colormap = root_screen->default_colormap;
    //   visual_type = config.transparency ?
    //       xcb_aux_find_visual_by_attrs(root_screen, -1, 32) : NULL;
    //   if (visual_type != NULL) {
    //       depth = xcb_aux_get_depth_of_visual(root_screen,
    //                                           visual_type->visual_id);
    //       colormap = xcb_generate_id(xcb_connection);
    //       xcb_create_colormap_checked(xcb_connection,
    //                                   XCB_COLORMAP_ALLOC_NONE, colormap,
    //                                   xcb_root, visual_type->visual_id);
    //       if (xcb_request_failed(cm_cookie, "Could not allocate colormap"))
    //           exit(EXIT_FAILURE);
    //   } else {
    //       visual_type = get_visualtype(root_screen);
    //   }
    let transparency = config::config(|c| c.transparency);
    let argb32 = if transparency { x.find_visual_by_depth(32) } else { None };
    let visual_id = match argb32 {
        Some((visual_id, visual_depth)) => {
            // The ARGB32 visual: depth 32 plus its own colormap, so RGBA
            // colors keep their alpha instead of being clipped to opaque.
            depth = visual_depth;
            colormap = x.generate_id();
            x.create_colormap(colormap, root_screen.root, visual_id);
            DLOG!(
                "Using ARGB32 visual {:#x} (depth {}) for the transparent bar",
                visual_id,
                depth
            );
            visual_id
        }
        None => {
            // C: visual_type = get_visualtype(root_screen); — without a 32-bit
            // visual the bar stays opaque on the root depth/visual (the C code
            // has no error message here either).
            if transparency {
                DLOG!("No 32-bit visual found, the bar cannot be transparent");
            }
            root_screen.root_visual
        }
    };

    DEPTH.with(|d| d.set(depth));
    COLORMAP.with(|c| c.set(colormap));
    VISUAL_ID.with(|v| v.set(visual_id));
    // The drawing surfaces and the off-screen pixmaps use the same depth
    // (C: draw_util_surface_init reads the global depth of xcb.c).
    crate::live::set_depth(depth as u8);

    // Now we get the atoms and save them in a nice data structure
    get_atoms(x);

    root_atom_contents(x, "I3_SOCKET_PATH")
}

/// C: libi3/root_atom_contents.c — via the XConn abstraction (the libi3
/// function takes a concrete RustConnection, which the port does not own).
fn root_atom_contents(x: &mut dyn XConnLike, atomname: &str) -> Option<String> {
    let atom = x.intern_atom(atomname)?;
    let root = x.root_screen().root;
    let reply = x.get_property(root, atom, XCB_ATOM_STRING, 0, 1024)?;
    reply.first_string().filter(|s| !s.is_empty())
}

/// C: xcb.c:1601 void get_atoms(void)
pub fn get_atoms(x: &mut dyn XConnLike) {
    let mut atoms = [0u32; atom::NUM_ATOMS];
    for (idx, name) in ATOM_NAMES.iter().enumerate() {
        match x.intern_atom(name) {
            Some(atom) => atoms[idx] = atom,
            None => {
                ELOG!("Could not get atom {}", name);
                std::process::exit(1);
            }
        }
    }
    ATOMS.with(|a| *a.borrow_mut() = atoms);
    DLOG!("Got atoms");
}

/// C: xcb.c:1497 void init_xcb_late(char *fontname)
pub fn init_xcb_late(fontname: Option<&str>, x: &mut dyn XConnLike) {
    // C: if (fontname == NULL) fontname = "-misc-fixed-...";
    //    font = load_font(fontname, true); set_font(&font);
    let height = match crate::live::connection() {
        Some(conn) => {
            let font = libi3::font::load_font(Some(&*conn), fontname.unwrap_or(DEFAULT_FONT), true);
            libi3::font::set_font(&font);
            font.height
        }
        None => placeholder_font_height(fontname),
    };
    FONT_HEIGHT.with(|h| h.set(height));
    DLOG!("Calculated font-height: {}", height);

    let (bar_height_cfg, padding) = config::config(|c| (c.bar_height, c.padding));

    // If the bar height was explicitly set (but padding was not set), use it.
    // Otherwise, calculate it based on the font size.
    let default_px = height + 2 * logical_px(ws_voff_px);
    let mut padding_scaled = logical_px(padding.y as i32) + logical_px(padding.height as i32);
    if bar_height_cfg > 0 && padding.x == 0 && padding.y == 0 && padding.width == 0 && padding.height == 0 {
        padding_scaled = bar_height_cfg as i32 - default_px;
        DLOG!("setting padding_scaled={} based on bar_height={}", padding_scaled, bar_height_cfg);
    } else {
        DLOG!("padding: x={}, y={} -> padding_scaled={}", padding.x, padding.y, padding_scaled);
    }
    BAR_HEIGHT.with(|h| h.set(default_px + padding_scaled));
    let tray_padding = logical_px(config::config(|c| c.tray_padding));
    ICON_SIZE.with(|s| s.set(bar_height() - 2 * tray_padding));

    if config::config(|c| c.separator_symbol.is_some()) {
        let width = config::config_mut(|c| match c.separator_symbol.as_mut() {
            Some(symbol) => predict_text_width(symbol),
            None => 0,
        });
        SEPARATOR_SYMBOL_WIDTH.with(|w| w.set(width));
    }

    x.flush();

    if config::config(|c| c.hide_on_modifier) == bar_display_mode_t::M_HIDE {
        register_xkb_keyevents(x);
    }
}

/// C: xcb.c:1478 static void send_tray_clientmessage(void)
pub fn send_tray_clientmessage(x: &mut dyn XConnLike) {
    let atoms = ATOMS.with(|a| *a.borrow());
    let tray_atom = TRAY_REPLY.with(|a| a.get());
    let selwin = SELWIN.with(|w| w.get());
    let root = x.root_screen().root;

    x.send_client_message(
        root,
        false,
        0xFFFFFF,
        atoms[atom::MANAGER],
        [XCB_CURRENT_TIME, tray_atom, selwin, 0, 0],
    );
}

/// C: xcb.c:1520 static void init_tray(void)
pub fn init_tray(x: &mut dyn XConnLike) {
    DLOG!("Initializing system tray functionality");

    let screen = screen();
    let atomname = format!("_NET_SYSTEM_TRAY_S{}", screen);

    let mut tray_atom = TRAY_REPLY.with(|a| a.get());
    let mut tray_cookie = 0u32;
    if tray_atom == XCB_NONE {
        tray_cookie = x.intern_atom(&atomname).unwrap_or(XCB_NONE);
    }

    // tray support: we need a window to own the selection
    let selwin = x.generate_id();
    SELWIN.with(|w| w.set(selwin));
    let root_screen = x.root_screen();
    let values = [
        (i3_core::backend::XCB_CW_BACK_PIXEL, root_screen.black_pixel),
        (i3_core::backend::XCB_CW_BORDER_PIXEL, root_screen.black_pixel),
        (i3_core::backend::XCB_CW_OVERRIDE_REDIRECT, 1),
        (i3_core::backend::XCB_CW_COLORMAP, COLORMAP.with(|c| c.get())),
    ];
    x.create_window(
        DEPTH.with(|d| d.get()),
        selwin,
        root_screen.root,
        -1,
        -1,
        1,
        1,
        0,
        i3_core::backend::XCB_WINDOW_CLASS_INPUT_OUTPUT,
        VISUAL_ID.with(|v| v.get()),
        &values,
    );

    // set the atoms
    let orientation = _NET_SYSTEM_TRAY_ORIENTATION_HORZ;
    let atoms = ATOMS.with(|a| *a.borrow());
    x.change_property(
        selwin,
        atoms[atom::_NET_SYSTEM_TRAY_ORIENTATION],
        XCB_ATOM_CARDINAL,
        32,
        PropMode::REPLACE,
        &orientation.to_le_bytes(),
    );
    x.change_property(
        selwin,
        atoms[atom::_NET_SYSTEM_TRAY_VISUAL],
        XCB_ATOM_VISUALID,
        32,
        PropMode::REPLACE,
        &VISUAL_ID.with(|v| v.get()).to_le_bytes(),
    );

    init_tray_colors(x);

    if tray_atom == XCB_NONE {
        tray_atom = tray_cookie;
        TRAY_REPLY.with(|a| a.set(tray_atom));
    }

    // C: xcb_set_selection_owner(xcb_connection, selwin, tray_reply->atom,
    //     XCB_CURRENT_TIME);
    x.set_selection_owner(selwin, tray_atom, XCB_CURRENT_TIME);

    /* Verify that we have the selection */
    // C: a missing xcb_get_selection_owner reply is fatal.
    let owner = match x.get_selection_owner(tray_atom) {
        Some(owner) => owner,
        None => {
            ELOG!("Could not get selection owner for {}", atomname);
            std::process::exit(1);
        }
    };

    if owner != selwin {
        ELOG!(
            "Could not set the {} selection. Maybe another tray is already running?",
            atomname
        );
        // NOTE that this error is not fatal. We just can’t provide tray
        // functionality. The C code returns here without resetting selwin, so
        // reconfig_windows() never retries init_tray() — that is the disable.
        return;
    }

    send_tray_clientmessage(x);
}

/// C: xcb.c:1585 void init_tray_colors(void)
pub fn init_tray_colors(x: &mut dyn XConnLike) {
    // Convert colors.bar_fg (#rrggbb) to 16-bit RGB
    let bar_fg = config::config(|c| c.colors.bar_fg.clone()).unwrap_or_else(|| "#FFFFFF".to_string());

    DLOG!("Setting bar_fg = {} as _NET_SYSTEM_TRAY_COLORS", bar_fg);

    let get = |r: std::ops::Range<usize>| -> u8 {
        u8::from_str_radix(&bar_fg[r], 16).unwrap_or(0)
    };
    let (r, g, b) = if bar_fg.len() >= 7 {
        (get(1..3), get(3..5), get(5..7))
    } else {
        (0xff, 0xff, 0xff)
    };

    let r16 = ((r as u16) << 8) | r as u16;
    let g16 = ((g as u16) << 8) | g as u16;
    let b16 = ((b as u16) << 8) | b as u16;

    let tray_colors: [u16; 12] = [
        r16, g16, b16, // foreground color
        r16, g16, b16, // error color
        r16, g16, b16, // warning color
        r16, g16, b16, // success color
    ];

    let mut bytes = Vec::with_capacity(48);
    for v in tray_colors {
        bytes.extend_from_slice(&(v as u32).to_le_bytes());
    }

    let atoms = ATOMS.with(|a| *a.borrow());
    let selwin = SELWIN.with(|w| w.get());
    x.change_property(
        selwin,
        atoms[atom::_NET_SYSTEM_TRAY_COLORS],
        XCB_ATOM_CARDINAL,
        32,
        PropMode::REPLACE,
        &bytes,
    );
}

/// C: xcb.c:1622 void clean_xcb(void)
pub fn clean_xcb(x: &mut dyn XConnLike) {
    crate::outputs::free_outputs(x);
    // C: xcb.c:1622 free_font() when a font was loaded.
    if crate::live::connection().is_some() {
        libi3::font::free_font();
    }
    x.flush();
}

/// C: xcb.c:1647 void kick_tray_clients(i3_output *output)
pub fn kick_tray_clients(x: &mut dyn XConnLike, output_index: usize) {
    let clients = crate::outputs::outputs(|outputs| {
        std::mem::take(&mut outputs[output_index].trayclients)
    });
    if clients.is_empty() {
        return;
    }

    let root = x.root_screen().root;
    for trayclient in clients {
        // Unmap, then reparent (to root) the tray client windows.
        x.unmap_window(trayclient.win);
        x.reparent_window(trayclient.win, root, 0, 0);
    }

    // Fake a DestroyNotify so that Qt re-adds tray icons. We cannot actually
    // destroy the window because then Qt will not restore its event mask on the
    // new window.
    let selwin = SELWIN.with(|w| w.get());
    x.send_client_message(selwin, false, XCB_EVENT_MASK_STRUCTURE_NOTIFY, 17 /* XCB_DESTROY_NOTIFY */, [selwin, selwin, 0, 0, 0]);

    send_tray_clientmessage(x);
}

/// C: xcb.c:1688 void destroy_window(i3_output *output)
///
/// The C code takes the output pointer; the Rust port takes its index in the
/// global list (the pointer identity is i3_output::ptr_id).
pub fn destroy_window(x: &mut dyn XConnLike, index: usize) {
    let bar_id = crate::outputs::outputs(|outputs| outputs[index].bar.id);
    if bar_id == XCB_NONE {
        return;
    }

    kick_tray_clients(x, index);

    // TODO(X11 port): draw_util_surface_free() + xcb_free_pixmap() of the two
    // off-screen buffers (both depend on requests the XConn trait lacks).
    x.destroy_window(bar_id);
    crate::outputs::outputs(|outputs| outputs[index].bar.id = XCB_NONE);
}

/// C: xcb.c:1707 static xcb_void_cookie_t config_strut_partial(i3_output *output)
pub fn config_strut_partial(output: &i3_output) -> Vec<u8> {
    // A local struct to save the strut_partial property.
    let left = 0u32;
    let right = 0u32;
    let mut top = 0u32;
    let mut bottom = 0u32;
    let mut top_start_x = 0u32;
    let mut top_end_x = 0u32;
    let mut bottom_start_x = 0u32;
    let mut bottom_end_x = 0u32;

    match config::config(|c| c.position) {
        position_t::POS_NONE => {}
        position_t::POS_TOP => {
            top = bar_height() as u32;
            top_start_x = output.rect.x as u32;
            top_end_x = (output.rect.x + output.rect.w) as u32;
        }
        position_t::POS_BOT => {
            bottom = bar_height() as u32;
            bottom_start_x = output.rect.x as u32;
            bottom_end_x = (output.rect.x + output.rect.w) as u32;
        }
    }

    let strut_partial = [
        left,
        right,
        top,
        bottom,
        0, // left_start_y
        0, // left_end_y
        0, // right_start_y
        0, // right_end_y
        top_start_x,
        top_end_x,
        bottom_start_x,
        bottom_end_x,
    ];

    let mut bytes = Vec::with_capacity(48);
    for v in strut_partial {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// C: xcb.c:1774 static i3_output *get_tray_output(void)
pub fn get_tray_output() -> Option<u32> {
    let tray_outputs = config::config(|c| c.tray_outputs.clone());

    if tray_outputs.is_empty() {
        // No tray_output specified, use first active output.
        return crate::outputs::outputs(|outputs| {
            outputs.iter().find(|o| o.active).map(|o| o.ptr_id)
        });
    } else if libi3::strcasecmp(&tray_outputs[0].output, "none") == 0 {
        // Check for "tray_output none"
        return None;
    }

    // If one or more tray_output assignments were specified, we ensure that at
    // least one of them is actually an output managed by this instance.
    crate::outputs::outputs(|outputs| {
        for tray_output in tray_outputs.iter() {
            for output in outputs.iter() {
                let name = output.name.clone().unwrap_or_default();
                if output.active
                    && (libi3::strcasecmp(&name, &tray_output.output) == 0
                        || (libi3::strcasecmp(&tray_output.output, "primary") == 0 && output.primary))
                {
                    return Some(output.ptr_id);
                }
            }
        }
        None
    })
}

thread_local! {
    /// C: xcb.c:36 int screen;
    static SCREEN: Cell<i32> = const { Cell::new(0) };
}

/// C: xcb.c:36 int screen
pub fn screen() -> i32 {
    SCREEN.with(|s| s.get())
}

/// C: xcb.c:36 int screen (setter; main.rs takes it from the connect call)
pub fn set_screen(value: i32) {
    SCREEN.with(|s| s.set(value));
}

/// C: xcb.c:1832 void reconfig_windows(bool redraw_bars)
pub fn reconfig_windows(x: &mut dyn XConnLike, redraw_bars: bool) {
    let count = crate::outputs::outputs(|outputs| outputs.len());

    for idx in 0..count {
        let active = crate::outputs::outputs(|outputs| outputs[idx].active);
        if !active {
            // If an output is not active, we destroy its bar.
            DLOG!("Destroying window for output");
            destroy_window(x, idx);
            continue;
        }

        let bar_id = crate::outputs::outputs(|outputs| outputs[idx].bar.id);
        if bar_id == XCB_NONE {
            DLOG!("Creating window for output");

            let bar_id = x.generate_id();
            let buffer_id = x.generate_id();
            let statusline_buffer_id = x.generate_id();

            let (out_rect, out_name) = crate::outputs::outputs(|outputs| {
                (outputs[idx].rect, outputs[idx].name.clone())
            });
            let hide_on_modifier = config::config(|c| c.hide_on_modifier);
            let bar_bg = colors(|c| c.bar_bg);

            // We enable the following EventMask fields: EXPOSURE, to get expose
            // events (we have to re-draw then); SUBSTRUCTURE_REDIRECT, to get
            // ConfigureRequests when the tray child windows use ConfigureWindow;
            // BUTTON_PRESS, to handle clicks on the workspace buttons.
            let mut event_mask = (1 << 15) | (1 << 20) | (1 << 12) | (1 << 13);
            if hide_on_modifier == bar_display_mode_t::M_DOCK {
                // If the bar is normally visible, catch visibility change
                // events to suspend the status process when the bar is obscured
                // by full-screened windows.
                event_mask |= 1 << 16;
            }

            let black_pixel = x.root_screen().black_pixel;
            let values = [
                (i3_core::backend::XCB_CW_BACK_PIXEL, bar_bg.colorpixel),
                (i3_core::backend::XCB_CW_BORDER_PIXEL, black_pixel),
                (
                    i3_core::backend::XCB_CW_OVERRIDE_REDIRECT,
                    (hide_on_modifier != bar_display_mode_t::M_DOCK) as u32,
                ),
                (i3_core::backend::XCB_CW_EVENT_MASK, event_mask),
                (i3_core::backend::XCB_CW_COLORMAP, COLORMAP.with(|c| c.get())),
                (i3_core::backend::XCB_CW_CURSOR, 0),
            ];

            let bar_y = if config::config(|c| c.position) == position_t::POS_TOP {
                out_rect.y
            } else {
                out_rect.y + out_rect.h - bar_height()
            };

            let root_window = x.root_screen().root;
            x.create_window(
                DEPTH.with(|d| d.get()),
                bar_id,
                root_window,
                out_rect.x as i16,
                bar_y as i16,
                out_rect.w as u16,
                bar_height() as u16,
                0,
                i3_core::backend::XCB_WINDOW_CLASS_INPUT_OUTPUT,
                VISUAL_ID.with(|v| v.get()),
                &values,
            );

            // TODO(X11 port): the two off-screen pixmaps (xcb_create_pixmap)
            // cannot be created through the XConn trait yet; the ids are kept so
            // that the layout code has its surfaces.
            let _ = (buffer_id, statusline_buffer_id);

            // Set the WM_CLASS and WM_NAME (we don't need UTF-8) atoms.
            let bar_id_name = config::config(|c| c.bar_id.clone()).unwrap_or_default();
            let mut class = bar_id_name.clone().into_bytes();
            class.push(0);
            class.extend_from_slice(b"i3bar");
            class.push(0);
            x.change_property(bar_id, XCB_ATOM_WM_CLASS, XCB_ATOM_STRING, 8, PropMode::REPLACE, &class);

            let name = format!("i3bar for output {}", out_name.clone().unwrap_or_default());
            x.change_property(bar_id, XCB_ATOM_WM_NAME, XCB_ATOM_STRING, 8, PropMode::REPLACE, name.as_bytes());

            // We want dock windows (for now). When override_redirect is set, i3
            // is ignoring this one.
            let atoms = ATOMS.with(|a| *a.borrow());
            x.change_property(
                bar_id,
                atoms[atom::_NET_WM_WINDOW_TYPE],
                XCB_ATOM_ATOM,
                32,
                PropMode::REPLACE,
                &atoms[atom::_NET_WM_WINDOW_TYPE_DOCK].to_le_bytes(),
            );

            let strut = crate::outputs::outputs(|outputs| {
                config_strut_partial(&outputs[idx])
            });
            x.change_property(
                bar_id,
                atoms[atom::_NET_WM_STRUT_PARTIAL],
                XCB_ATOM_CARDINAL,
                32,
                PropMode::REPLACE,
                &strut,
            );

            // We finally map the bar (display it on screen), unless the
            // modifier-switch is on.
            if hide_on_modifier == bar_display_mode_t::M_DOCK {
                x.map_window(bar_id);
            }

            crate::outputs::outputs(|outputs| {
                let output = &mut outputs[idx];
                output.bar.id = bar_id;
                output.bar.width = out_rect.w;
                output.bar.height = bar_height();
                output.buffer.id = buffer_id;
                output.buffer.width = out_rect.w;
                output.buffer.height = bar_height();
                output.statusline_buffer.id = statusline_buffer_id;
                output.statusline_buffer.width = out_rect.w;
                output.statusline_buffer.height = bar_height();
                if hide_on_modifier == bar_display_mode_t::M_DOCK {
                    output.visible = true;
                }
            });
        } else {
            // We already have a bar, so we just reconfigure it.
            let (out_rect, out_name) = crate::outputs::outputs(|outputs| {
                (outputs[idx].rect, outputs[idx].name.clone())
            });
            let hide_on_modifier = config::config(|c| c.hide_on_modifier);

            let y = if config::config(|c| c.position) == position_t::POS_TOP {
                out_rect.y
            } else {
                out_rect.y + out_rect.h - bar_height()
            };
            let values = [
                (ConfigWindow::X, out_rect.x as u32),
                (ConfigWindow::Y, y as u32),
                (ConfigWindow::WIDTH, out_rect.w as u32),
                (ConfigWindow::HEIGHT, bar_height() as u32),
                (ConfigWindow::STACK_MODE, StackMode::ABOVE as u32),
            ];

            DLOG!("Reconfiguring strut partial property for output");
            let strut = crate::outputs::outputs(|outputs| config_strut_partial(&outputs[idx]));
            let atoms = ATOMS.with(|a| *a.borrow());
            x.change_property(
                bar_id,
                atoms[atom::_NET_WM_STRUT_PARTIAL],
                XCB_ATOM_CARDINAL,
                32,
                PropMode::REPLACE,
                &strut,
            );

            // TODO(X11 port): destroy + recreate the two pixmaps
            // (xcb_free_pixmap/xcb_create_pixmap) and re-init the surfaces.

            DLOG!("Reconfiguring window for output to {},{}", values[0].1, values[1].1);
            x.configure_window(bar_id, &values);

            // C: xcb_free_pixmap + xcb_create_pixmap for both buffers, then
            // draw_util_surface_init() with the new size. The pixmaps belong to
            // the drawing backend (live.rs), which recreates them as soon as the
            // recorded size differs; recording the new size is the C call's
            // effect on the i3_output.
            crate::outputs::outputs(|outputs| {
                let output = &mut outputs[idx];
                output.bar.width = out_rect.w;
                output.bar.height = bar_height();
                output.buffer.width = out_rect.w;
                output.buffer.height = bar_height();
                output.statusline_buffer.width = out_rect.w;
                output.statusline_buffer.height = bar_height();
            });

            // C: mask = XCB_CW_OVERRIDE_REDIRECT;
            x.change_window_attributes(
                bar_id,
                &[(
                    i3_core::backend::XCB_CW_OVERRIDE_REDIRECT,
                    (hide_on_modifier != bar_display_mode_t::M_DOCK) as u32,
                )],
            );

            if redraw_bars {
                // Unmap the window, and draw it again when in dock mode.
                x.unmap_window(bar_id);
                if hide_on_modifier == bar_display_mode_t::M_DOCK {
                    crate::child::cont_children();
                    x.map_window(bar_id);
                } else {
                    crate::child::stop_children();
                }

                if hide_on_modifier == bar_display_mode_t::M_HIDE {
                    // Switching to hide mode, register for keyevents.
                    register_xkb_keyevents(x);
                } else {
                    // Switching to dock/invisible mode, deregister from
                    // keyevents.
                    deregister_xkb_keyevents(x);
                }
            }
            let _ = out_name;
        }
    }

    // Finally, check if we want to initialize the tray or destroy the selection
    // window. The result of get_tray_output() is cached.
    let tray_output = get_tray_output();
    OUTPUT_FOR_TRAY.with(|o| o.set(tray_output));
    match tray_output {
        Some(_) => {
            if SELWIN.with(|w| w.get()) == XCB_NONE {
                init_tray(x);
            }
        }
        None => {
            if SELWIN.with(|w| w.get()) != XCB_NONE {
                DLOG!("Destroying tray selection window");
                let selwin = SELWIN.with(|w| w.get());
                x.destroy_window(selwin);
                SELWIN.with(|w| w.set(XCB_NONE));
            }
        }
    }
}

/// C: xcb.c:2100 static void draw_button(surface_t *surface, color_t fg_color,
/// color_t bg_color, color_t border_color, int x, int width, int text_width,
/// i3String *text)
#[allow(clippy::too_many_arguments)]
fn draw_button(
    index: usize,
    surface: surface_id,
    fg_color: color_t,
    bg_color: color_t,
    border_color: color_t,
    x_pos: i32,
    width: i32,
    text_width: i32,
    text: &mut i3String,
    cmds: &mut Vec<DrawCmd>,
) {
    let bar_h = bar_height();
    let height = bar_h - 2 * logical_px(1);

    // Draw the border of the button.
    cmds.push(DrawCmd::Rectangle {
        surface,
        output: index,
        color: border_color,
        x: x_pos,
        y: logical_px(1),
        w: width,
        h: height,
    });

    // Draw the inside of the button.
    cmds.push(DrawCmd::Rectangle {
        surface,
        output: index,
        color: bg_color,
        x: x_pos + logical_px(1),
        y: 2 * logical_px(1),
        w: width - 2 * logical_px(1),
        h: height - 2 * logical_px(1),
    });

    cmds.push(DrawCmd::Text {
        surface,
        output: index,
        text: i3string_as_utf8(text).unwrap_or("").to_string(),
        pango_markup: libi3::i3string_is_markup(text),
        fg: fg_color,
        bg: bg_color,
        x: x_pos + (width - text_width) / 2,
        y: bar_h / 2 - font_height() / 2,
        max_width: text_width,
    });
}

/// C: xcb.c:2114 void draw_bars(bool unhide)
pub fn draw_bars(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, mut unhide: bool) {
    DLOG!("Drawing bars...");

    // An active output without its own bar: create it now (C: reconfig_windows(false)).
    let needs_reconfig = crate::outputs::outputs(|outputs| {
        outputs.iter().any(|o| o.active && o.bar.id == XCB_NONE)
    });
    if needs_reconfig {
        reconfig_windows(x, false);
    }

    let count = crate::outputs::outputs(|outputs| outputs.len());
    let mut cmds: Vec<DrawCmd> = Vec::new();

    for index in 0..count {
        let active = crate::outputs::outputs(|outputs| outputs[index].active);
        if !active {
            DLOG!("Output inactive, skipping...");
            continue;
        }

        let mut workspace_width = logical_px(config::config(|c| c.padding.x as i32));
        let use_focus_colors = crate::outputs::outputs(|outputs| crate::outputs::output_has_focus(&outputs[index]));

        // First things first: clear the backbuffer.
        let bar_color = colors(|c| if use_focus_colors { c.focus_bar_bg } else { c.bar_bg });
        cmds.push(DrawCmd::ClearSurface {
            surface: surface_id::BUFFER,
            output: index,
            color: bar_color,
        });

        // Workspace buttons.
        if !config::config(|c| c.disable_ws) {
            let ws_count = crate::outputs::outputs(|outputs| outputs[index].workspaces.len());
            for ws_idx in 0..ws_count {
                let (mut fg_color, mut bg_color, mut border_color) = colors(|c| {
                    (c.inactive_ws_fg, c.inactive_ws_bg, c.inactive_ws_border)
                });
                let (visible, focused, urgent, name_width) = crate::outputs::outputs(|outputs| {
                    let ws = &outputs[index].workspaces[ws_idx];
                    (ws.visible, ws.focused, ws.urgent, ws.name_width)
                });

                if visible {
                    if !focused {
                        colors(|c| {
                            fg_color = c.active_ws_fg;
                            bg_color = c.active_ws_bg;
                            border_color = c.active_ws_border;
                        });
                    } else {
                        colors(|c| {
                            fg_color = c.focus_ws_fg;
                            bg_color = c.focus_ws_bg;
                            border_color = c.focus_ws_border;
                        });
                    }
                }
                if urgent {
                    DLOG!("WS is urgent!");
                    colors(|c| {
                        fg_color = c.urgent_ws_fg;
                        bg_color = c.urgent_ws_bg;
                        border_color = c.urgent_ws_border;
                    });
                    unhide = true;
                }

                let w = predict_button_width(name_width);
                let mut name = crate::outputs::outputs(|outputs| {
                    outputs[index].workspaces[ws_idx].name.take()
                });
                if let Some(name) = name.as_mut() {
                    draw_button(
                        index,
                        surface_id::BUFFER,
                        fg_color,
                        bg_color,
                        border_color,
                        workspace_width,
                        w,
                        name_width,
                        name,
                        &mut cmds,
                    );
                }
                crate::outputs::outputs(|outputs| {
                    outputs[index].workspaces[ws_idx].name = name
                });

                workspace_width += w;
                if ws_idx + 1 != ws_count {
                    workspace_width += logical_px(ws_spacing_px);
                }
            }
        }

        // Binding mode indicator.
        let has_binding = BINDING.with(|b| b.borrow().name.is_some());
        if has_binding && !config::config(|c| c.disable_binding_mode_indicator) {
            workspace_width += logical_px(ws_spacing_px);

            let (name_width, binding_fg, binding_bg, binding_border) = {
                let binding = BINDING.with(|b| b.borrow().name_width);
                let colors_ = colors(|c| {
                    (c.binding_mode_fg, c.binding_mode_bg, c.binding_mode_border)
                });
                (binding, colors_.0, colors_.1, colors_.2)
            };

            let w = predict_button_width(name_width);
            let mut name = BINDING.with(|b| b.borrow_mut().name.take());
            if let Some(name) = name.as_mut() {
                draw_button(
                    index,
                    surface_id::BUFFER,
                    binding_fg,
                    binding_bg,
                    binding_border,
                    workspace_width,
                    w,
                    name_width,
                    name,
                    &mut cmds,
                );
            }
            BINDING.with(|b| b.borrow_mut().name = name);

            unhide = true;
            workspace_width += w;
        }

        // Statusline.
        let statusline_empty = statusline::head(|head| head.is_empty());
        if !statusline_empty {
            DLOG!("Printing statusline!");

            let (tray_width, rect_w) = crate::outputs::outputs(|outputs| {
                (get_tray_width(&outputs[index].trayclients), outputs[index].rect.w)
            });
            let hoff = logical_px(((workspace_width > 0) as i32 + (tray_width > 0) as i32) * sb_hoff_px);
            let max_statusline_width = (rect_w - workspace_width - tray_width - hoff) as u32;
            let mut clip_left = 0u32;

            // Reset short mode between outputs.
            let statusline_width = statusline::head(|head| {
                for block in head.iter_mut() {
                    block.use_short = false;
                }
                adjust_statusline_length(head, max_statusline_width)
            });

            if statusline_width > max_statusline_width {
                clip_left = statusline_width - max_statusline_width;
            }

            let visible_statusline_width = MIN(statusline_width, max_statusline_width) as i32;
            let padding_width = logical_px(config::config(|c| c.padding.width as i32));
            let mut x_dest = rect_w - tray_width - logical_px((tray_width > 0) as i32 * sb_hoff_px) - visible_statusline_width;
            x_dest -= padding_width;

            draw_statusline(index, clip_left, use_focus_colors, &mut cmds);
            cmds.push(DrawCmd::CopySurface {
                src: surface_id::STATUSLINE_BUFFER,
                dst: surface_id::BUFFER,
                output: index,
                src_x: 0,
                src_y: 0,
                dst_x: x_dest,
                dst_y: 0,
                w: visible_statusline_width,
                h: bar_height(),
            });

            crate::outputs::outputs(|outputs| {
                outputs[index].statusline_width = statusline_width as i32;
            });
        }
    }

    for cmd in cmds.iter() {
        draw.draw(cmd);
    }

    // Assure the bar is hidden/unhidden according to the specified hidden_state
    // and mode.
    let (hidden_state, hide_on_modifier) = config::config(|c| (c.hidden_state, c.hide_on_modifier));
    if mod_pressed() || hidden_state == hidden_state_t::S_SHOW || unhide {
        unhide_bars(x);
    } else if hide_on_modifier == bar_display_mode_t::M_HIDE {
        hide_bars(x);
    }

    redraw_bars(x, draw);
}

/// C: xcb.c:2230 void redraw_bars(void)
pub fn redraw_bars(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend) {
    let count = crate::outputs::outputs(|outputs| outputs.len());
    for index in 0..count {
        let (active, rect) = crate::outputs::outputs(|outputs| {
            (outputs[index].active, outputs[index].rect)
        });
        if !active {
            continue;
        }

        draw.draw(&DrawCmd::CopySurface {
            src: surface_id::BUFFER,
            dst: surface_id::BAR,
            output: index,
            src_x: 0,
            src_y: 0,
            dst_x: 0,
            dst_y: 0,
            w: rect.w,
            h: rect.h,
        });
        x.flush();
    }
}

/// C: xcb.c:2245 void set_current_mode(struct mode *current)
pub fn set_current_mode(current: &mut mode) {
    let mut name = current.name.take();
    BINDING.with(|b| {
        let mut binding = b.borrow_mut();
        binding.name = name.take();
        binding.name_width = current.name_width;
    });
    let activated = BINDING.with(|b| b.borrow().name.is_some());
    ACTIVATED_MODE.with(|a| a.set(activated));
}

/// The current binding mode's text (test/logging helper).
pub fn current_mode_utf8() -> Option<String> {
    BINDING.with(|b| {
        let mut binding = b.borrow_mut();
        binding.name.as_mut().and_then(|n| i3string_as_utf8(n)).map(|s| s.to_string())
    })
}

/// The current binding mode's rendered width (test helper).
pub fn current_mode_width() -> i32 {
    BINDING.with(|b| b.borrow().name_width)
}

/// The "activated_mode" flag (C: xcb.c:91).
pub fn activated_mode() -> bool {
    ACTIVATED_MODE.with(|a| a.get())
}

// ---------------------------------------------------------------------------
// the X event loop body
// ---------------------------------------------------------------------------

/// The events the C xcb_prep_cb() switch handles, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XEvent {
    /// C: XCB_VISIBILITY_NOTIFY
    VisibilityNotify { window: u32, state: u8 },
    /// C: XCB_EXPOSE
    Expose { window: u32, count: u32 },
    /// C: XCB_BUTTON_PRESS / XCB_BUTTON_RELEASE
    Button { event: xcb_button_press_event_t },
    /// C: XCB_CLIENT_MESSAGE
    ClientMessage { event: xcb_client_message_event_t },
    /// C: XCB_DESTROY_NOTIFY
    DestroyNotify { window: u32 },
    /// C: XCB_UNMAP_NOTIFY
    UnmapNotify { window: u32 },
    /// C: XCB_MAP_NOTIFY
    MapNotify { window: u32 },
    /// C: XCB_PROPERTY_NOTIFY
    PropertyNotify { window: u32, atom: u32, state: u8 },
    /// C: XCB_CONFIGURE_REQUEST
    ConfigureRequest { window: u32 },
    /// C: XCB_RESIZE_REQUEST
    ResizeRequest { window: u32 },
    /// C: xkb_base event (xcb_xkb_state_notify_event_t)
    XkbState { type_: u8, mods: u16 },
}

/// C: xcb.c:1347 static void xcb_prep_cb() — the while loop over the pending
/// events (the watcher is driven by main.rs's poll loop).
pub fn xcb_prep_cb(x: &mut dyn XConnLike, draw: &mut dyn DrawBackend, events: &[XEvent]) {
    for event in events {
        match *event {
            XEvent::XkbState { type_, mods } => {
                xkb_state_notify(x, type_, mods);
            }
            XEvent::VisibilityNotify { window, state } => {
                // Visibility change: a bar is [un]obscured by other window.
                handle_visibility_notify(window, state);
            }
            XEvent::Expose { count, .. } => {
                if count == 0 {
                    // Expose-events happen, when the window needs to be redrawn.
                    redraw_bars(x, draw);
                }
            }
            XEvent::Button { event } => {
                // Button press events are mouse buttons clicked on one of our
                // bars.
                handle_button(x, draw, &event);
            }
            XEvent::ClientMessage { event } => {
                // Client messages are used for client-to-client communication,
                // for example system tray widgets talk to us directly via client
                // messages.
                handle_client_message(x, draw, &event);
            }
            XEvent::DestroyNotify { window } => {
                // DestroyNotify signifies the end of the XEmbed protocol.
                handle_destroy_notify(x, draw, window);
            }
            XEvent::UnmapNotify { window } => {
                // UnmapNotify is received when a tray client hides its window.
                handle_unmap_notify(x, draw, window);
            }
            XEvent::MapNotify { window } => {
                handle_map_notify(x, draw, window);
            }
            XEvent::PropertyNotify { window, atom, state } => {
                handle_property_notify(x, window, atom, state);
            }
            XEvent::ConfigureRequest { window } => {
                // ConfigureRequest, sent by a tray child.
                handle_configure_request(x, window);
            }
            XEvent::ResizeRequest { window } => {
                // ResizeRequest sent by a tray child using override_redirect.
                handle_resize_request(x, window);
            }
        }
    }

    x.flush();
}

/// Test-only helpers: a no-op X connection for code paths that do not need X.
pub mod test_support {
    use super::XConn;

    use i3_core::backend::ScreenInfo;
    use std::cell::Cell;

    /// A no-op XConn (the C code would talk to a real server). Atom interning
    /// and id generation are still answered so that the IPC wiring of main.rs
    /// can run without a display.
    #[derive(Debug, Default)]
    pub struct NullConn {
        next: Cell<u32>,
        /// C: the selection owner this no-op server reports. Without it
        /// init_tray()'s get_selection_owner() round trip has no reply and the
        /// C code path would exit("Could not get selection owner").
        selection_owner: Cell<u32>,
    }

    impl XConn for NullConn {
        fn intern_atom(&mut self, _name: &str) -> Option<u32> {
            let id = self.next.get() + 100;
            self.next.set(id);
            Some(id)
        }

        /// C: xcb_set_selection_owner — remember the owner so that the
        /// verification round trip right after it succeeds.
        fn set_selection_owner(&mut self, owner: u32, _selection: u32, _timestamp: u32) {
            self.selection_owner.set(owner);
        }

        /// C: xcb_get_selection_owner.
        fn get_selection_owner(&mut self, _selection: u32) -> Option<u32> {
            Some(self.selection_owner.get())
        }

        fn generate_id(&mut self) -> u32 {
            let id = self.next.get() + 1;
            self.next.set(id);
            0x1000 + id
        }

        fn root_screen(&mut self) -> ScreenInfo {
            ScreenInfo {
                root: 0x50,
                width_in_pixels: 1920,
                height_in_pixels: 1080,
                root_depth: 24,
                root_visual: 0x21,
                default_colormap: 0x20,
                black_pixel: 0,
                white_pixel: 0xffffff,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{blockalign_t, status_block};
    use crate::config::{config_mut, reset_config};
    use crate::outputs::{init_outputs, parse_outputs_json};
    use i3_core::xcb_util::RecordingConn;
    use libi3::i3string_from_utf8;

    const OUTPUTS_JSON: &str = r#"[
        {"name":"eDP-1","active":true,"primary":true,"rect":{"x":0,"y":0,"width":800,"height":600}}
    ]"#;

    const WORKSPACES_JSON: &str = r#"[
        {"id":1234,"num":1,"name":"1","visible":true,"focused":true,"urgent":false,"output":"eDP-1"},
        {"id":5678,"num":2,"name":"2: www","visible":false,"focused":false,"urgent":false,"output":"eDP-1"}
    ]"#;

    fn reset_xcb_state() {
        reset_config();
        init_outputs();
        statusline::reset();
        crate::ipc::clear_sent_messages();
        MOD_PRESSED.with(|m| m.set(false));
        ACTIVATED_MODE.with(|a| a.set(false));
        BINDING.with(|b| *b.borrow_mut() = mode { name: None, name_width: 0 });
        BAR_HEIGHT.with(|h| h.set(0));
        ICON_SIZE.with(|i| i.set(0));
        SEPARATOR_SYMBOL_WIDTH.with(|w| w.set(0));
        FONT_HEIGHT.with(|h| h.set(0));
        SELWIN.with(|w| w.set(XCB_NONE));
        TRAY_REPLY.with(|a| a.set(XCB_NONE));
        OUTPUT_FOR_TRAY.with(|o| o.set(None));
        COLORS.with(|c| *c.borrow_mut() = xcb_colors_t::default());
    }

    fn block(full: &str, short: Option<&str>) -> status_block {
        status_block {
            full_text: Some(i3string_from_utf8(full)),
            short_text: short.map(i3string_from_utf8),
            ..Default::default()
        }
    }

    fn push_statusline(blocks: Vec<status_block>) {
        statusline::head(|head| {
            head.clear();
            head.extend(blocks);
        });
    }

    fn bar_setup() {
        reset_xcb_state();
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        crate::workspaces::parse_workspaces_json(WORKSPACES_JSON.as_bytes());
        // The tests act as if reconfig_windows() had already created the bar.
        crate::outputs::outputs(|outputs| {
            outputs[0].bar.id = 0x900;
            outputs[0].statusline_width = 200;
        });
    }

    // -- layout maths -------------------------------------------------------

    #[test]
    fn get_sep_offset_matches_c() {
        let mut b = block("x", None);
        b.sep_block_width = 9;
        assert_eq!(get_sep_offset(&b), 5); // 9/2 + 9%2
        b.sep_block_width = 10;
        assert_eq!(get_sep_offset(&b), 5);
        b.sep_block_width = 0;
        assert_eq!(get_sep_offset(&b), 0);
        b.sep_block_width = 9;
        b.no_separator = true;
        assert_eq!(get_sep_offset(&b), 0);
    }

    #[test]
    fn predict_block_length_min_width_align() {
        // "abcde" = 5 glyphs * MONOSPACE_ADVANCE
        let text_width = 5 * MONOSPACE_ADVANCE as u32;
        let padding = 100 - text_width;
        for (align, want_offset, want_append) in [
            (blockalign_t::ALIGN_LEFT, 0, padding),
            (blockalign_t::ALIGN_RIGHT, padding, 0),
            // C: x_offset = padding_width / 2; x_append = padding_width / 2 + padding_width % 2
            (blockalign_t::ALIGN_CENTER, padding / 2, padding / 2 + padding % 2),
        ] {
            let mut b = block("abcde", None);
            b.min_width = 100;
            b.align = align;
            predict_block_length(&mut b);
            assert_eq!(b.full_render.width, text_width);
            assert_eq!(b.full_render.x_offset, want_offset, "align {:?}", align);
            assert_eq!(b.full_render.x_append, want_append, "align {:?}", align);
            assert_eq!(b.render_length, 100);
        }
    }

    #[test]
    fn predict_block_length_center_odd_padding_goes_right() {
        // min_width - width is odd: x_offset gets the smaller half.
        let text_width = MONOSPACE_ADVANCE as u32; // "x"
        let mut b = block("x", None);
        b.min_width = text_width + 5;
        b.align = blockalign_t::ALIGN_CENTER;
        predict_block_length(&mut b);
        assert_eq!(b.full_render.x_offset, 2);
        assert_eq!(b.full_render.x_append, 3);
    }

    #[test]
    fn predict_block_length_with_border() {
        let mut b = block("x", None);
        b.border = Some("#ffffff".to_string());
        b.border_left = 2;
        b.border_right = 3;
        predict_block_length(&mut b);
        // render->width = text width + logical_px(border_left + border_right)
        assert_eq!(b.full_render.width, MONOSPACE_ADVANCE as u32 + 5);
    }

    #[test]
    fn predict_block_length_empty_text_is_zero() {
        let mut b = block("", None);
        predict_block_length(&mut b);
        assert_eq!(b.render_length, 0);
        assert_eq!(b.full_render.width, 0);
    }

    #[test]
    fn predict_statusline_length_adds_separators() {
        let mut a = block("abc", None);
        a.sep_block_width = 9;
        let mut b = block("de", None);
        b.sep_block_width = 9;
        let mut head = vec![a, b];
        // 3*7 = 21, + 9 separator, + 2*7 = 14
        assert_eq!(predict_statusline_length(&mut head), 21 + 9 + 14);
    }

    #[test]
    fn switch_block_to_short_returns_saved_width() {
        let mut b = block("abcdefghij", Some("ab"));
        predict_block_length(&mut b);
        assert_eq!(b.render_length, 10 * MONOSPACE_ADVANCE as u32);

        let saved = switch_block_to_short(&mut b);
        assert!(b.use_short);
        assert_eq!(b.render_length, 2 * MONOSPACE_ADVANCE as u32);
        assert_eq!(saved, (10 - 2) * MONOSPACE_ADVANCE as u32);

        // Already short: no further saving (C: if (.. || block->use_short) return 0)
        assert_eq!(switch_block_to_short(&mut b), 0);

        // No short text at all.
        let mut c = block("xxxx", None);
        predict_block_length(&mut c);
        assert_eq!(switch_block_to_short(&mut c), 0);
        assert!(!c.use_short);
    }

    #[test]
    fn adjust_statusline_length_switches_blocks_progressively() {
        let mut head = vec![
            block("aaaaaaaaaa", Some("a")),
            block("bbbbbbbbbb", Some("b")),
        ];
        // 2 * 10 * 7 (the blocks carry no separator width here: sep_block_width
        // is only set by the stdin parser, see new_block_defaults()).
        let width = adjust_statusline_length(&mut head, 1000);
        assert_eq!(width, 2 * 10 * MONOSPACE_ADVANCE as u32);
        assert!(!head[0].use_short);

        // With a max width below the full length, the first block is shortened
        // and the loop stops as soon as the width fits.
        let width = adjust_statusline_length(&mut head, 100);
        assert!(head[0].use_short);
        assert!(!head[1].use_short);
        assert_eq!(width, MONOSPACE_ADVANCE as u32 + 10 * MONOSPACE_ADVANCE as u32);

        // A very small max width shortens both blocks.
        let width = adjust_statusline_length(&mut head, 1);
        assert!(head[1].use_short);
        assert_eq!(width, 2 * MONOSPACE_ADVANCE as u32);
    }

    #[test]
    fn adjust_statusline_length_shortens_same_name_blocks_together() {
        // One logical block represented by two JSON blocks with the same name.
        let mut first = block("aaaaaaaaaa", Some("a"));
        first.name = Some("logical".to_string());
        let mut second_other = block("yyyyyyyyyy", Some("y"));
        second_other.name = Some("other".to_string());
        let mut third = block("aaaaaaaaaa", Some("a"));
        third.name = Some("logical".to_string());

        let mut head = vec![first, second_other, third];
        // max fits everything except the first two long blocks: shortening the
        // first block also shortens its namesake.
        let _ = adjust_statusline_length(&mut head, 200);
        assert!(head[0].use_short, "first logical block shortened");
        assert!(head[2].use_short, "namesake shortened with it");
    }

    #[test]
    fn predict_button_width_uses_ws_min_width() {
        assert_eq!(predict_button_width(20), 20 + 2 * ws_hoff_px + 2 * 1);
        config_mut(|c| c.ws_min_width = 200);
        assert_eq!(predict_button_width(20), 200);
    }

    #[test]
    fn quote_workspace_name_escapes_quotes_and_backslashes() {
        assert_eq!(quote_workspace_name("1"), "workspace \"1\"");
        assert_eq!(quote_workspace_name("a\"b"), "workspace \"a\\\"b\"");
        assert_eq!(quote_workspace_name("a\\b"), "workspace \"a\\\\b\"");
    }

    #[test]
    fn get_tray_width_counts_only_mapped_clients() {
        reset_xcb_state();
        config_mut(|c| c.tray_padding = 2);
        init_xcb_late(None, &mut test_support::NullConn::default());
        let icon = icon_size();
        assert!(icon > 0);

        let clients = vec![
            trayclient { win: 1, mapped: true, ..Default::default() },
            trayclient { win: 2, mapped: false, ..Default::default() },
            trayclient { win: 3, mapped: true, ..Default::default() },
        ];
        // 2 mapped clients * (icon_size + tray_padding) + tray_loff_px
        assert_eq!(get_tray_width(&clients), 2 * (icon + 2) + tray_loff_px);
        assert_eq!(get_tray_width(&[]), 0);
    }

    #[test]
    fn init_xcb_late_geometry_from_font_and_padding() {
        reset_xcb_state();
        init_xcb_late(None, &mut test_support::NullConn::default());
        // default font height 13 + 2 * ws_voff_px
        assert_eq!(bar_height(), 13 + 2 * ws_voff_px);
        assert_eq!(icon_size(), bar_height());

        reset_xcb_state();
        config_mut(|c| c.bar_height = 30);
        init_xcb_late(None, &mut test_support::NullConn::default());
        assert_eq!(bar_height(), 30);

        reset_xcb_state();
        config_mut(|c| {
            c.bar_height = 30;
            c.padding.y = 2;
        });
        init_xcb_late(None, &mut test_support::NullConn::default());
        // padding wins over bar_height (C: else branch)
        assert_eq!(bar_height(), 19 + 2);

        reset_xcb_state();
        config_mut(|c| c.tray_padding = 3);
        init_xcb_late(None, &mut test_support::NullConn::default());
        assert_eq!(icon_size(), bar_height() - 2 * 3);
    }

    #[test]
    fn init_xcb_late_measures_separator_symbol() {
        reset_xcb_state();
        config_mut(|c| c.separator_symbol = Some(i3string_from_utf8("|")));
        init_xcb_late(None, &mut test_support::NullConn::default());
        assert_eq!(separator_symbol_width(), MONOSPACE_ADVANCE);
    }

    // -- drawing commands ---------------------------------------------------

    #[test]
    fn draw_bars_records_workspace_buttons_and_statusline() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        push_statusline(vec![block("hello", None)]);

        let mut draw = RecordingBackend::new();
        let mut x = test_support::NullConn::default();
        draw_bars(&mut x, &mut draw, false);

        // The backbuffer is cleared first.
        assert!(matches!(
            draw.cmds[0],
            DrawCmd::ClearSurface { surface: surface_id::BUFFER, .. }
        ));

        // Two workspace buttons ("1" and "2: www") + the statusline text.
        let texts = draw.texts();
        assert!(texts.contains(&"1".to_string()), "texts: {:?}", texts);
        assert!(texts.contains(&"2: www".to_string()));
        assert!(texts.contains(&"hello".to_string()));

        // The statusline buffer is copied into the backbuffer and then the
        // backbuffer into the bar window.
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::CopySurface { src: surface_id::STATUSLINE_BUFFER, dst: surface_id::BUFFER, .. }
        )));
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::CopySurface { src: surface_id::BUFFER, dst: surface_id::BAR, .. }
        )));
    }

    #[test]
    fn draw_bars_button_colors_follow_state() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());

        let mut draw = RecordingBackend::new();
        let mut x = test_support::NullConn::default();
        draw_bars(&mut x, &mut draw, false);

        let focused_border = colors(|c| c.focus_ws_border);
        let inactive_border = colors(|c| c.inactive_ws_border);
        let borders: Vec<color_t> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Rectangle { surface: surface_id::BUFFER, color, .. } => Some(*color),
                _ => None,
            })
            .collect();
        assert!(borders.contains(&focused_border), "focused ws uses focus colors");
        assert!(borders.contains(&inactive_border), "invisible ws uses inactive colors");
    }

    #[test]
    fn draw_bars_urges_unhide() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        crate::outputs::outputs(|outputs| outputs[0].workspaces[0].urgent = true);

        let mut x = RecordingConn::new(0x50);
        let mut draw = RecordingBackend::new();
        config_mut(|c| c.hide_on_modifier = bar_display_mode_t::M_HIDE);
        draw_bars(&mut x, &mut draw, false);
        // unhide_bars() reconfigures and maps the bar.
        assert!(x.log.iter().any(|l| l.starts_with("map_window(0x900)")));
        assert!(x.log.iter().any(|l| l.starts_with("configure_window(0x900")));
    }

    #[test]
    fn draw_bars_hides_bars_in_hide_mode() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        config_mut(|c| c.hide_on_modifier = bar_display_mode_t::M_HIDE);

        let mut x = RecordingConn::new(0x50);
        let mut draw = RecordingBackend::new();
        draw_bars(&mut x, &mut draw, false);
        assert!(x.log.iter().any(|l| l.starts_with("unmap_window(0x900)")), "log: {:?}", x.log);
    }

    #[test]
    fn draw_bars_skips_inactive_outputs() {
        reset_xcb_state();
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        crate::outputs::outputs(|outputs| {
            outputs[0].active = false;
            outputs[0].bar.id = 0x900;
        });
        push_statusline(vec![block("hello", None)]);

        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        draw_bars(&mut x, &mut draw, false);
        assert!(draw.cmds.is_empty(), "cmds: {:?}", draw.cmds);
    }

    #[test]
    fn draw_statusline_clips_from_the_left() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        // One block wider than the available space.
        push_statusline(vec![block(&"x".repeat(200), None)]);

        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        draw_bars(&mut x, &mut draw, false);

        let copied = draw.cmds.iter().find_map(|c| match c {
            DrawCmd::CopySurface { src: surface_id::STATUSLINE_BUFFER, w, .. } => Some(*w),
            _ => None,
        });
        // The visible part never exceeds the bar width.
        assert!(copied.unwrap_or(0) <= 800);
        // A left clip means the text starts at a wrapped (negative as i32) x.
        let text_x = draw.cmds.iter().find_map(|c| match c {
            DrawCmd::Text { surface: surface_id::STATUSLINE_BUFFER, x, .. } => Some(*x),
            _ => None,
        });
        assert!(text_x.unwrap_or(0) < 0, "clipped text is drawn off-screen: {:?}", text_x);
    }

    #[test]
    fn draw_statusline_uses_short_text_when_switched() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        let mut b = block(&"x".repeat(200), Some("short"));
        b.name = Some("n".to_string());
        push_statusline(vec![b]);

        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        draw_bars(&mut x, &mut draw, false);
        assert!(draw.texts().contains(&"short".to_string()));
    }

    #[test]
    fn statusline_shorts_are_reset_between_outputs() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        statusline::head(|head| {
            let mut b = block("aaaa", Some("a"));
            b.use_short = true;
            head.clear();
            head.push(b);
        });

        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        draw_bars(&mut x, &mut draw, false);
        // The long form fits, so after the reset the full text is drawn again.
        assert!(draw.texts().contains(&"aaaa".to_string()), "texts: {:?}", draw.texts());
    }

    #[test]
    fn binding_mode_indicator_is_drawn() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        let mut m = mode { name: Some(i3string_from_utf8("resize")), name_width: 6 * MONOSPACE_ADVANCE };
        set_current_mode(&mut m);
        assert!(activated_mode());

        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        draw_bars(&mut x, &mut draw, false);
        assert!(draw.texts().contains(&"resize".to_string()));

        // config.disable_binding_mode_indicator hides it again.
        config_mut(|c| c.disable_binding_mode_indicator = true);
        let mut draw2 = RecordingBackend::new();
        draw_bars(&mut x, &mut draw2, false);
        assert!(!draw2.texts().contains(&"resize".to_string()));
    }

    #[test]
    fn workspace_buttons_can_be_disabled() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        config_mut(|c| c.disable_ws = true);

        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        draw_bars(&mut x, &mut draw, false);
        assert!(!draw.texts().contains(&"1".to_string()));
    }

    #[test]
    fn custom_separator_is_drawn() {
        bar_setup();
        config_mut(|c| c.separator_symbol = Some(i3string_from_utf8("|")));
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        // sep_block_width is set per block by the stdin parser; the separator
        // is only drawn when the block reserves space for it.
        let mut a = block("a", None);
        a.sep_block_width = 9;
        let mut b = block("b", None);
        b.sep_block_width = 9;
        push_statusline(vec![a, b]);

        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        draw_bars(&mut x, &mut draw, false);
        assert!(draw.texts().contains(&"|".to_string()), "texts: {:?}", draw.texts());
    }

    // -- clicks -------------------------------------------------------------

    #[test]
    fn handle_button_focuses_workspace_by_id() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());

        let event = xcb_button_press_event_t {
            response_type: 4, // XCB_BUTTON_PRESS
            event: 0x900,
            root_x: 10,
            root_y: 5,
            event_x: 5,
            event_y: 3,
            state: 0,
            detail: 1,
        };
        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        handle_button(&mut x, &mut draw, &event);

        let sent = crate::ipc::sent_messages();
        assert_eq!(
            sent,
            vec![(0, Some("[con_id=1234] focus workspace".to_string()))]
        );
    }

    #[test]
    fn handle_button_focuses_workspace_by_name_without_id() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        crate::outputs::outputs(|outputs| outputs[0].workspaces[0].id = 0);

        let event = xcb_button_press_event_t {
            response_type: 4,
            event: 0x900,
            root_x: 10,
            root_y: 5,
            event_x: 5,
            event_y: 3,
            state: 0,
            detail: 1,
        };
        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        handle_button(&mut x, &mut draw, &event);

        assert_eq!(
            crate::ipc::sent_messages(),
            vec![(0, Some("workspace \"1\"".to_string()))]
        );
    }

    #[test]
    fn handle_button_scrolls_to_previous_workspace() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        // Focus the second workspace so that scrolling up has a target.
        crate::outputs::outputs(|outputs| {
            outputs[0].workspaces[0].visible = false;
            outputs[0].workspaces[0].focused = false;
            outputs[0].workspaces[1].visible = true;
            outputs[0].workspaces[1].focused = true;
        });

        let event = xcb_button_press_event_t {
            response_type: 4,
            event: 0x900,
            root_x: 10,
            root_y: 5,
            event_x: 5,
            event_y: 3,
            state: 0,
            detail: XCB_BUTTON_SCROLL_UP,
        };
        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        handle_button(&mut x, &mut draw, &event);
        assert_eq!(
            crate::ipc::sent_messages(),
            vec![(0, Some("[con_id=1234] focus workspace".to_string()))]
        );
    }

    #[test]
    fn handle_button_custom_command_wins() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        config_mut(|c| {
            c.bindings = vec![crate::config::binding_t {
                input_code: 1,
                command: Some("my command".to_string()),
                release: false,
            }]
        });

        let event = xcb_button_press_event_t {
            response_type: 4,
            event: 0x900,
            root_x: 10,
            root_y: 5,
            event_x: 5,
            event_y: 3,
            state: 0,
            detail: 1,
        };
        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        handle_button(&mut x, &mut draw, &event);
        assert_eq!(
            crate::ipc::sent_messages(),
            vec![(0, Some("my command".to_string()))]
        );
    }

    #[test]
    fn handle_button_click_in_padding_is_ignored() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        config_mut(|c| c.padding.x = 10);

        let event = xcb_button_press_event_t {
            response_type: 4,
            event: 0x900,
            event_x: 5,
            ..Default::default()
        };
        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        handle_button(&mut x, &mut draw, &event);
        assert!(crate::ipc::sent_messages().is_empty());
    }

    #[test]
    fn handle_button_unknown_bar_is_ignored() {
        bar_setup();
        let event = xcb_button_press_event_t { event: 0x1234, ..Default::default() };
        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        handle_button(&mut x, &mut draw, &event);
        assert!(crate::ipc::sent_messages().is_empty());
    }

    // -- tray ---------------------------------------------------------------

    #[test]
    fn configure_trayclients_sorts_by_wm_class() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        crate::outputs::outputs(|outputs| {
            outputs[0].trayclients = vec![
                trayclient {
                    win: 0x11,
                    mapped: true,
                    class_class: Some("Zzz".to_string()),
                    ..Default::default()
                },
                trayclient {
                    win: 0x12,
                    mapped: true,
                    class_class: Some("Aaa".to_string()),
                    ..Default::default()
                },
                trayclient {
                    win: 0x13,
                    mapped: false,
                    class_class: Some("Mmm".to_string()),
                    ..Default::default()
                },
            ];
        });

        let mut x = RecordingConn::new(0x50);
        configure_trayclients(&mut x);

        // Sorted ascending by class: Aaa, Zzz. The loop walks them in reverse so
        // that "Aaa" ends up leftmost.
        let expected = {
            let step = icon_size() + 0;
            vec![
                format!("configure_window(0x11, [(X, {})])", 800 - step),
                format!("configure_window(0x12, [(X, {})])", 800 - 2 * step),
            ]
        };
        assert_eq!(x.log, expected);
    }

    #[test]
    fn trayclient_update_class_parses_wm_class() {
        bar_setup();
        let mut x = RecordingConn::new(0x50);
        // WM_CLASS is two NUL-terminated strings: instance, then class.
        let mut value = b"instance\0TheClass\0".to_vec();
        value.truncate(value.len());
        x.props.insert(
            (0x11, XCB_ATOM_WM_CLASS),
            i3_core::backend::PropertyReply {
                type_: XCB_ATOM_STRING,
                format: 8,
                value,
            },
        );
        let mut client = trayclient { win: 0x11, ..Default::default() };
        trayclient_update_class(&mut x, &mut client);
        assert_eq!(client.class_instance.as_deref(), Some("instance"));
        assert_eq!(client.class_class.as_deref(), Some("TheClass"));
    }

    #[test]
    fn trayclient_lookup_only_considers_active_outputs() {
        bar_setup();
        crate::outputs::outputs(|outputs| {
            outputs[0].trayclients = vec![trayclient { win: 0x11, mapped: true, ..Default::default() }];
        });
        assert!(trayclient_from_window(0x11).is_some());

        crate::outputs::outputs(|outputs| outputs[0].active = false);
        assert!(trayclient_from_window(0x11).is_none());
    }

    #[test]
    fn get_tray_output_selects_first_active_output() {
        bar_setup();
        let ptr = crate::outputs::outputs(|outputs| outputs[0].ptr_id);
        assert_eq!(get_tray_output(), Some(ptr));

        config_mut(|c| c.tray_outputs = vec![crate::config::tray_output_t { output: "none".to_string() }]);
        assert_eq!(get_tray_output(), None);

        config_mut(|c| c.tray_outputs = vec![crate::config::tray_output_t { output: "eDP-1".to_string() }]);
        assert_eq!(get_tray_output(), Some(ptr));

        config_mut(|c| c.tray_outputs = vec![crate::config::tray_output_t { output: "primary".to_string() }]);
        assert_eq!(get_tray_output(), Some(ptr));

        config_mut(|c| c.tray_outputs = vec![crate::config::tray_output_t { output: "HDMI-9".to_string() }]);
        assert_eq!(get_tray_output(), None);
    }

    // -- tray selection -----------------------------------------------------

    #[test]
    fn recording_conn_models_selection_ownership() {
        // C: the xcb_set_selection_owner / xcb_get_selection_owner round trip.
        // The test double keeps the ownership table a real server would hold.
        let mut x = RecordingConn::new(0x50);
        assert_eq!(x.get_selection_owner(0x1234), Some(0)); // unowned
        x.set_selection_owner(0xabc, 0x1234, XCB_CURRENT_TIME);
        assert_eq!(x.get_selection_owner(0x1234), Some(0xabc));
        // XCB_NONE releases the selection again.
        x.set_selection_owner(XCB_NONE, 0x1234, XCB_CURRENT_TIME);
        assert_eq!(x.get_selection_owner(0x1234), Some(0));
        assert_eq!(
            x.log,
            vec![
                "get_selection_owner(selection=4660)".to_string(),
                "set_selection_owner(owner=0xabc, selection=4660, ts=0)".to_string(),
                "get_selection_owner(selection=4660)".to_string(),
                "set_selection_owner(owner=0x0, selection=4660, ts=0)".to_string(),
                "get_selection_owner(selection=4660)".to_string(),
            ]
        );
    }

    // -- transparency (-t) --------------------------------------------------

    #[test]
    fn init_xcb_early_takes_the_argb32_visual_for_transparency() {
        // C: xcb.c:1352-1367 — with -t i3bar looks for a depth-32 visual and
        // switches depth, visual and colormap to it, so the bar window (and the
        // tray selection window) can carry alpha.
        reset_xcb_state();
        config_mut(|c| c.transparency = true);
        let mut x = RecordingConn::new(0x50);
        // The screen offers the ARGB32 visual at depth 32.
        x.visual_by_depth = Some((0x60, 32));
        x.log.clear();

        init_xcb_early(&mut x);

        // C: colormap = xcb_generate_id(xcb_connection);
        //    xcb_create_colormap_checked(XCB_COLORMAP_ALLOC_NONE, colormap,
        //                                xcb_root, visual_type->visual_id);
        assert!(
            x.log
                .iter()
                .any(|l| l == "create_colormap(0x1, win=0x50, visual=0x60)"),
            "log: {:?}",
            x.log
        );
        // C: depth = xcb_aux_get_depth_of_visual(root_screen, visual_id);
        assert_eq!(DEPTH.with(|d| d.get()), 32);
        // C: visual_type = xcb_aux_find_visual_by_attrs(root_screen, -1, 32);
        assert_eq!(VISUAL_ID.with(|v| v.get()), 0x60);
        assert_eq!(COLORMAP.with(|c| c.get()), 0x1);
        // The drawing surfaces follow the window depth (C: the global depth).
        assert_eq!(crate::live::depth(), 32);
    }

    #[test]
    fn init_xcb_early_stays_opaque_without_a_32bit_visual() {
        // C: when xcb_aux_find_visual_by_attrs() returns NULL the bar keeps the
        // root depth, the default colormap and the root visual (the C code has
        // no error message on this path, only the debug log).
        reset_xcb_state();
        config_mut(|c| c.transparency = true);
        let mut x = RecordingConn::new(0x50);
        x.log.clear();

        init_xcb_early(&mut x);

        assert!(
            x.log.iter().any(|l| l == "find_visual_by_depth(32)"),
            "log: {:?}",
            x.log
        );
        assert!(
            !x.log.iter().any(|l| l.starts_with("create_colormap(")),
            "log: {:?}",
            x.log
        );
        // The RecordingConn screen: root depth 24, visual 0x21, colormap 0x20.
        assert_eq!(DEPTH.with(|d| d.get()), 24);
        assert_eq!(VISUAL_ID.with(|v| v.get()), 0x21);
        assert_eq!(COLORMAP.with(|c| c.get()), 0x20);
        assert_eq!(crate::live::depth(), 24);
    }

    #[test]
    fn init_tray_acquires_the_selection_and_announces_the_manager() {
        reset_xcb_state();
        let mut x = RecordingConn::new(0x50);
        init_xcb_early(&mut x); // DEPTH/COLORMAP/VISUAL_ID + the atom table
        x.log.clear();

        init_tray(&mut x);

        let selwin = SELWIN.with(|w| w.get());
        assert_ne!(selwin, XCB_NONE);
        let tray_atom = TRAY_REPLY.with(|a| a.get());
        // C: "_NET_SYSTEM_TRAY_S%d" for this screen (0 in the tests).
        assert_eq!(x.atoms.get("_NET_SYSTEM_TRAY_S0").copied(), Some(tray_atom));

        // C: xcb_set_selection_owner(selwin, tray_reply->atom, XCB_CURRENT_TIME)
        assert!(
            x.log.contains(&format!(
                "set_selection_owner(owner={:#x}, selection={}, ts=0)",
                selwin, tray_atom
            )),
            "log: {:?}",
            x.log
        );
        // C: the get_selection_owner() round trip found us as the owner, so the
        // MANAGER ClientMessage goes out on the root window.
        assert!(
            x.log
                .contains(&format!("get_selection_owner(selection={})", tray_atom)),
            "log: {:?}",
            x.log
        );
        let atoms = ATOMS.with(|a| *a.borrow());
        assert!(
            x.log.contains(&format!(
                "send_client_message(win=0x50, type={}, mask=0xffffff, propagate=false, data32=[0, {}, {}, 0, 0])",
                atoms[atom::MANAGER], tray_atom, selwin
            )),
            "log: {:?}",
            x.log
        );
    }

    #[test]
    fn init_tray_gives_up_when_another_tray_owns_the_selection() {
        reset_xcb_state();
        let mut x = RecordingConn::new(0x50);
        init_xcb_early(&mut x);
        // Another tray manager already holds _NET_SYSTEM_TRAY_S0.
        x.selection_owner_reply = Some(0x7777);
        x.log.clear();

        init_tray(&mut x);

        let selwin = SELWIN.with(|w| w.get());
        // We asked for the selection ...
        assert!(
            x.log.contains(&format!(
                "set_selection_owner(owner={:#x}, selection={}, ts=0)",
                selwin,
                TRAY_REPLY.with(|a| a.get())
            )),
            "log: {:?}",
            x.log
        );
        assert!(x.log.iter().any(|l| l.starts_with("get_selection_owner(")));
        // ... but the reply named the other tray, so no MANAGER message is sent
        // and tray support is disabled (C: "Maybe another tray is already
        // running?", then return). selwin stays set, exactly like C, which is
        // what keeps reconfig_windows() from retrying init_tray().
        assert_ne!(selwin, XCB_NONE);
        assert!(
            !x.log.iter().any(|l| l.starts_with("send_client_message(")),
            "log: {:?}",
            x.log
        );
    }

    #[test]
    fn dock_request_reparents_and_registers_the_client() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        let mut conn = test_support::NullConn::default();
        init_xcb_early(&mut conn); // interns the atoms
        init_colors(&xcb_color_strings_t::default(), &mut conn);
        let output_ptr = crate::outputs::outputs(|outputs| outputs[0].ptr_id);
        OUTPUT_FOR_TRAY.with(|o| o.set(Some(output_ptr)));

        let atoms = ATOMS.with(|a| *a.borrow());
        let mut x = RecordingConn::new(0x50);
        let mut draw = RecordingBackend::new();
        let event = xcb_client_message_event_t {
            window: 0x50,
            r#type: atoms[atom::_NET_SYSTEM_TRAY_OPCODE],
            format: 32,
            data32: [0, SYSTEM_TRAY_REQUEST_DOCK, 0x77, 0, 0],
        };
        handle_client_message(&mut x, &mut draw, &event);

        assert!(x.log.iter().any(|l| l.starts_with("change_window_attributes(0x77")), "log: {:?}", x.log);
        assert!(x.log.iter().any(|l| l.starts_with("reparent_window(0x77 -> 0x900")), "log: {:?}", x.log);
        assert!(x.log.iter().any(|l| l.starts_with("configure_window(0x77")), "log: {:?}", x.log);
        assert!(x.log.iter().any(|l| l.starts_with("send_client_message(win=0x77")), "log: {:?}", x.log);
        assert!(x.log.iter().any(|l| l.starts_with("map_window(0x77)")), "log: {:?}", x.log);
        assert_eq!(crate::outputs::outputs(|o| o[0].trayclients.len()), 1);
    }

    #[test]
    fn destroy_notify_removes_the_tray_client() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        crate::outputs::outputs(|outputs| {
            outputs[0].trayclients = vec![trayclient { win: 0x11, mapped: true, ..Default::default() }];
        });
        let mut x = RecordingConn::new(0x50);
        let mut draw = RecordingBackend::new();
        handle_destroy_notify(&mut x, &mut draw, 0x11);
        assert!(crate::outputs::outputs(|o| o[0].trayclients.is_empty()));
    }

    #[test]
    fn map_and_unmap_notify_toggle_mapped() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        crate::outputs::outputs(|outputs| {
            outputs[0].trayclients = vec![trayclient { win: 0x11, mapped: false, ..Default::default() }];
        });
        let mut x = RecordingConn::new(0x50);
        let mut draw = RecordingBackend::new();

        handle_map_notify(&mut x, &mut draw, 0x11);
        assert!(crate::outputs::outputs(|o| o[0].trayclients[0].mapped));

        handle_unmap_notify(&mut x, &mut draw, 0x11);
        assert!(!crate::outputs::outputs(|o| o[0].trayclients[0].mapped));
    }

    #[test]
    fn kick_tray_clients_reparents_to_root() {
        bar_setup();
        crate::outputs::outputs(|outputs| {
            outputs[0].trayclients = vec![trayclient { win: 0x11, mapped: true, ..Default::default() }];
        });
        let mut x = RecordingConn::new(0x50);
        kick_tray_clients(&mut x, 0);
        assert!(x.log.iter().any(|l| l.starts_with("unmap_window(0x11)")));
        assert!(x.log.iter().any(|l| l.starts_with("reparent_window(0x11 -> 0x50")));
        assert!(crate::outputs::outputs(|o| o[0].trayclients.is_empty()));
    }

    // -- xkb ----------------------------------------------------------------

    #[test]
    fn xkb_modifier_press_unhides_and_release_hides() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        config_mut(|c| {
            c.hide_on_modifier = bar_display_mode_t::M_HIDE;
            c.modifier = crate::config::XCB_MOD_MASK_4;
        });

        let mut x = RecordingConn::new(0x50);
        // Mod4 pressed: unhide_bars() maps the bar.
        assert!(xkb_state_notify(&mut x, XCB_XKB_STATE_NOTIFY, crate::config::XCB_MOD_MASK_4 as u16));
        assert!(mod_pressed());
        assert!(x.log.iter().any(|l| l.starts_with("map_window(0x900)")));

        x.log.clear();
        // Mod4 released: hide_bars() unmaps it again.
        assert!(xkb_state_notify(&mut x, XCB_XKB_STATE_NOTIFY, 0));
        assert!(!mod_pressed());
        assert!(x.log.iter().any(|l| l.starts_with("unmap_window(0x900)")));
    }

    #[test]
    fn xkb_release_does_not_hide_while_a_mode_is_activated() {
        // C: activated_mode is cleared when the modifier goes down, not on
        // release; a release with a mode active leaves the bar visible.
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        config_mut(|c| {
            c.hide_on_modifier = bar_display_mode_t::M_HIDE;
            c.modifier = crate::config::XCB_MOD_MASK_4;
        });
        MOD_PRESSED.with(|m| m.set(true));
        ACTIVATED_MODE.with(|a| a.set(true));

        let mut x = RecordingConn::new(0x50);
        xkb_state_notify(&mut x, XCB_XKB_STATE_NOTIFY, 0);
        assert!(!mod_pressed());
        // C: else if (!activated_mode) hide_bars();
        assert!(!x.log.iter().any(|l| l.starts_with("unmap_window(0x900)")));
        assert!(activated_mode(), "activated_mode is only reset on press");
    }

    #[test]
    fn xkb_ignores_events_when_no_modifier_is_configured() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        config_mut(|c| c.modifier = XCB_NONE);

        let mut x = RecordingConn::new(0x50);
        assert!(!xkb_state_notify(&mut x, XCB_XKB_STATE_NOTIFY, 0));
        assert!(!mod_pressed());
    }

    // -- window setup -------------------------------------------------------

    #[test]
    fn reconfig_windows_creates_bar_and_properties() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());
        crate::outputs::outputs(|outputs| outputs[0].bar.id = XCB_NONE);

        let mut x = RecordingConn::new(0x50);
        reconfig_windows(&mut x, false);

        assert!(x.log.iter().any(|l| l.starts_with("create_window(")), "log: {:?}", x.log);
        // WM_CLASS, WM_NAME, _NET_WM_WINDOW_TYPE and _NET_WM_STRUT_PARTIAL
        let bar_id = crate::outputs::outputs(|o| o[0].bar.id);
        let bar_props = format!("change_property(win={:#x}", bar_id);
        assert_eq!(
            x.log.iter().filter(|l| l.starts_with(&bar_props)).count(),
            4,
            "log: {:?}",
            x.log
        );
        // M_DOCK maps the bar.
        assert!(x.log.iter().any(|l| l.starts_with("map_window(")));
        assert_ne!(crate::outputs::outputs(|o| o[0].bar.id), XCB_NONE);
    }

    #[test]
    fn reconfig_windows_destroys_inactive_output_bars() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        crate::outputs::outputs(|outputs| outputs[0].active = false);

        let mut x = RecordingConn::new(0x50);
        reconfig_windows(&mut x, false);
        assert!(x.log.iter().any(|l| l.starts_with("destroy_window(0x900)")), "log: {:?}", x.log);
        assert_eq!(crate::outputs::outputs(|o| o[0].bar.id), XCB_NONE);
    }

    #[test]
    fn config_strut_partial_top_and_bottom() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        let rect = crate::outputs::outputs(|o| o[0].rect);

        config_mut(|c| c.position = crate::config::position_t::POS_TOP);
        let bytes = crate::outputs::outputs(|o| config_strut_partial(&o[0]));
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(words[2], bar_height() as u32); // top
        assert_eq!(words[8], rect.x as u32); // top_start_x
        assert_eq!(words[9], (rect.x + rect.w) as u32); // top_end_x

        config_mut(|c| c.position = crate::config::position_t::POS_BOT);
        let bytes = crate::outputs::outputs(|o| config_strut_partial(&o[0]));
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(words[3], bar_height() as u32); // bottom
        assert_eq!(words[10], rect.x as u32); // bottom_start_x
        assert_eq!(words[11], (rect.x + rect.w) as u32); // bottom_end_x
    }

    #[test]
    fn init_xcb_early_interns_atoms_and_reads_the_socket_path() {
        reset_xcb_state();
        let mut conn = RecordingConn::new(0x50);
        // Provide the I3_SOCKET_PATH root property.
        let atom = conn.intern_named("I3_SOCKET_PATH");
        conn.props.insert(
            (0x50, atom),
            i3_core::backend::PropertyReply {
                type_: XCB_ATOM_STRING,
                format: 8,
                value: b"/run/user/1000/i3/ipc-socket.1\0".to_vec(),
            },
        );
        let path = init_xcb_early(&mut conn);
        assert_eq!(path.as_deref(), Some("/run/user/1000/i3/ipc-socket.1"));
        // Every atom of xcb_atoms.def was interned.
        let atoms = ATOMS.with(|a| *a.borrow());
        assert!(atoms.iter().all(|&a| a != 0));
    }

    #[test]
    fn visibility_notify_stops_children_when_nothing_is_visible() {
        bar_setup();
        init_xcb_late(None, &mut test_support::NullConn::default());
        crate::outputs::outputs(|outputs| outputs[0].visible = true);

        // The bar gets fully obscured -> no visible output left.
        handle_visibility_notify(0x900, XCB_VISIBILITY_FULLY_OBSCURED);
        assert!(!crate::outputs::outputs(|o| o[0].visible));

        handle_visibility_notify(0x900, 0);
        assert!(crate::outputs::outputs(|o| o[0].visible));
    }

    #[test]
    fn xcb_prep_cb_dispatches_expose_to_redraw() {
        bar_setup();
        init_colors(&xcb_color_strings_t::default(), &mut test_support::NullConn::default());
        init_xcb_late(None, &mut test_support::NullConn::default());

        let mut x = test_support::NullConn::default();
        let mut draw = RecordingBackend::new();
        xcb_prep_cb(&mut x, &mut draw, &[XEvent::Expose { window: 0x900, count: 0 }]);
        assert!(draw.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::CopySurface { src: surface_id::BUFFER, dst: surface_id::BAR, .. }
        )));
    }
}
