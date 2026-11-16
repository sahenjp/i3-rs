//! C: src/config.c — configuration file (calling the parser with the correct
//! path, switching key bindings mode) — plus the config file loading and
//! variable substitution runtime of src/config_parser.c (parse_file and its
//! helpers), which operates on the same parser_ctx as parse_config.
//!
//! The Configuration struct holds everything from include/configuration.h
//! that data.rs does not already carry for the tree layer (data.rs keeps a
//! subset struct Config; the complete one is Configuration here).
//!
//! X11-side side effects (key grabbing, i3-nagbar, X resource lookups, real
//! font metrics) are stubbed out with TODO(X11 port) notes, like the tree
//! layer does through the ConBackend.

// C identifiers are kept verbatim (enum values, struct names).
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

use std::collections::VecDeque;
use std::path::Path;

use i3_spec::config_parser::{
    free_variables, parse_config, ConfigParserCtx, ParseFileResult, ParserContext,
};
use libi3::draw_util::color_t;
use libi3::font::i3Font;
use libi3::get_config_path;

use crate::config_directives::{check_for_duplicate_bindings, ConfigDirectives};
use crate::data::{gaps_t, Match, Rect};
use crate::tree::Tree;

/// C: include/libi3.h — the cached font type. libi3/font.rs keeps the
/// numeric field without the constants, so they live here for now
/// (TODO(X11 port): move into libi3::font together with load_font).
pub const FONT_TYPE_NONE: i32 = 0;
/// C: include/libi3.h FONT_TYPE_XCB
pub const FONT_TYPE_XCB: i32 = 1;
/// C: include/libi3.h FONT_TYPE_PANGO
pub const FONT_TYPE_PANGO: i32 = 2;

/// C: libi3/font.c:159 load_font — string-keeping variant used while parsing:
/// the pattern and the pango/xcb classification are stored, the metrics need
/// Pango/X11. The caller that owns the connection loads the real font from
/// that pattern with libi3::font::load_font() through [apply_font] (C has the
/// connection as a global and does both inside CFGFUN(font)).
pub fn load_font(pattern: &str, fallback: bool) -> i3Font {
    let _ = fallback; // C: loads "fixed" when the pattern cannot be loaded
    let r#type = if pattern.starts_with("pango:") || pattern.starts_with("xft:") {
        FONT_TYPE_PANGO
    } else {
        FONT_TYPE_XCB
    };
    i3Font {
        r#type,
        height: 0,
        pattern: pattern.to_string(),
    }
}

/// C: src/config_directives.c:173 CFGFUN(font) + src/config.c:277-281 —
/// loads the configured font for real and installs it as the current font
/// (libi3/font.c:256 set_font), so every draw_text()/decoration uses it.
///
/// C does this inside the config parser because the xcb connection is a
/// process global; the port's parser runs without one (config validation with
/// -C has no X server at all), so the caller that owns the connection performs
/// the same two calls (load_font + set_font) as soon as
/// load_configuration() has returned — that is main.c:687 in the startup path
/// (server.rs) and cmd_reload (src/commands.c:1680, crate::reload).
///
/// `tree` receives the real font height, which render_deco_height()
/// (src/render.c:27) reads through the tree-layer config subset.
pub fn apply_font<C: x11rb::connection::Connection>(
    conn: Option<&C>,
    d: &mut ConfigDirectives,
    mut tree: Option<&mut Tree>,
) -> i3Font {
    let pattern = if d.config.font.pattern.is_empty() {
        // C: load_font("fixed", true) — same fallback as config.c:279.
        "fixed".to_string()
    } else {
        d.config.font.pattern.clone()
    };
    let font = libi3::font::load_font(conn, &pattern, true);
    libi3::font::set_font(&font);
    if let Some(t) = tree.as_deref_mut() {
        t.config.font_height = font.height.max(0) as u32;
    }
    d.config.font = font.clone();
    // C: src/config.c:285-291 — bar config blocks without a configured font use
    // the i3-wide font. In C this runs at the end of load_configuration(),
    // after CFGFUN(font) has already loaded the font (and its "fixed"
    // fallback) during parsing, so config.font.pattern already holds the
    // *resolved* pattern. The port defers the font load to here, so the
    // assignment runs here as well — otherwise the bar config would keep the
    // unresolvable pattern instead of "fixed" (testcases/t/317-*.t).
    apply_barconfig_font_default(d);
    font
}

/// C: src/config.c:285-291 — "Make bar config blocks without a configured font
/// use the i3-wide font."
pub fn apply_barconfig_font_default(d: &mut ConfigDirectives) {
    let font_pattern = d.config.font.pattern.clone();
    for current in &mut d.barconfigs {
        if current.font.is_none() {
            current.font = Some(font_pattern.clone());
        }
    }
}

/// Copies the part of the configuration that the tree/command layer reads
/// into the tree's config subset (C: there is no counterpart — src/con.c,
/// src/gaps.c, src/render.c and src/commands.c read the one global `config`).
///
/// The subset exists because the tree layer must not depend on the config
/// layer; it therefore has to be refreshed whenever a configuration is loaded,
/// i.e. at startup and on every reload (src/commands.c:1680).
pub fn apply_to_tree(t: &mut Tree, config: &Configuration) {
    let c = &mut t.config;
    c.default_border = config.default_border;
    c.default_floating_border = config.default_floating_border;
    c.default_border_width = config.default_border_width;
    c.default_floating_border_width = config.default_floating_border_width;
    c.default_orientation = config.default_orientation;
    c.default_layout = config.default_layout;
    c.focus_wrapping = config.focus_wrapping;
    c.disable_focus_follows_mouse = config.disable_focus_follows_mouse;
    // C: src/x.c:1553 reads the one global config.mouse_warping.
    c.mouse_warping = config.mouse_warping;
    c.hide_edge_borders = config.hide_edge_borders;
    c.smart_gaps = config.smart_gaps;
    c.gaps = config.gaps;
    // C: src/click.c route_click reads config.floating_modifier,
    // config.tiling_drag; src/tiling_drag.c reads config.swap_modifier; the
    // resize bar / drop indicator use config.client.focused colors.
    c.floating_modifier = config.floating_modifier;
    c.swap_modifier = config.swap_modifier;
    c.tiling_drag = config.tiling_drag;
    c.client_focused_indicator = config.client.focused.indicator.colorpixel;
    c.client_focused_border = config.client.focused.border.colorpixel;
    // C: src/handlers.c:1293 reads config.popup_during_fullscreen (one enum in
    // configuration.h). The port keeps one enum per layer and their
    // discriminants differ, so the value is mapped by name.
    c.popup_during_fullscreen = match config.popup_during_fullscreen {
        popup_during_fullscreen_t::PDF_IGNORE => crate::data::PopupDuringFullscreen::PDF_IGNORE,
        popup_during_fullscreen_t::PDF_SMART => crate::data::PopupDuringFullscreen::PDF_SMART,
        popup_during_fullscreen_t::PDF_ALL => crate::data::PopupDuringFullscreen::PDF_ALL,
        popup_during_fullscreen_t::PDF_LEAVE_FULLSCREEN => {
            crate::data::PopupDuringFullscreen::PDF_LEAVE_FULLSCREEN
        }
    };
    // C: src/commands.c:104/406 read config.workspace_auto_back_and_forth.
    c.workspace_auto_back_and_forth = config.workspace_auto_back_and_forth;
    // C: src/handlers.c:1289 reads config.focus_on_window_activation for
    // windows that "request focus" by stacking themselves above all others.
    c.focus_on_window_activation = match config.focus_on_window_activation {
        focus_on_window_activation_t::FOWA_SMART => crate::data::FocusOnWindowActivation::FOWA_SMART,
        focus_on_window_activation_t::FOWA_URGENT => crate::data::FocusOnWindowActivation::FOWA_URGENT,
        focus_on_window_activation_t::FOWA_FOCUS => crate::data::FocusOnWindowActivation::FOWA_FOCUS,
        focus_on_window_activation_t::FOWA_NONE => crate::data::FocusOnWindowActivation::FOWA_NONE,
    };
    // C: workspace_urgency_timer is a double; configuration.h keeps a float for
    // the config-layer copy, so the widening is explicit here.
    c.workspace_urgency_timer = config.workspace_urgency_timer as f64;
    // C: src/floating.c:185-229 reads config.floating_{minimum,maximum}_*.
    c.floating_minimum_width = config.floating_minimum_width;
    c.floating_minimum_height = config.floating_minimum_height;
    c.floating_maximum_width = config.floating_maximum_width;
    c.floating_maximum_height = config.floating_maximum_height;
    // C: src/render.c:27 render_deco_height() reads config.font.height.
    c.font_height = config.font.height.max(0) as u32;
}

/// C: libi3/dpi.c:85 logical_px — converts a logical amount of pixels to
/// physical pixels. Without an X server (root_screen == NULL, C comment:
/// "Dpi info may not be available when parsing a config without an X server,
/// such as for config file validation") C returns the logical value
/// unchanged — this port is always in that state (TODO(X11 port): wire to
/// libi3/dpi.rs when root_screen exists).
pub fn logical_px(logical: i32) -> i32 {
    logical
}

/// C: libi3/draw_util.c:163 draw_util_hex_to_color — parses a hex color
/// ("#rrggbb" or "#rrggbbaa") into a color_t, including the .colorpixel
/// member (libi3/get_colorpixel.c), which the XCB/pango text drawing uses.
/// This wrapper keeps the C-file reference for the config layer; the parse
/// itself is libi3's 1:1 port, so every `color`/`client.*`/`bar.*`
/// directive ends up with a real pixel value (no two colors collapse into
/// the same one).
// C: libi3/draw_util.c:163 computes alpha/channel for .colorpixel; the port
// delegates to libi3's 1:1 port of the same function, so the two locals below
// stay as documentation of the C computation only.
#[allow(unused_variables)]
pub fn draw_util_hex_to_color(color: &str) -> color_t {
    let bytes = color.as_bytes();
    if bytes.len() < 6 || bytes[0] != b'#' {
        // C: ELOG("Could not parse color: %s\n", color);
        crate::log::errorlog(&format!("Could not parse color: {}\n", color));
        return draw_util_hex_to_color("#A9A9A9");
    }
    // C: the alpha group is present only for "#rrggbbaa" (9 bytes).
    let alpha: &[u8] = if bytes.len() == 9 { &bytes[7..9] } else { b"FF" };
    // C: strtol(group, NULL, 16) / 255.0 per channel.
    let channel = |slice: &[u8]| -> f64 { hex_strtol(slice) as f64 / 255.0 };
    // C: .colorpixel = get_colorpixel(color) — the XCB text drawing needs the
    // pixel value, so the real libi3 implementation is used (libi3's
    // draw_util_hex_to_color is the 1:1 port of this function).
    libi3::draw_util::draw_util_hex_to_color(color)
}

/// C: strtol(groups, NULL, 16) on up to two hex digits (leading whitespace
/// and signs never occur in the color groups).
fn hex_strtol(slice: &[u8]) -> i64 {
    let mut value: i64 = 0;
    for &b in slice.iter().take(2) {
        let digit = match b {
            b'0'..=b'9' => (b - b'0') as i64,
            b'a'..=b'f' => (b - b'a' + 10) as i64,
            b'A'..=b'F' => (b - b'A' + 10) as i64,
            _ => break,
        };
        value = value * 16 + digit;
    }
    value
}

/// C: include/configuration.h:315 — mode enum inside struct Barconfig.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum bar_mode_t {
    #[default]
    M_DOCK = 0,
    M_HIDE = 1,
    M_INVISIBLE = 2,
}

/// C: include/configuration.h:320 — hidden_state enum inside struct Barconfig.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum bar_hidden_state_t {
    #[default]
    S_HIDE = 0,
    S_SHOW = 1,
}

/// C: include/configuration.h:329 — position enum inside struct Barconfig.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum bar_position_t {
    #[default]
    P_BOTTOM = 0,
    P_TOP = 1,
}

/// C: include/configuration.h:199-208 — focus_on_window_activation enum
/// (anonymous enum inside struct Config).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum focus_on_window_activation_t {
    /// Focus if the target workspace is visible, set urgency hint otherwise.
    #[default]
    FOWA_SMART = 0,
    /// Always set the urgency hint.
    FOWA_URGENT = 1,
    /// Always focus the window.
    FOWA_FOCUS = 2,
    /// Ignore the request (no focus, no urgency hint).
    FOWA_NONE = 3,
}

/// C: include/configuration.h:215-219 — title_align enum.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum title_align_t {
    #[default]
    ALIGN_LEFT = 0,
    ALIGN_CENTER = 1,
    ALIGN_RIGHT = 2,
}

/// C: include/configuration.h:257-271 — popup_during_fullscreen enum.
///
/// NOTE: data.rs keeps a PopupDuringFullscreen with different discriminants
/// for the tree layer; this port keeps the C values of the header.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum popup_during_fullscreen_t {
    /// Display (and focus) the popup when it belongs to the fullscreen
    /// window only.
    #[default]
    PDF_SMART = 0,
    /// Leave fullscreen mode unconditionally.
    PDF_LEAVE_FULLSCREEN = 1,
    /// Just ignore the popup, that is, don't map it.
    PDF_IGNORE = 2,
    /// Display all floating windows.
    PDF_ALL = 3,
}

/// C: include/tiling_drag.h:15 — tiling_drag_t.
///
/// Defined in data.rs because the tree-layer config subset (data::Config)
/// needs the same type for src/click.c's route_click().
pub use crate::data::tiling_drag_t;

/// C: include/configuration.h:436 — config_load_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum config_load_t {
    C_VALIDATE,
    C_LOAD,
    C_RELOAD,
}

/// C: include/configuration.h:54 — struct Colortriple. It makes sense to
/// group colors for background, border and text as every element in i3 has
/// them (window decorations, bar).
/// (No Debug/Clone derives on top: libi3's color_t carries neither.)
#[derive(Clone, Copy, Default)]
pub struct Colortriple {
    pub border: color_t,
    pub background: color_t,
    pub text: color_t,
    pub indicator: color_t,
    pub child_border: color_t,
}

/// C: include/configuration.h:79 — struct IncludedFile. List entry struct
/// for an included file (C: a TAILQ; a VecDeque here).
#[derive(Debug, Clone, Default)]
pub struct IncludedFile {
    pub path: String,
    pub raw_contents: String,
    pub variable_replaced_contents: String,
}

/// C: include/configuration.h:93 — struct Mode. The configuration file can
/// contain multiple sets of bindings. Apart from the default set
/// (name == "default"), you can specify other sets and change the currently
/// active set of bindings by using the "mode <name>" command.
#[derive(Debug, Clone, Default)]
pub struct Mode {
    pub name: String,
    pub pango_markup: bool,
    /// C: struct bindings_head *bindings (a TAILQ; a VecDeque here).
    pub bindings: VecDeque<crate::data::Binding>,
}

/// C: include/configuration.h:417 — struct Barbinding. Defines a mouse
/// command to be executed instead of the default behavior when clicking on
/// the non-statusline part of i3bar.
#[derive(Debug, Clone, Default)]
pub struct Barbinding {
    /// The button to be used (e.g., 1 for "button1").
    pub input_code: i32,
    /// The command which is to be executed for this button.
    pub command: String,
    /// If true, the command will be executed after the button is released.
    pub release: bool,
}

/// C: include/configuration.h:430 — struct tray_output_t.
#[derive(Debug, Clone, Default)]
pub struct tray_output_t {
    pub output: String,
}

/// C: include/configuration.h:379 — struct bar_colors (inside Barconfig).
#[derive(Debug, Clone, Default)]
pub struct bar_colors {
    pub background: Option<String>,
    pub statusline: Option<String>,
    pub separator: Option<String>,
    pub focused_background: Option<String>,
    pub focused_statusline: Option<String>,
    pub focused_separator: Option<String>,
    pub focused_workspace_border: Option<String>,
    pub focused_workspace_bg: Option<String>,
    pub focused_workspace_text: Option<String>,
    pub active_workspace_border: Option<String>,
    pub active_workspace_bg: Option<String>,
    pub active_workspace_text: Option<String>,
    pub inactive_workspace_border: Option<String>,
    pub inactive_workspace_bg: Option<String>,
    pub inactive_workspace_text: Option<String>,
    pub urgent_workspace_border: Option<String>,
    pub urgent_workspace_bg: Option<String>,
    pub urgent_workspace_text: Option<String>,
    pub binding_mode_border: Option<String>,
    pub binding_mode_bg: Option<String>,
    pub binding_mode_text: Option<String>,
}

/// C: include/configuration.h:290 — struct Barconfig. Holds the status bar
/// configuration (i3bar). One of these structures is created for each 'bar'
/// block in the config.
#[derive(Debug, Clone, Default)]
pub struct Barconfig {
    /// Automatically generated ID for this bar config. Used by the bar
    /// process to request a specific configuration.
    pub id: Option<String>,
    /// Number of outputs in the outputs array.
    pub num_outputs: i32,
    /// Outputs on which this bar should show up on. C uses an array for
    /// simplicity (since we store just strings).
    pub outputs: Vec<String>,
    /// List of outputs on which the tray is allowed to be shown, in order.
    pub tray_outputs: VecDeque<tray_output_t>,
    /// Padding around the tray icons.
    pub tray_padding: i32,
    /// Path to the i3 IPC socket.
    pub socket_path: Option<String>,
    /// Bar display mode (hide unless modifier is pressed or show in dock
    /// mode or always hide in invisible mode).
    pub mode: bar_mode_t,
    /// The current hidden_state of the bar, which indicates whether it is
    /// hidden or shown.
    pub hidden_state: bar_hidden_state_t,
    /// Bar modifier (to show bar when in hide mode).
    pub modifier: u32,
    /// C: struct bar_bindings_head bar_bindings.
    pub bar_bindings: VecDeque<Barbinding>,
    /// Bar position (bottom by default).
    pub position: bar_position_t,
    /// Command that should be run to execute i3bar.
    pub i3bar_command: Option<String>,
    /// Command that should be run to get a statusline, for example
    /// 'i3status'. Will be passed to the shell.
    pub status_command: Option<String>,
    /// Command that should be run to get the workspace buttons. Will be
    /// passed to the shell.
    pub workspace_command: Option<String>,
    /// Font specification for all text rendered on the bar.
    pub font: Option<String>,
    /// A custom separator to use instead of a vertical line.
    pub separator_symbol: Option<String>,
    /// Hide workspace buttons? Configuration option is
    /// 'workspace_buttons no' but we invert the bool to get the correct
    /// default when initializing with zero.
    pub hide_workspace_buttons: bool,
    /// The minimal width for workspace buttons.
    pub workspace_min_width: i32,
    /// Strip workspace numbers? Configuration option is
    /// 'strip_workspace_numbers yes'.
    pub strip_workspace_numbers: bool,
    /// Strip workspace name? Configuration option is
    /// 'strip_workspace_name yes'.
    pub strip_workspace_name: bool,
    /// Hide mode button? Configuration option is
    /// 'binding_mode_indicator no' but we invert the bool for the same
    /// reason as hide_workspace_buttons.
    pub hide_binding_mode_indicator: bool,
    /// Enable verbose mode? Useful for debugging purposes.
    pub verbose: bool,
    /// Defines the height of the bar in pixels.
    pub bar_height: u32,
    pub padding: Rect,
    pub colors: bar_colors,
}

/// C: include/configuration.h:241 — struct config_client (color codes).
#[derive(Clone, Copy, Default)]
pub struct config_client {
    pub background: color_t,
    pub focused: Colortriple,
    pub focused_inactive: Colortriple,
    pub focused_tab_title: Colortriple,
    pub unfocused: Colortriple,
    pub urgent: Colortriple,
    pub placeholder: Colortriple,
    pub got_focused_tab_title: bool,
}

/// C: include/configuration.h:251 — struct config_bar (color codes).
#[derive(Clone, Copy, Default)]
pub struct config_bar {
    pub focused: Colortriple,
    pub unfocused: Colortriple,
    pub urgent: Colortriple,
}

/// C: include/configuration.h:106 — struct Config, the complete
/// configuration. The part which is not already in dedicated structures in
/// include/data.h. data.rs keeps a subset (also named Config) that the tree
/// layer reads; this is the full structure used by the config layer.
/// (No Debug/Clone derives: libi3's i3Font and color_t carry neither; C has
/// no derived operations on struct Config either.)
pub struct Configuration {
    pub terminal: Option<String>,
    pub font: i3Font,
    pub ipc_socket_path: Option<String>,
    pub restart_state_path: Option<String>,
    pub default_layout: crate::data::Layout,
    pub container_stack_limit: i32,
    pub container_stack_limit_value: i32,
    pub default_border_width: i32,
    pub default_floating_border_width: i32,
    /// Default orientation for new containers.
    pub default_orientation: crate::data::Orientation,
    /// By default, focus follows mouse. If the user explicitly wants to
    /// turn this off (and instead rely only on the keyboard for changing
    /// focus), we allow them to do this with this relatively special option.
    /// It is not planned to add any different focus models.
    pub disable_focus_follows_mouse: bool,
    /// With the mouse_warping option, you can control when the mouse cursor
    /// should be warped. "none" disables warping entirely, whereas "output"
    /// is the default behavior.
    pub mouse_warping: crate::data::Warping,
    /// Remove borders if they are adjacent to the screen edge.
    pub hide_edge_borders: crate::data::HideEdgeBordersMode,
    /// By default, a workspace bar is drawn at the bottom of the screen.
    pub disable_workspace_bar: bool,
    /// Focus wrapping mode (see the C header for the full comment).
    pub focus_wrapping: crate::data::FocusWrapping,
    /// Use the Xinerama API instead of RandR.
    pub force_xinerama: bool,
    /// Don't use RandR 1.5 for querying outputs.
    pub disable_randr15: bool,
    /// Overwrites output detection (for testing), see src/fake_outputs.c.
    pub fake_outputs: Option<String>,
    /// Automatic workspace back and forth switching.
    pub workspace_auto_back_and_forth: bool,
    /// The reset of the urgency flag can be delayed using an urgency timer.
    pub workspace_urgency_timer: f32,
    /// Behavior when a window sends a NET_ACTIVE_WINDOW message.
    pub focus_on_window_activation: focus_on_window_activation_t,
    /// Specifies whether or not marks should be displayed in the window
    /// decoration.
    pub show_marks: bool,
    /// Title alignment options.
    pub title_align: title_align_t,
    /// The default border style for new windows.
    pub default_border: crate::data::BorderStyle,
    /// The default border style for new floating windows.
    pub default_floating_border: crate::data::BorderStyle,
    /// The modifier which needs to be pressed in combination with your mouse
    /// buttons to do things with floating windows (move, resize).
    pub floating_modifier: u32,
    /// The modifier which needs to be pressed in combination with the
    /// floating modifier and your mouse buttons to swap containers during
    /// tiling drag.
    pub swap_modifier: u32,
    /// Maximum and minimum dimensions of a floating window.
    pub floating_maximum_width: i32,
    pub floating_maximum_height: i32,
    pub floating_minimum_width: i32,
    pub floating_minimum_height: i32,
    /// Color codes.
    pub client: config_client,
    pub bar: config_bar,
    /// What should happen when a new popup is opened during fullscreen mode.
    pub popup_during_fullscreen: popup_during_fullscreen_t,
    /// The number of currently parsed barconfigs.
    pub number_barconfigs: i32,
    pub tiling_drag: tiling_drag_t,
    /// Gap sizes.
    pub gaps: gaps_t,
    /// Disable gaps if there is only one container on the workspace.
    pub smart_gaps: crate::data::SmartGaps,
}

impl Default for Configuration {
    /// C: memset(&config, 0, sizeof(config)) — every member zero. Enum
    /// discriminants follow the C values (0 for every anonymous enum).
    fn default() -> Configuration {
        Configuration {
            terminal: None,
            font: i3Font {
                r#type: FONT_TYPE_NONE,
                height: 0,
                pattern: String::new(),
            },
            ipc_socket_path: None,
            restart_state_path: None,
            default_layout: crate::data::Layout::L_DEFAULT,
            container_stack_limit: 0,
            container_stack_limit_value: 0,
            default_border_width: 0,
            default_floating_border_width: 0,
            default_orientation: crate::data::Orientation::NO_ORIENTATION,
            disable_focus_follows_mouse: false,
            mouse_warping: crate::data::Warping::POINTER_WARPING_OUTPUT,
            hide_edge_borders: crate::data::HideEdgeBordersMode(0),
            disable_workspace_bar: false,
            focus_wrapping: crate::data::FocusWrapping::FOCUS_WRAPPING_OFF,
            force_xinerama: false,
            disable_randr15: false,
            fake_outputs: None,
            workspace_auto_back_and_forth: false,
            workspace_urgency_timer: 0.0,
            focus_on_window_activation: focus_on_window_activation_t::FOWA_SMART,
            show_marks: false,
            title_align: title_align_t::ALIGN_LEFT,
            default_border: crate::data::BorderStyle::BS_NONE,
            default_floating_border: crate::data::BorderStyle::BS_NONE,
            floating_modifier: 0,
            swap_modifier: 0,
            floating_maximum_width: 0,
            floating_maximum_height: 0,
            floating_minimum_width: 0,
            floating_minimum_height: 0,
            client: config_client::default(),
            bar: config_bar::default(),
            popup_during_fullscreen: popup_during_fullscreen_t::PDF_SMART,
            number_barconfigs: 0,
            tiling_drag: tiling_drag_t::TILING_DRAG_OFF,
            gaps: gaps_t::default(),
            smart_gaps: crate::data::SmartGaps::SMART_GAPS_OFF,
        }
    }
}

/// C: src/config.c:185-192 — the INIT_COLOR macro of load_configuration.
macro_rules! INIT_COLOR {
    ($x:expr, $cborder:expr, $cbackground:expr, $ctext:expr, $cindicator:expr) => {
        $x.border = draw_util_hex_to_color($cborder);
        $x.background = draw_util_hex_to_color($cbackground);
        $x.text = draw_util_hex_to_color($ctext);
        $x.indicator = draw_util_hex_to_color($cindicator);
        $x.child_border = draw_util_hex_to_color($cbackground);
    };
}

/// C: src/config.c:182-232 — "Clear the old config or initialize the data
/// structure": memset(0) followed by the default values.
pub fn init_defaults(config: &mut Configuration) {
    // C: memset(&config, 0, sizeof(config));
    *config = Configuration::default();

    // Initialize default colors
    config.client.background = draw_util_hex_to_color("#000000");
    INIT_COLOR!(config.client.focused, "#4c7899", "#285577", "#ffffff", "#2e9ef4");
    INIT_COLOR!(config.client.focused_inactive, "#333333", "#5f676a", "#ffffff", "#484e50");
    INIT_COLOR!(config.client.unfocused, "#333333", "#222222", "#888888", "#292d2e");
    INIT_COLOR!(config.client.urgent, "#2f343a", "#900000", "#ffffff", "#900000");
    config.client.got_focused_tab_title = false;

    // border and indicator color are ignored for placeholder contents
    INIT_COLOR!(config.client.placeholder, "#000000", "#0c0c0c", "#ffffff", "#000000");

    // the last argument (indicator color) is ignored for bar colors
    INIT_COLOR!(config.bar.focused, "#4c7899", "#285577", "#ffffff", "#000000");
    INIT_COLOR!(config.bar.unfocused, "#333333", "#222222", "#888888", "#000000");
    INIT_COLOR!(config.bar.urgent, "#2f343a", "#900000", "#ffffff", "#000000");

    config.show_marks = true;

    config.default_border = crate::data::BorderStyle::BS_NORMAL;
    config.default_floating_border = crate::data::BorderStyle::BS_NORMAL;
    config.default_border_width = logical_px(2);
    config.default_floating_border_width = logical_px(2);
    // Set default_orientation to NO_ORIENTATION for auto orientation.
    config.default_orientation = crate::data::Orientation::NO_ORIENTATION;

    config.gaps.inner = 0;
    config.gaps.top = 0;
    config.gaps.right = 0;
    config.gaps.bottom = 0;
    config.gaps.left = 0;

    // Set default urgency reset delay to 500ms
    if config.workspace_urgency_timer == 0.0 {
        config.workspace_urgency_timer = 0.5;
    }

    config.focus_wrapping = crate::data::FocusWrapping::FOCUS_WRAPPING_ON;

    config.tiling_drag = tiling_drag_t::TILING_DRAG_MODIFIER;
    config.swap_modifier = crate::config_directives::XCB_KEY_BUT_MASK_SHIFT;
}

/// C: src/config.c:165 load_configuration — (re)loads the configuration file
/// (sets useful defaults before). If you specify override_configpath, only
/// this path is used to look for a configuration file. load_type specifies
/// the type of loading: C_VALIDATE is used to only verify the correctness of
/// the config file (used with the flag -C), C_LOAD will load the config for
/// normal use and display errors in the nagbar, C_RELOAD will also clear the
/// previous config.
///
/// The "parseまで" part is implemented: the X11/tree side effects after the
/// parse (key grabbing, tree_render, …) run in later phases; the directives
/// write into the ConfigDirectives state and the caller pushes bindings and
/// assignments into the Tree.
pub fn load_configuration(
    d: &mut ConfigDirectives,
    override_configpath: Option<&str>,
    load_type: config_load_t,
) -> bool {
    load_configuration_with_tree(d, override_configpath, load_type, None)
}

/// [load_configuration] for a caller that owns the Tree.
///
/// C's free_configuration() walks the global all_cons list on C_RELOAD and
/// clears every container's ran_assignments / assignment counter
/// (src/config.c:86-96), so that assignments re-run against the new config.
/// The port's free_configuration() needs the Tree for the same walk, which is
/// why the caller that has one (a reload, src/commands.c:1680) hands it in;
/// the config parser itself (and -C validation) passes None like the C code
/// does when the global list is not reachable yet.
pub fn load_configuration_with_tree(
    d: &mut ConfigDirectives,
    override_configpath: Option<&str>,
    load_type: config_load_t,
    tree: Option<&mut Tree>,
) -> bool {
    if load_type == config_load_t::C_RELOAD {
        free_configuration(d, tree);
    }

    // C: SLIST_INIT(&modes); + the default mode (config.c:170-179).
    d.modes.clear();
    let default_mode = Mode {
        name: "default".to_string(),
        pango_markup: false,
        bindings: VecDeque::new(),
    };
    d.modes.push_front(default_mode);
    // C: bindings = default_mode->bindings; current_binding_mode = default_mode->name;
    d.current_binding_mode = Some(crate::config_directives::DEFAULT_BINDING_MODE.to_string());

    // Clear the old config or initialize the data structure.
    init_defaults(&mut d.config);

    // C: FREE(current_configpath); current_configpath = get_config_path(...);
    d.current_configpath = get_config_path(override_configpath, true);
    let current_configpath = match d.current_configpath.clone() {
        Some(path) => path,
        None => {
            // C: die("Unable to find the configuration file (looked at ...)");
            panic!(
                "Unable to find the configuration file (looked at \
                 $XDG_CONFIG_HOME/i3/config, ~/.i3/config, $XDG_CONFIG_DIRS/i3/config \
                 and /etc/i3/config)"
            );
        }
    };

    // C: config.c:242-250 — clear the old included files.
    d.included_files.clear();

    // C: char resolved_path[PATH_MAX]; if (realpath(...) == NULL) die(...);
    let resolved_path = match std::fs::canonicalize(&current_configpath) {
        Ok(path) => path,
        Err(err) => {
            // C: die("realpath(%s): %s", current_configpath, strerror(errno));
            panic!("realpath({}): {}", current_configpath, err);
        }
    };
    let resolved = resolved_path.to_string_lossy().into_owned();

    // C: file = scalloc(1, sizeof(IncludedFile)); file->path = sstrdup(resolved_path);
    //    TAILQ_INSERT_TAIL(&included_files, file, files);
    // The entry is inserted *before* parsing, so the main file is already
    // part of the list while its own "include" directives run (C relies on
    // that for the duplicate check in config_directives.c:39). It also means
    // the main file is TAILQ_FIRST(), which is what GET_CONFIG ("config" =
    // TAILQ_FIRST(&included_files)->raw_contents, ipc.c:1288) and
    // GET_VERSION (skip the first entry, ipc.c:1094) expect.
    d.included_files.push_back(IncludedFile {
        path: resolved.clone(),
        ..Default::default()
    });
    let mut file = IncludedFile {
        path: resolved.clone(),
        ..Default::default()
    };
    // C: LOG("Parsing configfile %s\n", resolved_path);
    crate::log::log_(&format!("Parsing configfile {}\n", resolved));

    // C: struct parser_ctx ctx = { .use_nagbar = (load_type != C_VALIDATE), ... };
    let mut ctx = ConfigParserCtx::<Match>::default();
    ctx.use_nagbar = load_type != config_load_t::C_VALIDATE;
    let result = parse_file(&mut ctx, d, &resolved, &mut file);
    free_variables(&mut ctx);
    // C: the list entry and the parse_file argument are the same object; the
    // port copies the contents into the entry that was pushed above. It is
    // looked up by path because a recursive include can append entries in
    // between.
    if let Some(pos) = d.included_files.iter().position(|f| f.path == resolved) {
        d.included_files[pos].raw_contents = file.raw_contents;
        d.included_files[pos].variable_replaced_contents = file.variable_replaced_contents;
    }

    if result == ParseFileResult::PARSE_FILE_FAILED {
        // C: die("Could not open configuration file: %s\n", strerror(errno));
        panic!("Could not open configuration file: {}", errno_string());
    }

    // C: extract_workspace_names_from_bindings(); reorder_bindings();
    // TODO(bindings port): both operate on the global bindings list; they
    // belong to src/workspace.c / src/bindings.c (reorder) and run once the
    // parsed bindings are pushed into the Tree by the caller.

    // C: config.c:277-281
    if d.config.font.r#type == FONT_TYPE_NONE && load_type != config_load_t::C_VALIDATE {
        // C: ELOG("You did not specify required configuration option \"font\"\n");
        crate::log::errorlog("You did not specify required configuration option \"font\"\n");
        d.config.font = load_font("fixed", true);
        // C: set_font(&config.font); — the caller installs it as the current
        // font with the real metrics through apply_font() (the parser has no
        // X connection; see the doc comment there).
    }

    // C: config.c:285-291 "Make bar config blocks without a configured font
    // use the i3-wide font." — C can do this right here because CFGFUN(font)
    // already ran load_font() during parsing, which rewrites
    // config.font.pattern to "fixed" when the requested X font does not exist.
    // The port's parser has no X connection, so the font is only resolved by
    // apply_font(); the same assignment therefore lives there (see
    // apply_barconfig_font_default).

    if load_type == config_load_t::C_RELOAD {
        // C: translate_keysyms(); grab_all_keys(conn); regrab_all_buttons(conn);
        //    gaps_reapply_workspace_assignments(); tree_render();
        // These need the Bindings, the X connection, the backend and the Tree,
        // which load_configuration() does not take; the C_RELOAD caller drives
        // them right after this call, in the same order — see
        // crate::reload::reload.
    }

    // C: return result == 0;
    result == ParseFileResult::PARSE_FILE_SUCCESS
}

/// C: src/config.c:32 free_configuration — frees the previous configuration
/// (only called with C_RELOAD). tree carries the parts the C globals
/// all_cons / assignments / ws_assignments hold (ran_assignments
/// invalidation); the deco_render_params pixmap caches are TODO(render
/// port).
pub fn free_configuration(d: &mut ConfigDirectives, tree: Option<&mut Tree>) {
    // C: assert(conn != NULL); — X11-only.

    // If we are currently in a binding mode, we first revert to the default
    // since we have no guarantee that the current mode will even still exist
    // after parsing the config again. See #2228.
    // C: switch_mode("default"); ungrab_all_keys(conn);
    // TODO(commands/x port): switch_mode and ungrab_all_keys run in the
    // commands/x layer; clearing the lists below removes the state they
    // would act on.
    d.current_binding_mode = Some(crate::config_directives::DEFAULT_BINDING_MODE.to_string());

    // C: while (!SLIST_EMPTY(&modes)) { free all modes with their bindings }
    d.modes.clear();

    // C: while (!TAILQ_EMPTY(&assignments)) { free all assignments }
    d.assignments.clear();

    // C: while (!TAILQ_EMPTY(&ws_assignments)) { free all ws assignments }
    d.ws_assignments.clear();

    // C: while (!TAILQ_EMPTY(&barconfigs)) { free all bar configs }
    d.barconfigs.clear();

    // C: TAILQ_FOREACH (con, &all_cons, all_cons) — assignments changed,
    // previously ran assignments are invalid; pixmap caches invalidated.
    if let Some(t) = tree {
        // C iterates the TAILQ while mutating the cons; the port copies the ids
        // first so the loop can borrow the map mutably.
        let all_cons: Vec<crate::ConId> = t.all_cons.iter().copied().collect();
        for con in all_cons {
            if let Some(window) = t.cons[con].window {
                if let Some(win) = t.windows.get_mut(window) {
                    win.ran_assignments.clear();
                    win.nr_assignments = 0;
                }
            }
            // C: FREE(con->deco_render_params); — the cached decoration
            // parameters are dropped so that the reload's tree_render()
            // (config.c:298) redraws every decoration with the new drawing
            // parameters (colors, font, border widths).
            if let Some(c) = t.cons.get_mut(con) {
                c.deco_render_params = None;
            }
        }
    }

    // C: free_font(); — TODO(X11 port)
    // C: free(config.ipc_socket_path); free(config.restart_state_path);
    //    free(config.fake_outputs); — cleared by init_defaults (C memset).
}

/// C: src/config_parser.c:507-533 `start_config_error_nagbar`
///
/// The i3-nagbar invocation of the config parser, in C's order: the font is
/// config.font.pattern ("fixed" when unset), the message depends on
/// has_errors, the first button re-runs the editor and reloads, the second one
/// shows the error log. The caller passes the spawner so the argv can be
/// compared against C without forking.
///
/// argv[0] is the placeholder C passes as NULL — exec_i3_utility() replaces it
/// with the (resolved) i3-nagbar binary.
pub fn config_error_nagbar_argv(
    configpath: &str,
    font_pattern: &str,
    has_errors: bool,
    errorfilename: Option<&str>,
) -> Vec<String> {
    // C: sasprintf(&editaction, "i3-sensible-editor \"%s\" && i3-msg reload\n", configpath);
    let editaction = format!("i3-sensible-editor \"{}\" && i3-msg reload\n", configpath);
    // C: sasprintf(&pageraction, "i3-sensible-pager \"%s\"\n", errorfilename);
    let pageraction = format!("i3-sensible-pager \"{}\"\n", errorfilename.unwrap_or(""));

    let mut argv: Vec<String> = vec![
        // C: NULL /* will be replaced by the executable path */
        String::new(),
        // C: "-f", (config.font.pattern ? config.font.pattern : "fixed")
        "-f".to_string(),
        font_pattern.to_string(),
        // C: "-t", (has_errors ? "error" : "warning")
        "-t".to_string(),
        if has_errors { "error" } else { "warning" }.to_string(),
        // C: "-m", (has_errors ? "You have an error in your i3 config file!"
        //                 : "Your config is outdated. Please fix the warnings
        //                    to make sure everything works.")
        "-m".to_string(),
        if has_errors {
            "You have an error in your i3 config file!".to_string()
        } else {
            "Your config is outdated. Please fix the warnings to make sure everything works."
                .to_string()
        },
        // C: "-b", "edit config", editaction
        "-b".to_string(),
        "edit config".to_string(),
        editaction,
    ];

    // C: (errorfilename ? "-b" : NULL), (has_errors ? "show errors" :
    //    "show warnings"), pageraction
    // A NULL in the middle terminates the C argv as far as execvp() is
    // concerned, so the three remaining slots only exist when the error log is
    // known — dropping them here is what the child would have seen.
    if errorfilename.is_some() {
        argv.push("-b".to_string());
        argv.push(
            if has_errors { "show errors" } else { "show warnings" }.to_string(),
        );
        argv.push(pageraction);
    }

    argv
}

/// C: src/config_parser.c:507 `start_config_error_nagbar` — the tail:
/// start_nagbar(&config_error_nagbar_pid, argv).
pub fn start_config_error_nagbar(
    spawner: &mut dyn crate::util::NagbarSpawner,
    nagbar_pid: &mut i32,
    configpath: &str,
    font_pattern: &str,
    has_errors: bool,
    errorfilename: Option<&str>,
) {
    let argv = config_error_nagbar_argv(configpath, font_pattern, has_errors, errorfilename);
    // C: start_nagbar(&config_error_nagbar_pid, argv);
    //    free(editaction); free(pageraction);
    crate::util::start_nagbar(spawner, nagbar_pid, &argv);
}

/// C: src/config_parser.c:850 parse_file — parses the given file by first
/// replacing the variables, then calling parse_config and possibly launching
/// i3-nagbar. The working directory is restored afterwards (C: getcwd/chdir).
pub fn parse_file(
    ctx: &mut ConfigParserCtx<Match>,
    hooks: &mut ConfigDirectives,
    f: &str,
    included_file: &mut IncludedFile,
) -> ParseFileResult {
    // C: char *old_dir = getcwd(NULL, 0);
    let old_dir = match std::env::current_dir() {
        Ok(dir) => Some(dir),
        Err(_) => None,
    };

    let mut result = parse_file_inner(ctx, hooks, f, included_file);

    match old_dir {
        Some(old) => {
            if std::env::set_current_dir(&old).is_err() {
                // C: ELOG("chdir(%s) failed: %s\n", old_dir, strerror(errno));
                crate::log::errorlog(&format!("chdir({}) failed: {}\n", old.display(), errno_string()));
                result = ParseFileResult::PARSE_FILE_FAILED;
            }
        }
        // C: chdir(NULL) fails with EFAULT → PARSE_FILE_FAILED.
        None => result = ParseFileResult::PARSE_FILE_FAILED,
    }
    result
}

/// C: src/config_parser.c:609 try_chdir — changes the working directory to
/// the config file's directory (so that relative includes keep working).
fn try_chdir(path: &str) -> bool {
    // C: dirname(3) might modify the buffer, so a copy is used.
    let dir = dirname(path);
    // C: LOG("Changing working directory to config file directory %s\n", dir);
    crate::log::log_(&format!(
        "Changing working directory to config file directory {}\n",
        dir
    ));
    match std::env::set_current_dir(&dir) {
        Ok(()) => true,
        Err(err) => {
            // C: ELOG("chdir(%s) failed: %s\n", dir, strerror(errno));
            crate::log::errorlog(&format!("chdir({}) failed: {}\n", dir, err));
            false
        }
    }
}

/// C: dirname(3) — "." for a bare filename, "/" for "/".
fn dirname(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }
    match Path::new(path).parent() {
        Some(parent) if parent.as_os_str().is_empty() => ".".to_string(),
        Some(parent) => parent.to_string_lossy().into_owned(),
        None => ".".to_string(),
    }
}

/// C: strerror(errno) — best effort: the string of the last OS error.
fn errno_string() -> String {
    std::io::Error::last_os_error().to_string()
}

/// C: src/config_parser.c:625 parse_file_inner
fn parse_file_inner(
    ctx: &mut ConfigParserCtx<Match>,
    hooks: &mut ConfigDirectives,
    f: &str,
    included_file: &mut IncludedFile,
) -> ParseFileResult {
    if !try_chdir(f) {
        return ParseFileResult::PARSE_FILE_FAILED;
    }

    // C: open + fstat + fread(stbuf.st_size) — a short read fails the call.
    use std::io::Read;
    let mut file = match std::fs::File::open(f) {
        Ok(file) => file,
        Err(_) => return ParseFileResult::PARSE_FILE_FAILED,
    };
    let size = match file.metadata() {
        Ok(meta) => meta.len() as usize,
        Err(_) => return ParseFileResult::PARSE_FILE_FAILED,
    };
    let mut raw = vec![0u8; size];
    if file.read_exact(&mut raw).is_err() {
        return ParseFileResult::PARSE_FILE_FAILED;
    }
    included_file.raw_contents = String::from_utf8_lossy(&raw).into_owned();

    // C: config_parser.c:653-746 — the line merging pass: lines ending in
    // "\\\n" are joined (comments keep their continuation), "#"-comments
    // and short lines are skipped, "set" and "set_from_resource" extract
    // variables.
    let mut buffer: Vec<u8> = Vec::with_capacity(4096); // C: char buffer[4096]
    let mut continuation: Option<usize> = None; // C: char *continuation
    let mut invalid_sets = false; // C: bool invalid_sets
    let mut buf: Vec<u8> = Vec::new(); // C: char *buf
    let mut reader = FgetsLineReader { data: &raw, pos: 0 };

    loop {
        // C: if (!continuation) { continuation = buffer; }
        let cpos = continuation.unwrap_or(0);
        // C: fgets(continuation, sizeof(buffer) - (continuation - buffer), fstr)
        let limit = 4096 - cpos;
        let chunk = match reader.next_chunk(limit) {
            Some(chunk) => chunk,
            None => break, // C: fgets == NULL && feof(fstr)
        };
        // C: fgets writes the chunk at the continuation position and
        // NUL-terminates it; the Vec models the string by its length.
        buffer.truncate(cpos);
        buffer.extend_from_slice(chunk);

        // C: if (buffer[strlen(buffer) - 1] != '\n' && !feof(fstr))
        if buffer.last() != Some(&b'\n') && reader.pos < raw.len() {
            // C: ELOG("Your line continuation is too long, it exceeds %zd bytes\n", sizeof(buffer));
            crate::log::errorlog("Your line continuation is too long, it exceeds 4096 bytes\n");
        }

        // C: sscanf(buffer, "%511s %4095[^\n]", key, value);
        let (key, mut value) = match sscanf_key_value(&buffer) {
            Some(kv) => kv,
            None => (Vec::new(), Vec::new()),
        };
        // C: const bool skip_line = (sscanf(...) < 1 || strlen(key) < 3);
        let skip_line = buffer_is_empty_or_key_short(&key);
        // C: const bool comment = (key[0] == '#');
        let comment = key.first() == Some(&b'#');
        // C: value[4095] = '\n'; — for a value truncated to 4095 bytes this
        // overwrites the NUL terminator in C; the port appends the byte to
        // model the same content.
        if value.len() == 4095 {
            value.push(b'\n');
        }

        // C: continuation = strstr(buffer, "\\\n");
        match strstr_backslash_newline(&buffer) {
            Some(pos) if !comment => {
                // Read more input into the buffer before processing it.
                continuation = Some(pos);
                continue;
            }
            _ => {
                // C: DLOG("line continuation in comment is ignored: ...") —
                // DLOG lives in i3-core; the continuation flag is cleared and
                // the (comment) line is processed as-is.
                continuation = None;
            }
        }

        // C: strcpy(buf + strlen(buf), buffer);
        buf.extend_from_slice(&buffer);

        // Skip comments and empty lines.
        if skip_line || comment {
            continue;
        }

        if strcasecmp_eq(&key, b"set") && !value.is_empty() {
            // C: char v_key[512]; char v_value[4096] = {'\0'};
            //    sscanf(value, "%511s %4095[^\n]", v_key, v_value)
            let (tokens, rest) = sscanf_tokens_rest(&value, &[511], 4095);
            if tokens.is_empty() {
                crate::log::errorlog(&format!(
                    "Failed to parse variable specification '{}', skipping it.\n",
                    String::from_utf8_lossy(&value)
                ));
                invalid_sets = true;
                continue;
            }
            if tokens[0].first() != Some(&b'$') {
                // C: ELOG("Malformed variable assignment, name has to start with $\n");
                crate::log::errorlog("Malformed variable assignment, name has to start with $\n");
                invalid_sets = true;
                continue;
            }
            upsert_variable(
                &mut ctx.variables,
                &String::from_utf8_lossy(&tokens[0]),
                &String::from_utf8_lossy(&rest),
            );
            continue;
        } else if strcasecmp_eq(&key, b"set_from_resource") {
            // C: char res_name[512] = {'\0'}; char v_key[512];
            //    char fallback[4096] = {'\0'};
            //    sscanf(value, "%511s %511s %4095[^\n]", v_key, res_name, fallback)
            let (tokens, rest) = sscanf_tokens_rest(&value, &[511, 511], 4095);
            if tokens.is_empty() {
                crate::log::errorlog(&format!(
                    "Failed to parse resource specification '{}', skipping it.\n",
                    String::from_utf8_lossy(&value)
                ));
                invalid_sets = true;
                continue;
            }
            if tokens[0].first() != Some(&b'$') {
                crate::log::errorlog("Malformed variable assignment, name has to start with $\n");
                invalid_sets = true;
                continue;
            }
            // C: Ensure that this string is terminated — fallback is empty
            // unless the third field was parsed (sscanf leaves it
            // uninitialized otherwise; the C comment explains the memory
            // safety hazard).
            let fallback = if tokens.len() >= 2 {
                String::from_utf8_lossy(&rest).into_owned()
            } else {
                String::new()
            };
            let res_name = if tokens.len() >= 2 {
                String::from_utf8_lossy(&tokens[1]).into_owned()
            } else {
                String::new()
            };
            // C: char *res_value = get_resource(res_name); — conn == NULL in
            // this port (no X11 connection), so get_resource returns NULL and
            // the fallback is used (C: config_parser.c:570-573).
            let res_value = match get_resource(&res_name) {
                Some(value) => value,
                None => {
                    // C: DLOG("Could not get resource '%s', using fallback '%s'.\n", ...);
                    fallback
                }
            };
            upsert_variable(
                &mut ctx.variables,
                &String::from_utf8_lossy(&tokens[0]),
                &res_value,
            );
            continue;
        }
    }
    // C: fclose(fstr); — automatic (the File drops).
    // C: xcb_xrm database cleanup — TODO(X11 port).

    // C: config_parser.c:755-816 — substitute the variables.
    let substituted = substitute_variables(ctx, &buf);
    included_file.variable_replaced_contents = substituted.clone();

    // C: struct context *context = scalloc(1, sizeof(struct context));
    //    context->filename = f;
    let mut context = ParserContext {
        filename: f.to_string(),
        ..Default::default()
    };
    parse_config(ctx, hooks, &substituted, &mut context);
    // C: parse_config reports through ELOG(), which the log layer prints; the
    // port collects the formatted lines in the ParserContext, so they are
    // emitted here (the C code has already printed them at this point).
    for line in &context.error_log {
        // C: ELOG(fmt) => errorlog("ERROR: " fmt)
        crate::log::elog(line);
    }
    if ctx.has_errors {
        context.has_errors = true;
    }

    check_for_duplicate_bindings(hooks, &mut context);

    if ctx.use_nagbar && (context.has_errors || context.has_warnings || invalid_sets) {
        // C: ELOG("FYI: You are using i3 version %s\n", i3_version);
        crate::log::errorlog(&format!("FYI: You are using i3 version {}\n", libi3::i3_version()));
        // C: start_config_error_nagbar(f, context->has_errors || invalid_sets);
        // The font pattern is config.font.pattern ("fixed" when the config did
        // not set one — C's ternary passes the NULL default the same way).
        let font_pattern = if hooks.config.font.pattern.is_empty() {
            "fixed".to_string()
        } else {
            hooks.config.font.pattern.clone()
        };
        // C: the second "-b" button is only added when the error log file
        // exists (errorfilename is set by init_logging()).
        let errorfilename = crate::log::errorfilename();
        start_config_error_nagbar(
            &mut crate::util::ForkExecNagbar,
            &mut hooks.config_error_nagbar_pid,
            f,
            &font_pattern,
            context.has_errors || invalid_sets,
            errorfilename.as_deref(),
        );
    }

    let has_errors = context.has_errors;
    // C: FREE(context->line_copy); free(context); free(new); free(buf);
    // — automatic in Rust.

    if has_errors {
        return ParseFileResult::PARSE_FILE_CONFIG_ERRORS;
    }
    ParseFileResult::PARSE_FILE_SUCCESS
}

/// C: fgets — reads up to limit-1 bytes, stopping after a '\n', from the
/// reader's position. None when the input is exhausted (C: fgets == NULL).
struct FgetsLineReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> FgetsLineReader<'a> {
    fn next_chunk(&mut self, limit: usize) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let start = self.pos;
        let max = start + limit.saturating_sub(1);
        let mut end = start;
        while end < self.data.len() && end < max && self.data[end] != b'\n' {
            end += 1;
        }
        if end < self.data.len() && self.data[end] == b'\n' {
            end += 1; // fgets includes the newline
        }
        self.pos = end;
        Some(&self.data[start..end])
    }
}

/// C: isspace(3).
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c')
}

/// C: "%<limit>s" — reads a whitespace-delimited token of at most limit
/// bytes starting at pos; returns the token and advances pos, or None when
/// no token could be read (C: assignment failure).
fn sscanf_token(input: &[u8], pos: &mut usize, limit: usize) -> Option<Vec<u8>> {
    while *pos < input.len() && is_c_space(input[*pos]) {
        *pos += 1;
    }
    let mut out = Vec::new();
    while *pos < input.len() && !is_c_space(input[*pos]) && out.len() < limit {
        out.push(input[*pos]);
        *pos += 1;
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// C: "%<limit>[^\n]" — reads up to limit bytes up to (not including) the
/// next newline.
fn sscanf_until_newline(input: &[u8], pos: &mut usize, limit: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while *pos < input.len() && input[*pos] != b'\n' && out.len() < limit {
        out.push(input[*pos]);
        *pos += 1;
    }
    out
}

/// C: sscanf(buffer, "%511s %4095[^\n]", key, value) — the first
/// whitespace-delimited token (up to 511 bytes) and the rest of the line
/// (up to 4095 bytes). None when not even the key could be read (C:
/// sscanf < 1).
fn sscanf_key_value(buffer: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut pos = 0;
    let key = sscanf_token(buffer, &mut pos, 511)?;
    // The literal " " in the C format skips whitespace after the key.
    while pos < buffer.len() && is_c_space(buffer[pos]) {
        pos += 1;
    }
    let value = sscanf_until_newline(buffer, &mut pos, 4095);
    Some((key, value))
}

/// C: sscanf(value, "%511s %511s %4095[^\n]", v_key, res_name, fallback) —
/// up to two whitespace-delimited tokens plus the trailing remainder.
fn sscanf_tokens_rest(
    input: &[u8],
    limits: &[usize],
    rest_limit: usize,
) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut pos = 0;
    let mut tokens = Vec::new();
    for &limit in limits {
        match sscanf_token(input, &mut pos, limit) {
            Some(token) => tokens.push(token),
            None => break,
        }
    }
    // The literal " " in the C format skips whitespace before "%[^\n]".
    while pos < input.len() && is_c_space(input[pos]) {
        pos += 1;
    }
    let rest = sscanf_until_newline(input, &mut pos, rest_limit);
    (tokens, rest)
}

/// C: skip_line's second condition — strlen(key) < 3 (the first one,
/// sscanf < 1, is expressed by the empty key).
fn buffer_is_empty_or_key_short(key: &[u8]) -> bool {
    key.len() < 3
}

/// C: strcasecmp(3) equality on bytes (ASCII case-insensitive).
fn strcasecmp_eq(a: &[u8], b: &[u8]) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// C: strstr(buffer, "\\\n") — the position of a backslash directly
/// followed by a newline.
fn strstr_backslash_newline(buffer: &[u8]) -> Option<usize> {
    if buffer.len() < 2 {
        return None;
    }
    (0..buffer.len() - 1).find(|&i| buffer[i] == b'\\' && buffer[i + 1] == b'\n')
}

/// C: strcasestr(3) — case-insensitive substring search (ASCII).
fn strcasestr(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// C: src/config_parser.c:536 upsert_variable — inserts or updates a
/// variable assignment depending on whether it already exists. New entries
/// are sorted so that longer keys come first (C comment: "ensure that the
/// correct variable is matched in case of one being the prefix of
/// another"); among equal-length keys the most recent one comes first.
pub fn upsert_variable(variables: &mut Vec<i3_spec::config_parser::Variable>, key: &str, value: &str) {
    for current in variables.iter_mut() {
        if current.key != key {
            continue;
        }
        // C: DLOG("Updated variable: %s = %s -> %s\n", key, current->value, value);
        current.value = value.to_string();
        return;
    }

    // C: DLOG("Defined new variable: %s = %s\n", key, value);
    let mut loc: Option<usize> = None;
    for (i, test) in variables.iter().enumerate() {
        if key.len() >= test.key.len() {
            break;
        }
        loc = Some(i);
    }

    let new = i3_spec::config_parser::Variable {
        key: key.to_string(),
        value: value.to_string(),
    };
    match loc {
        None => variables.insert(0, new), // C: SLIST_INSERT_HEAD
        Some(i) => variables.insert(i + 1, new), // C: SLIST_INSERT_AFTER(loc, new, variables)
    }
}

thread_local! {
    /// C: src/config_parser.c:558 `static xcb_xrm_database_t *database = NULL;`
    ///
    /// The C code builds the database lazily out of the X server's
    /// RESOURCE_MANAGER property (xcb_xrm_database_from_default) because the
    /// parser has the xcb connection as a process global. The port's parser
    /// runs without a connection (config validation with -C has no X server at
    /// all), so the caller that owns the connection reads the property and
    /// installs it here before parsing — the same split apply_font() uses.
    static RESOURCE_MANAGER: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Reads the X server's RESOURCE_MANAGER property and installs it as the
/// resource database for [get_resource].
///
/// C: src/config_parser.c:558-580 — `database = xcb_xrm_database_from_default(conn)`
/// does exactly this: it fetches RESOURCE_MANAGER from the root window and
/// parses it. The caller owns the connection (see [set_resource_manager]).
pub fn load_resource_manager<C: x11rb::connection::Connection>(conn: Option<&C>) {
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    let conn = match conn {
        Some(conn) => conn,
        None => {
            set_resource_manager(None);
            return;
        }
    };
    let root = match conn.setup().roots.first() {
        Some(screen) => screen.root,
        None => {
            set_resource_manager(None);
            return;
        }
    };
    let atom = match conn.intern_atom(false, b"RESOURCE_MANAGER") {
        Ok(cookie) => cookie.reply().map(|reply| reply.atom),
        Err(_) => Ok(0),
    };
    let atom = match atom {
        Ok(atom) if atom != 0 => atom,
        _ => {
            set_resource_manager(None);
            return;
        }
    };
    let reply = conn.get_property(false, root, atom, AtomEnum::STRING, 0, u32::MAX);
    // C: xcb_xrm_database_from_default() reads the property as a C string, so
    // everything after the first NUL byte is ignored (the testsuite sets the
    // property with format 32, which pads the value with NUL bytes).
    let value = reply.ok().and_then(|cookie| cookie.reply().ok()).and_then(|reply| {
        let end = reply
            .value
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(reply.value.len());
        if end == 0 {
            None
        } else {
            Some(String::from_utf8_lossy(&reply.value[..end]).into_owned())
        }
    });
    // C: ELOG("Failed to open the resource database.") when the property is
    // missing; every lookup then returns NULL.
    if value.is_none() {
        crate::log::errorlog("Failed to open the resource database.\n");
    }
    set_resource_manager(value);
}

/// Installs the X server's RESOURCE_MANAGER property (the raw xrdb string) as
/// the resource database for [get_resource].
///
/// C: src/config_parser.c:570-590 xcb_xrm_database_from_default(conn) — the
/// caller reads the root window property and hands the contents over; None
/// mirrors "Failed to open the resource database" (the C code then loads an
/// empty database and returns NULL for every lookup).
pub fn set_resource_manager(value: Option<String>) {
    RESOURCE_MANAGER.with(|db| *db.borrow_mut() = value);
}

/// C: src/config_parser.c:570 get_resource — looks up an X resource in the
/// X resource database.
///
/// The port parses the RESOURCE_MANAGER string itself instead of linking
/// libxcb-xrm: Xrm queries are answered by [xrm_lookup], which implements the
/// subset the config needs (a spec line "\*mark: works" answering the query
/// "i3wm.mark", "\*" matching any number of name components, "?"/"*" inside a
/// component via fnmatch, and the most specific matching spec winning).
fn get_resource(name: &str) -> Option<String> {
    RESOURCE_MANAGER.with(|db| {
        let db = db.borrow();
        let db = db.as_deref()?;
        xrm_lookup(db, name)
    })
}

/// Xrm lookup over a raw RESOURCE_MANAGER string (xcb_xrm_resource_get_string).
///
/// Xrm loose binding: a spec whose first component is "*" may skip any number
/// of leading name components, each component is matched with fnmatch(3)
/// ("?"/"*"), and among all matching specs the one with the most concrete
/// components wins (the last one on a tie, like xrdb's last-wins merge).
fn xrm_lookup(db: &str, name: &str) -> Option<String> {
    let name_components: Vec<&str> = name.split('.').collect();
    let mut best: Option<(usize, String)> = None;
    for line in db.lines() {
        // C: xcb_xrm parses "spec: value" lines; entries without a colon are
        // ignored (xrdb would treat them as comments/garbage as well).
        let (spec, value) = match line.split_once(':') {
            Some((spec, value)) => (spec.trim(), value.trim_start()),
            None => continue,
        };
        if spec.is_empty() {
            continue;
        }
        // Xrm: a spec starting with "." is a tight binding (it has to start at
        // the root of the name); everything else is a loose binding that may
        // start at any component boundary (xcb_xrm's XrmGetResource).
        let tight = spec.starts_with('.');
        let spec_components: Vec<&str> = spec.split('.').filter(|c| !c.is_empty()).collect();
        if spec_components.is_empty() {
            continue;
        }
        let matched = if tight {
            xrm_match_components(&spec_components, &name_components)
        } else {
            (0..=name_components.len())
                .any(|skip| xrm_match_components(&spec_components, &name_components[skip..]))
        };
        if !matched {
            continue;
        }
        let specificity = spec_components
            .iter()
            .filter(|c| !c.contains('*') && !c.contains('?'))
            .count();
        match &best {
            Some((seen, _)) if *seen > specificity => {}
            _ => best = Some((specificity, value.to_string())),
        }
    }
    best.map(|(_, value)| value)
}

/// Matches all spec components against all name components: a component that
/// is exactly "*" stands for zero or more name components, every other
/// component is compared with fnmatch(3) ("?"/"*" wildcards, like Xrm).
fn xrm_match_components(spec: &[&str], name: &[&str]) -> bool {
    if spec.is_empty() {
        return name.is_empty();
    }
    if spec[0] == "*" {
        for skip in 0..=name.len() {
            if xrm_match_components(&spec[1..], &name[skip..]) {
                return true;
            }
        }
        return false;
    }
    if name.is_empty() {
        return false;
    }
    fnmatch(spec[0].as_bytes(), name[0].as_bytes()) && xrm_match_components(&spec[1..], &name[1..])
}

/// C: src/config_parser.c:755-816 — for every custom variable, see how
/// often it occurs in the file and how much extra bytes it requires when
/// replaced; then allocate a new buffer and copy the file over, replacing
/// occurrences of the variables, always picking the nearest match first
/// (the variable list is sorted longest-key first by upsert_variable).
fn substitute_variables(ctx: &ConfigParserCtx<Match>, buf: &[u8]) -> String {
    let mut extra_bytes: i64 = 0;
    // We need to copy the buffer because we need to invalidate the
    // variables (otherwise we will count them twice, which is bad when
    // 'extra' is negative) (C comment)
    let mut bufcopy = buf.to_vec();
    for current in &ctx.variables {
        let extra = current.value.len() as i64 - current.key.len() as i64;
        let key = current.key.as_bytes();
        let mut next = 0usize;
        while next < bufcopy.len() {
            let off = match strcasestr(&bufcopy[next..], key) {
                Some(off) => off,
                None => break,
            };
            // We need to invalidate variables completely (otherwise we may
            // count the same variable more than once, thus causing buffer
            // overflow or allocation failure) with spaces (variable names
            // cannot contain spaces) (C comment)
            for b in &mut bufcopy[next + off..next + off + key.len()] {
                *b = b' ';
            }
            extra_bytes += extra;
            next += off + key.len();
        }
    }

    // Then, allocate a new buffer and copy the file over to the new one,
    // but replace occurrences of our variables (C comment)
    let mut new: Vec<u8> =
        Vec::with_capacity((buf.len() as i64 + extra_bytes + 1).max(0) as usize);
    let mut walk = 0usize;
    while walk < buf.len() {
        // Find the next variable
        let mut nearest: Option<usize> = None;
        let mut distance = buf.len(); // C: int distance = stbuf.st_size;
        for (i, current) in ctx.variables.iter().enumerate() {
            if let Some(off) = strcasestr(&buf[walk..], current.key.as_bytes()) {
                if off < distance {
                    distance = off;
                    nearest = Some(i);
                }
            }
        }
        match nearest {
            None => {
                // If there are no more variables, we just copy the rest
                new.extend_from_slice(&buf[walk..]);
                break;
            }
            Some(i) => {
                // Copy until the next variable, then copy its value
                new.extend_from_slice(&buf[walk..walk + distance]);
                new.extend_from_slice(ctx.variables[i].value.as_bytes());
                walk += distance + ctx.variables[i].key.len();
            }
        }
    }
    // C: sstrdup(new) — copies up to the first NUL byte.
    let end = new.iter().position(|&b| b == 0).unwrap_or(new.len());
    String::from_utf8_lossy(&new[..end]).into_owned()
}

/// C: src/config_directives.c:22 `const int ret = wordexp(pattern, &p, 0);`
///
/// The port calls the C library's wordexp(3) exactly like the C code does.
/// A pure-Rust emulation (tilde + glob) cannot reproduce the shell semantics
/// the config relies on: `include ` + backtick-`echo $path` + ``
/// (command substitution) is exercised by testcases/t/313-include.t, and
/// wordexp also expands environment variables inside the pattern.
///
/// Returns the C wordexp error code (0 == success); the caller logs it and
/// marks the config as erroneous (CFGFUN(include), config_directives.c:24-28).
pub fn wordexp(pattern: &str) -> Result<Vec<String>, i32> {
    /// C: <wordexp.h> wordexp_t
    #[repr(C)]
    struct WordexpT {
        we_wordc: usize,
        we_wordv: *mut *mut libc::c_char,
        we_offs: usize,
    }
    extern "C" {
        fn wordexp(
            words: *const libc::c_char,
            pwordexp: *mut WordexpT,
            flags: libc::c_int,
        ) -> libc::c_int;
        fn wordfree(pwordexp: *mut WordexpT);
    }

    // C: the pattern is a NUL terminated string (config_parser hands one over).
    let c_pattern = match std::ffi::CString::new(pattern) {
        Ok(pattern) => pattern,
        Err(_) => return Err(1),
    };
    let mut p = WordexpT {
        we_wordc: 0,
        we_wordv: std::ptr::null_mut(),
        we_offs: 0,
    };
    // C: wordexp(pattern, &p, 0) — flags 0: command substitution, parameter
    // expansion, tilde expansion and pathname expansion are all performed.
    let ret = unsafe { wordexp(c_pattern.as_ptr(), &mut p, 0) };
    if ret != 0 {
        return Err(ret);
    }
    let mut words = Vec::with_capacity(p.we_wordc);
    for i in 0..p.we_wordc {
        // C: char **w = p.we_wordv; for (i = 0; i < p.we_wordc; i++) w[i]
        let word = unsafe { *p.we_wordv.add(i) };
        if word.is_null() {
            continue;
        }
        words.push(
            unsafe { std::ffi::CStr::from_ptr(word) }
                .to_string_lossy()
                .into_owned(),
        );
    }
    // C: wordfree(&p);
    unsafe { wordfree(&mut p) };
    Ok(words)
}

/// C: glob(3) semantics for a single pattern — wildcard expansion over the
/// filesystem, with the leading-dot rule applied by fnmatch.
fn glob(pattern: &str) -> Vec<String> {
    let components: Vec<&str> = pattern.split('/').filter(|c| !c.is_empty()).collect();
    if components.is_empty() {
        return Vec::new();
    }
    let absolute = pattern.starts_with('/');
    let mut prefixes: Vec<String> = vec![if absolute { "/".to_string() } else { String::new() }];
    let mut results: Vec<String> = Vec::new();
    let last = components.len() - 1;
    for (i, component) in components.iter().enumerate() {
        let has_meta = component.contains('*') || component.contains('?') || component.contains('[');
        let mut next: Vec<String> = Vec::new();
        for prefix in &prefixes {
            let dir = if prefix.is_empty() { "." } else { prefix.as_str() };
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if has_meta {
                    if !fnmatch(component.as_bytes(), name.as_bytes()) {
                        continue;
                    }
                } else if name != *component {
                    continue;
                }
                let mut path = prefix.clone();
                if path.is_empty() {
                    // Relative pattern without a directory component.
                    path = name;
                } else {
                    if !path.ends_with('/') {
                        path.push('/');
                    }
                    path.push_str(&name);
                }
                if i == last {
                    results.push(path);
                } else if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    next.push(path);
                }
            }
        }
        prefixes = next;
        // A dead branch in a middle component ends the expansion; the last
        // component collects results instead.
        if prefixes.is_empty() && i + 1 < components.len() {
            return Vec::new();
        }
    }
    results
}

/// C: fnmatch(3) for a single path component — implements *, ?, and [...]
/// character classes with !/^ negation and the leading-dot rule (a leading
/// dot in the name is not matched by wildcards).
pub fn fnmatch(pattern: &[u8], name: &[u8]) -> bool {
    if name.first() == Some(&b'.') && !matches!(pattern.first(), Some(b'.')) {
        return false;
    }
    fn rec(p: &[u8], n: &[u8]) -> bool {
        if p.is_empty() {
            return n.is_empty();
        }
        match p[0] {
            b'*' => {
                if rec(&p[1..], n) {
                    return true;
                }
                (0..n.len()).any(|i| rec(&p[1..], &n[i + 1..]))
            }
            b'?' => !n.is_empty() && rec(&p[1..], &n[1..]),
            b'[' => {
                if n.is_empty() {
                    return false;
                }
                if p.len() < 3 {
                    // unterminated class: fnmatch treats '[' literally
                    return n[0] == b'[' && rec(&p[1..], &n[1..]);
                }
                let neg = matches!(p[1], b'!' | b'^');
                let start = if neg { 2 } else { 1 };
                let mut j = start;
                while j < p.len() && p[j] != b']' {
                    j += 1;
                }
                if j >= p.len() {
                    return n[0] == b'[' && rec(&p[1..], &n[1..]);
                }
                let set = &p[start..j];
                let c = n[0];
                let mut matched = false;
                let mut k = 0;
                while k < set.len() {
                    if k + 2 < set.len() && set[k + 1] == b'-' {
                        if set[k] <= c && c <= set[k + 2] {
                            matched = true;
                        }
                        k += 3;
                    } else {
                        if set[k] == c {
                            matched = true;
                        }
                        k += 1;
                    }
                }
                if neg {
                    matched = !matched;
                }
                if matched {
                    rec(&p[j + 1..], &n[1..])
                } else {
                    false
                }
            }
            c => !n.is_empty() && n[0] == c && rec(&p[1..], &n[1..]),
        }
    }
    rec(pattern, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// The parse_file tests change the process working directory (C:
    /// try_chdir), which is process-global — serialize them like libi3's
    /// env_lock does for environment variables.
    fn cwd_lock() -> MutexGuard<'static, ()> {
        // Shared with every other test module that touches the process cwd
        // (see crate::test_lock).
        crate::test_lock::cwd_lock()
    }

    /// Writes a config file into a fresh temporary directory and returns
    /// (directory, file path).
    fn tmp_config(tag: &str, name: &str, content: &str) -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("i3rs-cfg-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        (dir, path.to_string_lossy().into_owned())
    }

    fn cleanup(dir: &PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// C: upsert_variable (config_parser.c:536-568) — update and the
    /// longest-key-first ordering.
    #[test]
    fn upsert_variable_ordering_and_update() {
        let mut variables = Vec::new();
        upsert_variable(&mut variables, "$ws", "4");
        assert_eq!(variables.len(), 1);
        // Updating an existing key does not add an entry.
        upsert_variable(&mut variables, "$ws", "5");
        assert_eq!(variables.len(), 1);
        assert_eq!(variables[0].value, "5");
        // A longer key is inserted BEFORE shorter keys (longest-key first).
        upsert_variable(&mut variables, "$wspace", "1");
        assert_eq!(variables[0].key, "$wspace");
        assert_eq!(variables[1].key, "$ws");
        // An equal-or-shorter key is inserted AFTER the last longer key.
        upsert_variable(&mut variables, "$ws2", "2");
        assert_eq!(variables[0].key, "$wspace");
        assert_eq!(variables[1].key, "$ws2");
        assert_eq!(variables[2].key, "$ws");
    }

    /// C: strcasestr — case-insensitive substring search.
    #[test]
    fn strcasestr_case_insensitive() {
        assert_eq!(strcasestr(b"Hello $WS World", b"$ws"), Some(6));
        assert_eq!(strcasestr(b"abc", b"xyz"), None);
        assert_eq!(strcasestr(b"abc", b""), Some(0));
    }

    /// C: sscanf semantics — token limits and truncation.
    #[test]
    fn sscanf_truncation() {
        let long = vec![b'a'; 600];
        let mut input = long.clone();
        input.push(b' ');
        input.extend_from_slice(b"value");
        let (tokens, rest) = sscanf_tokens_rest(&input, &[511], 4095);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].len(), 511); // %511s truncates
        // C: after a truncated token the remainder of the token (still
        // non-whitespace) is picked up by the percent-bracket conversion.
        // %[the-line-remainder] reads up to the newline, so the rest of the
        // truncated token plus the trailing " value" is picked up.
        let mut expected = vec![b'a'; 89];
        expected.extend_from_slice(b" value");
        assert_eq!(rest, expected);
        assert_eq!(rest.len(), 600 - 511 + 6);
    }

    /// C: parse_file variable substitution — $vars are replaced, undefined
    /// variables are left as-is (C: only defined keys are substituted).
    #[test]
    fn variable_substitution_and_undefined() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "subst",
            "config",
            "set $ws 4\n\
             set $wsname 9\n\
             bindsym x workspace $ws\n\
             bindsym y workspace $undefined\n\
             bindsym z workspace number $wsname\n",
        );
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        // C: the default mode holds the parsed bindings.
        let default_mode = d.modes.iter().find(|m| m.name == "default").unwrap();
        let commands: Vec<String> = default_mode.bindings.iter().map(|b| b.command.clone()).collect();
        assert!(commands.contains(&"workspace 4".to_string()));
        // C: an undefined variable is not substituted.
        assert!(commands.contains(&"workspace $undefined".to_string()));
        // C: the longer key $wsname wins over the prefix $ws.
        assert!(commands.contains(&"workspace number 9".to_string()));
        // C: the replaced contents are stored on the IncludedFile.
        assert!(file.variable_replaced_contents.contains("workspace 4"));
    }

    /// C: strcasestr substitution quirk — the variable key is matched
    /// case-insensitively in the config text, like the C code does.
    #[test]
    fn variable_substitution_case_insensitive() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "substcase",
            "config",
            "set $Foo bar\n\
             font $foo\n",
        );
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        assert_eq!(d.config.font.pattern, "bar");
    }

    /// C: config_parser.c:677-684 — a line ending in "\\\n" is joined with
    /// the next line before the directive is parsed.
    #[test]
    fn line_continuation() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "cont",
            "config",
            "set $var hello \\\nworld\n\
             font $var\n",
        );
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        assert_eq!(d.config.font.pattern, "hello world");
    }

    /// C: set_from_resource — without an X11 connection the fallback value
    /// is used (C: get_resource returns NULL when conn == NULL).
    #[test]
    fn set_from_resource_uses_fallback() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "res",
            "config",
            "set_from_resource $ws i3wm.color \n\
             bindsym x workspace number $ws\n",
        );
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        let default_mode = d.modes.iter().find(|m| m.name == "default").unwrap();
        // C: fallback[0] = '\0' — the trailing whitespace-only fallback is empty.
        assert!(default_mode
            .bindings
            .iter()
            .any(|b| b.command == "workspace number " || b.command == "workspace number"));
    }

    /// C: config_directives.c:18-85 cfg_include — an included file is parsed
    /// with the outer variable list, listed in included_files and parsed
    /// only once (the second include is skipped).
    #[test]
    fn include_expansion_and_dedup() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "incl",
            "main.conf",
            "include sub.conf\n\
             include sub.conf\n\
             font from-main\n",
        );
        std::fs::write(dir.join("sub.conf"), "font from-sub\n").unwrap();
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        ctx.use_nagbar = false;
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        // C: load_configuration lists the main config file itself; a bare
        // parse_file call only lists the included file.
        let paths: Vec<String> = d.included_files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("sub.conf"));
        // C: the included directives were applied (font from-sub then
        // overwritten by font from-main).
        assert_eq!(d.config.font.pattern, "from-main");
        // C: variables defined in the included file are visible afterwards.
        cleanup(&dir);
    }

    /// C: wordexp/glob replacement — a wildcard pattern is expanded.
    #[test]
    fn include_glob_expansion() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config("glob", "main.conf", "include *.conf\n");
        std::fs::write(dir.join("a.conf"), "font font-a\n").unwrap();
        std::fs::write(dir.join("b.conf"), "font font-b\n").unwrap();
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        assert_eq!(d.config.font.pattern, "font-b"); // sorted glob order
        // A bare parse_file() call does not pre-list the main config (only
        // load_configuration() does, config.c:259), so the pattern also
        // matches the main config itself here; it is parsed once more and its
        // own include is skipped as a duplicate — exactly like the C
        // wordexp/glob flow. The three entries are main.conf, a.conf and the
        // re-included main.conf.
        assert_eq!(d.included_files.len(), 3);
    }

    /// C: src/config.c:259 — load_configuration() inserts the main config
    /// file into included_files *before* parsing it, so it is
    /// TAILQ_FIRST(&included_files). GET_CONFIG returns that entry's
    /// raw_contents as "config" (ipc.c:1288) and GET_VERSION skips it when
    /// building included_config_file_names (ipc.c:1094), so the order is part
    /// of the IPC contract.
    #[test]
    fn load_configuration_lists_main_file_first() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "included-order",
            "main.conf",
            "font pango:monospace 8\ninclude sub.conf\n",
        );
        std::fs::write(dir.join("sub.conf"), "font from-sub\n").unwrap();
        let mut d = ConfigDirectives::new();
        assert!(load_configuration(
            &mut d,
            Some(&path),
            config_load_t::C_LOAD
        ));
        let paths: Vec<String> = d.included_files.iter().map(|f| f.path.clone()).collect();
        let main = std::fs::canonicalize(&path).unwrap().to_string_lossy().into_owned();
        let sub = std::fs::canonicalize(dir.join("sub.conf"))
            .unwrap()
            .to_string_lossy()
            .into_owned();
        cleanup(&dir);
        assert_eq!(paths, vec![main, sub]);
        // C: TAILQ_FIRST(&included_files)->raw_contents is the GET_CONFIG body.
        assert!(d.included_files[0].raw_contents.contains("include sub.conf"));
    }

    /// C: config_directives.c:30-47 — a nonexistent include target is
    /// skipped silently (realpath fails), it is not an error.
    #[test]
    fn include_missing_file_is_skipped() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config("missing", "main.conf", "include does-not-exist.conf\n");
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        assert!(d.included_files.is_empty());
    }

    /// C: config_parser.c:143-440 — a config with a syntax error yields
    /// PARSE_FILE_CONFIG_ERRORS.
    #[test]
    fn parse_file_reports_config_errors() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "err",
            "config",
            "font pango:x\n\
             bogus directive here\n",
        );
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        ctx.use_nagbar = false;
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_CONFIG_ERRORS);
        // C: the valid directive before the error was still applied.
        assert_eq!(d.config.font.pattern, "pango:x");
    }

    /// C: glob/fnmatch — wildcards, character classes and the leading-dot
    /// rule.
    #[test]
    fn fnmatch_basics() {
        assert!(fnmatch(b"i3*", b"i3bar"));
        assert!(fnmatch(b"i3?", b"i3x"));
        assert!(!fnmatch(b"i3?", b"i3xy"));
        assert!(fnmatch(b"i3[ab]", b"i3a"));
        assert!(fnmatch(b"i3[!ab]", b"i3c"));
        // glob(3) leading-dot rule
        assert!(!fnmatch(b"*", b".hidden"));
        assert!(fnmatch(b".*", b".hidden"));
    }

    /// C: config_parser.c:609-628 try_chdir — parse_file changes into the
    /// directory of the config file, so a relative include pattern resolves
    /// against that directory. The pattern itself goes through the variable
    /// substitution of the enclosing file (C: config_parser.c:755-816).
    #[test]
    fn include_pattern_uses_expanded_variables() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "inclvar",
            "main.conf",
            "set $sub sub.conf\n\
             include $sub\n",
        );
        std::fs::write(dir.join("sub.conf"), "font from-sub\n").unwrap();
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        let paths: Vec<String> = d.included_files.iter().map(|f| f.path.clone()).collect();
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        assert_eq!(d.config.font.pattern, "from-sub");
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("sub.conf"), "{:?}", paths);
    }

    /// C: config_directives.c:909-927 — the inner parse_file shares the
    /// variable list with the outer parser, so a variable defined in one
    /// included file is available to the later include directives (and to
    /// their substitution). The includer's own text was substituted before
    /// the parse started, so it still sees the variable as undefined.
    #[test]
    fn variables_from_included_file_are_visible_to_later_includes() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "inclvars",
            "main.conf",
            "include sub.conf\n\
             include use.conf\n\
             font $fromsub\n",
        );
        std::fs::write(dir.join("sub.conf"), "set $fromsub from-sub\n").unwrap();
        std::fs::write(dir.join("use.conf"), "font $fromsub\n").unwrap();
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        // use.conf substituted the variable defined by sub.conf ...
        assert!(ctx.variables.iter().any(|v| v.key == "$fromsub" && v.value == "from-sub"));
        // ... and main.conf's own line could not (its substitution ran before
        // the include was parsed), so the last font directive wins the config
        // with the literal text.
        assert_eq!(d.config.font.pattern, "$fromsub");
    }

    /// C: config_parser.c:319-438 — the error report of an included file names
    /// the included file and its own line numbers, and the enclosing file is
    /// parsed further after the include (the error does not abort it).
    #[test]
    fn error_report_of_included_file_names_its_path() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "inclerr",
            "main.conf",
            "# c\n\
             include bad.conf\n\
             font pango:after\n",
        );
        let bad = dir.join("bad.conf");
        std::fs::write(&bad, "font pango:x\nbogus line\n").unwrap();
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        ctx.use_nagbar = false;
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        let bad_resolved = std::fs::canonicalize(&bad).unwrap().to_string_lossy().into_owned();
        let paths: Vec<String> = d.included_files.iter().map(|f| f.path.clone()).collect();
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_CONFIG_ERRORS);
        // The included file is reported through parse_config with its own
        // filename (C: parse_file_inner passes context->filename = f), see the
        // error line number tests in i3-spec's config_parser.
        assert!(bad_resolved.ends_with("bad.conf"));
        // C: cfg_include removes the file from included_files when parsing it
        // reported errors (remove_file_from_included_list).
        assert!(paths.is_empty(), "{:?}", paths);
        // The includer keeps going after the include.
        assert_eq!(d.config.font.pattern, "pango:after");
    }

    /// C: config_directives.c:876-883 — an include pattern whose target does
    /// not exist is skipped (realpath fails) without a parse error, and an
    /// undefined variable stays in the pattern (so nothing matches).
    #[test]
    fn include_with_undefined_variable_is_skipped() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "inclmissing",
            "main.conf",
            "include $nope/*.conf\n\
             font pango:kept\n",
        );
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        assert!(d.included_files.is_empty());
        assert_eq!(d.config.font.pattern, "pango:kept");
    }

    /// C: config_parser.c:700-816 — the variables of a file are collected and
    /// substituted before the whole file is parsed, so a definition affects
    /// every line of that file, including the lines above it.
    #[test]
    fn variable_definition_applies_to_the_whole_file() {
        let _cwd = cwd_lock();
        let (dir, path) = tmp_config(
            "later",
            "config",
            "font $fonts\n\
             set $fonts pango:late\n\
             font $fonts\n",
        );
        let mut d = ConfigDirectives::new();
        let mut ctx = ConfigParserCtx::<Match>::default();
        let mut file = IncludedFile { path: path.clone(), ..Default::default() };
        let result = parse_file(&mut ctx, &mut d, &path, &mut file);
        let replaced = file.variable_replaced_contents.clone();
        cleanup(&dir);
        assert_eq!(result, ParseFileResult::PARSE_FILE_SUCCESS);
        assert!(replaced.contains("font pango:late"), "{}", replaced);
        assert_eq!(d.config.font.pattern, "pango:late");
    }

    /// C: src/config_parser.c:507-527 — the i3-nagbar argv of a config with
    /// errors: same order and the same strings as the C initializer.
    #[test]
    fn config_error_nagbar_argv_matches_c() {
        let argv = config_error_nagbar_argv(
            "/home/u/.config/i3/config",
            "pango:monospace 8",
            true,
            Some("/run/user/1000/i3/errorlog.abc"),
        );
        assert_eq!(
            argv,
            vec![
                // C: NULL /* will be replaced by the executable path */
                String::new(),
                "-f".to_string(),
                "pango:monospace 8".to_string(),
                "-t".to_string(),
                "error".to_string(),
                "-m".to_string(),
                "You have an error in your i3 config file!".to_string(),
                "-b".to_string(),
                "edit config".to_string(),
                // C: sasprintf(&editaction, "i3-sensible-editor \"%s\" && i3-msg reload\n", configpath)
                "i3-sensible-editor \"/home/u/.config/i3/config\" && i3-msg reload\n".to_string(),
                "-b".to_string(),
                "show errors".to_string(),
                // C: sasprintf(&pageraction, "i3-sensible-pager \"%s\"\n", errorfilename)
                "i3-sensible-pager \"/run/user/1000/i3/errorlog.abc\"\n".to_string(),
            ]
        );
    }

    /// C: src/config_parser.c:513-528 — the warning flavour: type, message and
    /// the second button label change together.
    #[test]
    fn config_error_nagbar_argv_warning_flavour() {
        let argv = config_error_nagbar_argv("/tmp/cfg", "fixed", false, Some("/tmp/err"));
        assert_eq!(argv[4], "warning");
        assert_eq!(
            argv[6],
            "Your config is outdated. Please fix the warnings to make sure everything works."
        );
        assert_eq!(argv[11], "show warnings");
    }

    /// C: src/config_parser.c:522 — `(errorfilename ? "-b" : NULL)` truncates
    /// the argv, so without an error log neither the button nor the pager
    /// action reach the child.
    #[test]
    fn config_error_nagbar_argv_without_errorfile_drops_the_pager_button() {
        let argv = config_error_nagbar_argv("/tmp/cfg", "fixed", true, None);
        assert_eq!(argv.len(), 10);
        assert_eq!(argv[8], "edit config");
        assert!(!argv.iter().any(|a| a == "show errors"));
        assert!(!argv.iter().any(|a| a.contains("i3-sensible-pager")));
    }

    /// C: src/config_parser.c:527 `start_nagbar(&config_error_nagbar_pid, argv)`
    /// — the pid lands in the config state that cmd_reload kills again
    /// (src/commands.c:1671).
    #[test]
    fn start_config_error_nagbar_stores_the_pid_and_passes_the_argv() {
        let mut spawner = crate::util::RecordingNagbarSpawner::default();
        let mut nagbar_pid = -1;
        start_config_error_nagbar(
            &mut spawner,
            &mut nagbar_pid,
            "/tmp/broken.conf",
            "fixed",
            true,
            Some("/tmp/errlog"),
        );
        assert_eq!(nagbar_pid, 2001);
        assert_eq!(spawner.calls.len(), 1);
        assert_eq!(
            spawner.calls[0],
            config_error_nagbar_argv("/tmp/broken.conf", "fixed", true, Some("/tmp/errlog"))
        );
        // A second start is refused while a live pid is set (C: util.c:366-372);
        // the own pid stands in for the running nagbar.
        let mut live_pid = std::process::id() as i32;
        let mut spawner = crate::util::RecordingNagbarSpawner::default();
        start_config_error_nagbar(
            &mut spawner,
            &mut live_pid,
            "/tmp/broken.conf",
            "fixed",
            true,
            Some("/tmp/errlog"),
        );
        assert!(spawner.calls.is_empty());
        assert_eq!(live_pid, std::process::id() as i32);
    }
}
