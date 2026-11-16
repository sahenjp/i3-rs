#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! 1:1 port of i3bar/src/config.c (485 lines) — parses the configuration that
//! i3 sends in reply to GET_BAR_CONFIG.
//!
//! C: i3bar/src/config.c
//!
//! The C implementation drives a yajl callback parser keyed on the static
//! cur_key plus the parsing_bindings / parsing_tray_outputs / parsing_padding
//! flags. The Rust port uses serde_json::Value and iterates the resulting tree
//! in the same order, reproducing every branch (including the "got unexpected
//! string" abort) of the callbacks.

use serde_json::Value;

use libi3::string::{i3String, i3string_from_utf8_with_length};

use crate::{DLOG, ELOG};

/// C: configuration.h:17 typedef enum { POS_NONE = 0, POS_TOP, POS_BOT } position_t
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum position_t {
    #[default]
    POS_NONE = 0,
    POS_TOP = 1,
    POS_BOT = 2,
}

/// C: configuration.h:24 typedef enum { M_DOCK = 0, M_HIDE = 1, M_INVISIBLE = 2 } bar_display_mode_t
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum bar_display_mode_t {
    #[default]
    M_DOCK = 0,
    M_HIDE = 1,
    M_INVISIBLE = 2,
}

/// C: configuration.h:88 enum { S_HIDE = 0, S_SHOW = 1 } hidden_state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum hidden_state_t {
    #[default]
    S_HIDE = 0,
    S_SHOW = 1,
}

/// C: configuration.h:26 typedef struct binding_t
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct binding_t {
    pub input_code: i32,
    pub command: Option<String>,
    pub release: bool,
}

/// C: configuration.h:35 typedef struct tray_output_t
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct tray_output_t {
    pub output: String,
}

/// C: configuration.h:43 struct Rect (matches i3/include/data.h)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// C: xcb.h:38 struct xcb_color_strings_t — the color strings as received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct xcb_color_strings_t {
    pub bar_fg: Option<String>,
    pub bar_bg: Option<String>,
    pub sep_fg: Option<String>,
    pub focus_bar_fg: Option<String>,
    pub focus_bar_bg: Option<String>,
    pub focus_sep_fg: Option<String>,
    pub active_ws_fg: Option<String>,
    pub active_ws_bg: Option<String>,
    pub active_ws_border: Option<String>,
    pub inactive_ws_fg: Option<String>,
    pub inactive_ws_bg: Option<String>,
    pub inactive_ws_border: Option<String>,
    pub focus_ws_bg: Option<String>,
    pub focus_ws_fg: Option<String>,
    pub focus_ws_border: Option<String>,
    pub urgent_ws_bg: Option<String>,
    pub urgent_ws_fg: Option<String>,
    pub urgent_ws_border: Option<String>,
    pub binding_mode_bg: Option<String>,
    pub binding_mode_fg: Option<String>,
    pub binding_mode_border: Option<String>,
}

/// C: configuration.h:56 typedef struct config_t
///
/// (No Debug: separator_symbol is an opaque libi3 i3String.)
pub struct config_t {
    pub modifier: u32,
    pub bindings: Vec<binding_t>,
    pub position: position_t,
    pub verbose: bool,
    pub bar_height: u32,
    pub padding: Rect,
    pub transparency: bool,
    pub colors: xcb_color_strings_t,
    pub disable_binding_mode_indicator: bool,
    pub disable_ws: bool,
    pub ws_min_width: i32,
    pub strip_ws_numbers: bool,
    pub strip_ws_name: bool,
    pub bar_id: Option<String>,
    pub command: Option<String>,
    pub workspace_command: Option<String>,
    pub fontname: Option<String>,
    pub separator_symbol: Option<i3String>,
    pub tray_outputs: Vec<tray_output_t>,
    pub tray_padding: i32,
    pub num_outputs: i32,
    pub outputs: Vec<String>,

    pub hide_on_modifier: bar_display_mode_t,

    /// C: enum { S_HIDE = 0, S_SHOW = 1 } hidden_state
    pub hidden_state: hidden_state_t,
}

impl Default for config_t {
    fn default() -> config_t {
        // C: main.c:120 memset(&config, '\0', sizeof(config_t)) — every scalar
        // is zero, every pointer NULL.
        config_t {
            modifier: 0,
            bindings: Vec::new(),
            position: position_t::POS_NONE,
            verbose: false,
            bar_height: 0,
            padding: Rect::default(),
            transparency: false,
            colors: xcb_color_strings_t::default(),
            disable_binding_mode_indicator: false,
            disable_ws: false,
            ws_min_width: 0,
            strip_ws_numbers: false,
            strip_ws_name: false,
            bar_id: None,
            command: None,
            workspace_command: None,
            fontname: None,
            separator_symbol: None,
            tray_outputs: Vec::new(),
            tray_padding: 0,
            num_outputs: 0,
            outputs: Vec::new(),
            hide_on_modifier: bar_display_mode_t::M_DOCK,
            hidden_state: hidden_state_t::S_HIDE,
        }
    }
}

thread_local! {
    /// C: config.c:19 config_t config;
    static CONFIG: std::cell::RefCell<config_t> = std::cell::RefCell::new(config_t::default());
}

/// Read access to the global config struct.
pub fn config<R>(f: impl FnOnce(&config_t) -> R) -> R {
    CONFIG.with(|c| f(&c.borrow()))
}

/// Mutable access to the global config struct.
pub fn config_mut<R>(f: impl FnOnce(&mut config_t) -> R) -> R {
    CONFIG.with(|c| f(&mut c.borrow_mut()))
}

/// Reset the global to its memset('\0') state (test helper).
pub fn reset_config() {
    CONFIG.with(|c| *c.borrow_mut() = config_t::default());
}

/// C: config.c:253 COLOR(json_name, struct_name) — the key to struct field map.
fn color_slot<'a>(colors: &'a mut xcb_color_strings_t, key: &str) -> Option<&'a mut Option<String>> {
    Some(match key {
        "statusline" => &mut colors.bar_fg,
        "background" => &mut colors.bar_bg,
        "separator" => &mut colors.sep_fg,
        "focused_statusline" => &mut colors.focus_bar_fg,
        "focused_background" => &mut colors.focus_bar_bg,
        "focused_separator" => &mut colors.focus_sep_fg,
        "focused_workspace_border" => &mut colors.focus_ws_border,
        "focused_workspace_bg" => &mut colors.focus_ws_bg,
        "focused_workspace_text" => &mut colors.focus_ws_fg,
        "active_workspace_border" => &mut colors.active_ws_border,
        "active_workspace_bg" => &mut colors.active_ws_bg,
        "active_workspace_text" => &mut colors.active_ws_fg,
        "inactive_workspace_border" => &mut colors.inactive_ws_border,
        "inactive_workspace_bg" => &mut colors.inactive_ws_bg,
        "inactive_workspace_text" => &mut colors.inactive_ws_fg,
        "urgent_workspace_border" => &mut colors.urgent_ws_border,
        "urgent_workspace_bg" => &mut colors.urgent_ws_bg,
        "urgent_workspace_text" => &mut colors.urgent_ws_fg,
        "binding_mode_border" => &mut colors.binding_mode_border,
        "binding_mode_bg" => &mut colors.binding_mode_bg,
        "binding_mode_text" => &mut colors.binding_mode_fg,
        _ => return None,
    })
}

/// C: config.c:333-339 — the "unknown string" path aborts the parse.
fn unexpected_string(val: &str, cur_key: &str) -> ! {
    // C: printf("got unexpected string %.*s for cur_key = %s\n", ...); return 0;
    // -> yajl_status_error -> ELOG + exit(EXIT_FAILURE).
    println!("got unexpected string {} for cur_key = {}", val, cur_key);
    ELOG!("Could not parse config reply!");
    std::process::exit(1);
}

/// C: config.c:340 void parse_config_json(const unsigned char *json, size_t size)
pub fn parse_config_json(json: &[u8]) {
    // C: TAILQ_INIT(&(config.bindings)); TAILQ_INIT(&(config.tray_outputs));
    config_mut(|c| {
        c.bindings.clear();
        c.tray_outputs.clear();
    });

    let value: Value = match serde_json::from_slice(json) {
        Ok(v) => v,
        Err(_) => {
            // C: case yajl_status_client_canceled / yajl_status_error
            ELOG!("Could not parse config reply!");
            std::process::exit(1);
        }
    };

    let object = match value.as_object() {
        Some(o) => o,
        None => {
            ELOG!("Could not parse config reply!");
            std::process::exit(1);
        }
    };

    for (key, val) in object {
        let cur_key = key.as_str();

        // C: config_null_cb() — "id": null means the bar config was not found.
        if let Value::Null = val {
            if cur_key == "id" {
                ELOG!("No such bar config. Use 'i3-msg -t get_bar_config' to get the available configs.");
                ELOG!("Are you starting i3bar by hand? You should not:");
                ELOG!("Configure a 'bar' block in your i3 config and i3 will launch i3bar automatically.");
                std::process::exit(1);
            }
            continue;
        }

        // C: config_string_cb()
        if let Value::String(s) = val {
            let len = s.len();
            let bytes = s.as_bytes();
            let val_str = s.as_str();

            match cur_key {
                // C: "The id and socket_path are ignored, we already know them."
                "id" | "socket_path" => continue,
                "mode" => {
                    DLOG!("mode = {}, len = {}", val_str, len);
                    let mode = if len == "dock".len() && bytes.starts_with(b"dock") {
                        bar_display_mode_t::M_DOCK
                    } else if len == "hide".len() && bytes.starts_with(b"hide") {
                        bar_display_mode_t::M_HIDE
                    } else {
                        bar_display_mode_t::M_INVISIBLE
                    };
                    config_mut(|c| c.hide_on_modifier = mode);
                    continue;
                }
                "hidden_state" => {
                    DLOG!("hidden_state = {}, len = {}", val_str, len);
                    let state = if len == "hide".len() && bytes.starts_with(b"hide") {
                        hidden_state_t::S_HIDE
                    } else {
                        hidden_state_t::S_SHOW
                    };
                    config_mut(|c| c.hidden_state = state);
                    continue;
                }
                "modifier" => {
                    DLOG!("modifier = {}", val_str);
                    let modifier = if len == "none".len() && bytes.starts_with(b"none") {
                        XCB_NONE
                    } else if len == "shift".len() && bytes.starts_with(b"shift") {
                        XCB_MOD_MASK_SHIFT
                    } else if len == "ctrl".len() && bytes.starts_with(b"ctrl") {
                        XCB_MOD_MASK_CONTROL
                    } else if len == "Mod".len() + 1 && bytes.starts_with(b"Mod") {
                        match bytes[3] {
                            b'1' => XCB_MOD_MASK_1,
                            b'2' => XCB_MOD_MASK_2,
                            b'3' => XCB_MOD_MASK_3,
                            b'5' => XCB_MOD_MASK_5,
                            // C: falls through to the default below.
                            _ => XCB_MOD_MASK_4,
                        }
                    } else {
                        // C: config.modifier = XCB_MOD_MASK_4; return 1;
                        XCB_MOD_MASK_4
                    };
                    config_mut(|c| c.modifier = modifier);
                    continue;
                }
                "wheel_up_cmd" => {
                    DLOG!("wheel_up_cmd = {}", val_str);
                    config_mut(|c| {
                        c.bindings.push(binding_t {
                            input_code: 4,
                            command: Some(val_str.to_string()),
                            release: false,
                        })
                    });
                    continue;
                }
                "wheel_down_cmd" => {
                    DLOG!("wheel_down_cmd = {}", val_str);
                    config_mut(|c| {
                        c.bindings.push(binding_t {
                            input_code: 5,
                            command: Some(val_str.to_string()),
                            release: false,
                        })
                    });
                    continue;
                }
                "position" => {
                    DLOG!("position = {}", val_str);
                    let position = if len == "top".len() && bytes.starts_with(b"top") {
                        position_t::POS_TOP
                    } else {
                        position_t::POS_BOT
                    };
                    config_mut(|c| c.position = position);
                    continue;
                }
                "status_command" => {
                    DLOG!("status_command = {}", val_str);
                    config_mut(|c| c.command = Some(val_str.to_string()));
                    continue;
                }
                "workspace_command" => {
                    DLOG!("workspace_command = {}", val_str);
                    config_mut(|c| c.workspace_command = Some(val_str.to_string()));
                    continue;
                }
                "font" => {
                    DLOG!("font = {}", val_str);
                    config_mut(|c| c.fontname = Some(val_str.to_string()));
                    continue;
                }
                "separator_symbol" => {
                    DLOG!("separator = {}", val_str);
                    config_mut(|c| {
                        c.separator_symbol = Some(i3string_from_utf8_with_length(bytes, len as isize))
                    });
                    continue;
                }
                "outputs" => {
                    // C: config_string_cb() is called once per array element with
                    // cur_key == "outputs".
                    DLOG!("+output {}", val_str);
                    config_mut(|c| {
                        c.outputs.push(val_str.to_string());
                        c.num_outputs += 1;
                    });
                    continue;
                }
                "tray_output" => {
                    DLOG!("Found deprecated key tray_output {}.", val_str);
                    config_mut(|c| c.tray_outputs.push(tray_output_t { output: val_str.to_string() }));
                    continue;
                }
                _ => {
                    let handled = config_mut(|c| {
                        if let Some(slot) = color_slot(&mut c.colors, cur_key) {
                            *slot = Some(val_str.to_string());
                            true
                        } else {
                            false
                        }
                    });
                    if handled {
                        continue;
                    }
                    unexpected_string(val_str, cur_key);
                }
            }
        }

        // C: config_boolean_cb()
        if let Value::Bool(b) = val {
            let val = if *b { 1 } else { 0 };
            match cur_key {
                "binding_mode_indicator" => {
                    DLOG!("binding_mode_indicator = {}", val);
                    config_mut(|c| c.disable_binding_mode_indicator = val == 0);
                    continue;
                }
                "workspace_buttons" => {
                    DLOG!("workspace_buttons = {}", val);
                    config_mut(|c| c.disable_ws = val == 0);
                    continue;
                }
                "strip_workspace_numbers" => {
                    DLOG!("strip_workspace_numbers = {}", val);
                    config_mut(|c| c.strip_ws_numbers = val != 0);
                    continue;
                }
                "strip_workspace_name" => {
                    DLOG!("strip_workspace_name = {}", val);
                    config_mut(|c| c.strip_ws_name = val != 0);
                    continue;
                }
                "verbose" => {
                    // C: if (!config.verbose) { DLOG(...); config.verbose = val; }
                    if !config(|c| c.verbose) {
                        DLOG!("verbose = {}", val);
                        config_mut(|c| c.verbose = val != 0);
                    }
                    continue;
                }
                _ => continue,
            }
        }

        // C: config_integer_cb()
        if let Value::Number(n) = val {
            let ival = n.as_i64().unwrap_or(0);
            match cur_key {
                "bar_height" => {
                    DLOG!("bar_height = {}", ival);
                    config_mut(|c| c.bar_height = ival as u32);
                    continue;
                }
                "tray_padding" => {
                    DLOG!("tray_padding = {}", ival);
                    config_mut(|c| c.tray_padding = ival as i32);
                    continue;
                }
                "modifier" => {
                    DLOG!("modifier = {}", ival);
                    config_mut(|c| c.modifier = ival as u32);
                    continue;
                }
                "workspace_min_width" => {
                    DLOG!("workspace_min_width = {}", ival);
                    config_mut(|c| c.ws_min_width = ival as i32);
                    continue;
                }
                _ => continue,
            }
        }

        // C: the flag-driven branches (parsing_bindings / parsing_tray_outputs
        // / parsing_padding).
        if cur_key == "bindings" {
            if let Value::Array(items) = val {
                for item in items {
                    let obj = match item.as_object() {
                        Some(o) => o,
                        None => continue,
                    };
                    let input_code = obj.get("input_code").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    let command = obj.get("command").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let release = obj.get("release").and_then(|v| v.as_bool()).unwrap_or(false);
                    config_mut(|c| {
                        if let Some(existing) = c.bindings.last_mut() {
                            if existing.input_code == input_code && existing.command.is_some() {
                                ELOG!(
                                    "The binding for input_code = {} already has a command. This is a bug in i3.",
                                    input_code
                                );
                                std::process::exit(1);
                            }
                        }
                        c.bindings.push(binding_t { input_code, command, release });
                    });
                }
            }
            continue;
        }

        if cur_key == "colors" {
            // C: yajl descends into the nested map, so the color callbacks fire
            // with cur_key set to the inner key.
            if let Value::Object(colors) = val {
                for (color_key, color_val) in colors {
                    if let Value::String(color_val) = color_val {
                        let handled = config_mut(|c| {
                            if let Some(slot) = color_slot(&mut c.colors, color_key.as_str()) {
                                *slot = Some(color_val.clone());
                                true
                            } else {
                                false
                            }
                        });
                        if !handled {
                            unexpected_string(color_val, color_key);
                        }
                    }
                }
            }
            continue;
        }

        if cur_key == "outputs" {
            // C: config_string_cb() is called once per array element with
            // cur_key == "outputs" (see also the plain-string branch above).
            if let Value::Array(items) = val {
                for item in items {
                    if let Value::String(s) = item {
                        DLOG!("+output {}", s);
                        config_mut(|c| {
                            c.outputs.push(s.to_string());
                            c.num_outputs += 1;
                        });
                    }
                }
            }
            continue;
        }

        if cur_key == "tray_outputs" {
            if let Value::Array(items) = val {
                for item in items {
                    if let Value::String(s) = item {
                        DLOG!("Adding tray_output = {} to the list.", s);
                        config_mut(|c| c.tray_outputs.push(tray_output_t { output: s.to_string() }));
                    }
                }
            }
            continue;
        }

        if cur_key == "padding" {
            if let Value::Object(obj) = val {
                for (k, v) in obj {
                    let ival = match v.as_i64() {
                        Some(n) => n,
                        None => continue,
                    };
                    match k.as_str() {
                        "x" => {
                            DLOG!("padding.x = {}", ival);
                            config_mut(|c| c.padding.x = ival as u32);
                        }
                        "y" => {
                            DLOG!("padding.y = {}", ival);
                            config_mut(|c| c.padding.y = ival as u32);
                        }
                        "width" => {
                            DLOG!("padding.width = {}", ival);
                            config_mut(|c| c.padding.width = ival as u32);
                        }
                        "height" => {
                            DLOG!("padding.height = {}", ival);
                            config_mut(|c| c.padding.height = ival as u32);
                        }
                        _ => {}
                    }
                }
            }
            continue;
        }
    }

    // C: config.c:361 — 'workspace_buttons no' wins over workspace_command.
    config_mut(|c| {
        if c.disable_ws && c.workspace_command.is_some() {
            ELOG!(
                "You have specified 'workspace_buttons no'. Your 'workspace_command {}' will be ignored.",
                c.workspace_command.clone().unwrap_or_default()
            );
            c.workspace_command = None;
        }
    });
}

/// C: config.c:378 void parse_get_first_i3bar_config(const unsigned char *json, size_t size)
pub fn parse_get_first_i3bar_config(json: &[u8]) {
    // C: i3bar_config_string_cb() stores the first string seen, then returns 0
    // (stop parsing).
    let value: Value = match serde_json::from_slice(json) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(first) = first_string(&value) {
        config_mut(|c| c.bar_id = Some(first));
    }
}

fn first_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.iter().find_map(first_string),
        Value::Object(map) => map.values().find_map(first_string),
        _ => None,
    }
}

/// C: config.c:412 void free_colors(struct xcb_color_strings_t *colors)
pub fn free_colors(colors: &mut xcb_color_strings_t) {
    colors.bar_fg = None;
    colors.bar_bg = None;
    colors.sep_fg = None;
    colors.focus_bar_fg = None;
    colors.focus_bar_bg = None;
    colors.focus_sep_fg = None;
    colors.active_ws_fg = None;
    colors.active_ws_bg = None;
    colors.active_ws_border = None;
    colors.inactive_ws_fg = None;
    colors.inactive_ws_bg = None;
    colors.inactive_ws_border = None;
    colors.urgent_ws_fg = None;
    colors.urgent_ws_bg = None;
    colors.urgent_ws_border = None;
    colors.focus_ws_fg = None;
    colors.focus_ws_bg = None;
    colors.focus_ws_border = None;
    colors.binding_mode_fg = None;
    colors.binding_mode_bg = None;
    colors.binding_mode_border = None;
}

// C: xcb.h — the modifier masks named in the "modifier" config directive.
pub const XCB_NONE: u32 = 0;
/// C: xcb.h XCB_MOD_MASK_SHIFT
pub const XCB_MOD_MASK_SHIFT: u32 = 1 << 0;
/// C: xcb.h XCB_MOD_MASK_CONTROL
pub const XCB_MOD_MASK_CONTROL: u32 = 1 << 2;
/// C: xcb.h XCB_MOD_MASK_1
pub const XCB_MOD_MASK_1: u32 = 1 << 3;
/// C: xcb.h XCB_MOD_MASK_2
pub const XCB_MOD_MASK_2: u32 = 1 << 4;
/// C: xcb.h XCB_MOD_MASK_3
pub const XCB_MOD_MASK_3: u32 = 1 << 5;
/// C: xcb.h XCB_MOD_MASK_4
pub const XCB_MOD_MASK_4: u32 = 1 << 6;
/// C: xcb.h XCB_MOD_MASK_5
pub const XCB_MOD_MASK_5: u32 = 1 << 7;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::statusline;

    /// The bar config i3 4.25 sends for a default bar block plus a few
    /// directives (field names taken from i3/src/ipc.c dump_bar_config).
    const BAR_CONFIG: &str = r##"{
        "id": "bar-0",
        "mode": "dock",
        "hidden_state": "hide",
        "position": "top",
        "status_command": "i3status",
        "font": "pango:monospace 8",
        "workspace_buttons": true,
        "workspace_min_width": 0,
        "strip_workspace_numbers": false,
        "strip_workspace_name": false,
        "binding_mode_indicator": true,
        "verbose": false,
        "colors": {
            "background": "#000000",
            "statusline": "#ffffff",
            "separator": "#666666",
            "focused_workspace_text": "#ffffff",
            "focused_workspace_bg": "#285577",
            "focused_workspace_border": "#4c7899",
            "active_workspace_text": "#ffffff",
            "active_workspace_bg": "#333333",
            "active_workspace_border": "#333333",
            "inactive_workspace_text": "#888888",
            "inactive_workspace_bg": "#222222",
            "inactive_workspace_border": "#333333",
            "urgent_workspace_text": "#ffffff",
            "urgent_workspace_bg": "#900000",
            "urgent_workspace_border": "#2f343a",
            "binding_mode_text": "#ffffff",
            "binding_mode_bg": "#900000",
            "binding_mode_border": "#2f343a"
        },
        "tray_output": "primary",
        "tray_padding": 2,
        "padding": {"x": 1, "y": 2, "width": 3, "height": 4},
        "bar_height": 0,
        "separator_symbol": "|",
        "outputs": ["HDMI-1", "eDP-1"]
    }"##;

    fn bar_config_with_extra(extra: &str) -> String {
        // Insert extra top-level keys into BAR_CONFIG (before the last '}').
        let mut s = BAR_CONFIG.trim_end().to_string();
        assert!(s.ends_with('}'));
        s.pop();
        s.push(',');
        s.push_str(extra);
        s.push('}');
        s
    }

    #[test]
    fn parse_full_bar_config() {
        reset_config();
        parse_config_json(BAR_CONFIG.as_bytes());
        config(|c| {
            assert_eq!(c.hide_on_modifier, bar_display_mode_t::M_DOCK);
            assert_eq!(c.hidden_state, hidden_state_t::S_HIDE);
            assert_eq!(c.position, position_t::POS_TOP);
            assert_eq!(c.command.as_deref(), Some("i3status"));
            assert_eq!(c.fontname.as_deref(), Some("pango:monospace 8"));
            assert_eq!(c.ws_min_width, 0);
            assert_eq!(c.padding, Rect { x: 1, y: 2, width: 3, height: 4 });
            assert_eq!(c.tray_outputs, vec![tray_output_t { output: "primary".into() }]);
            assert_eq!(c.tray_padding, 2);
            assert_eq!(c.outputs, vec!["HDMI-1".to_string(), "eDP-1".to_string()]);
            assert_eq!(c.num_outputs, 2);
            assert_eq!(c.colors.bar_bg.as_deref(), Some("#000000"));
            assert_eq!(c.colors.bar_fg.as_deref(), Some("#ffffff"));
            assert_eq!(c.colors.focus_ws_border.as_deref(), Some("#4c7899"));
            assert_eq!(c.colors.binding_mode_fg.as_deref(), Some("#ffffff"));
            assert!(c.separator_symbol.is_some());
            assert!(!c.disable_ws);
            assert!(!c.disable_binding_mode_indicator);
            assert_eq!(c.bar_id, None); // "id" is ignored by config_string_cb
        });
    }

    #[test]
    fn parse_mode_and_bindings() {
        reset_config();
        let json = bar_config_with_extra(
            r#""mode": "hide",
               "modifier": "Mod4",
               "bindings": [
                   {"input_code": 1, "command": "nop", "release": false},
                   {"input_code": 3, "command": "focus left", "release": true}
               ],
               "wheel_up_cmd": "workspace prev",
               "wheel_down_cmd": "workspace next""#,
        );
        parse_config_json(json.as_bytes());
        config(|c| {
            assert_eq!(c.hide_on_modifier, bar_display_mode_t::M_HIDE);
            assert_eq!(c.modifier, XCB_MOD_MASK_4);
            // NOTE: the C parser appends in document order; serde_json's map is
            // sorted, so the two wheel_* bindings may swap places. The bindings
            // are looked up by input_code, so this is not observable.
            let mut bindings = c.bindings.clone();
            bindings.sort_by_key(|b| b.input_code);
            assert_eq!(
                bindings,
                vec![
                    binding_t { input_code: 1, command: Some("nop".into()), release: false },
                    binding_t { input_code: 3, command: Some("focus left".into()), release: true },
                    binding_t { input_code: 4, command: Some("workspace prev".into()), release: false },
                    binding_t { input_code: 5, command: Some("workspace next".into()), release: false },
                ]
            );
        });
    }

    #[test]
    fn parse_mode_invisible_and_modifier_variants() {
        reset_config();
        parse_config_json(br#"{"mode": "invisible", "modifier": "shift"}"#);
        config(|c| {
            assert_eq!(c.hide_on_modifier, bar_display_mode_t::M_INVISIBLE);
            assert_eq!(c.modifier, XCB_MOD_MASK_SHIFT);
        });

        reset_config();
        parse_config_json(br#"{"modifier": "ctrl"}"#);
        config(|c| assert_eq!(c.modifier, XCB_MOD_MASK_CONTROL));

        reset_config();
        parse_config_json(br#"{"modifier": "none"}"#);
        config(|c| assert_eq!(c.modifier, XCB_NONE));

        reset_config();
        // An unknown "ModN" falls through to Mod4 (C: switch(val[3]) default).
        parse_config_json(br#"{"modifier": "Mod9"}"#);
        config(|c| assert_eq!(c.modifier, XCB_MOD_MASK_4));

        reset_config();
        parse_config_json(br#"{"modifier": 12}"#);
        config(|c| assert_eq!(c.modifier, 12));
    }

    #[test]
    fn parse_booleans_and_hidden_state() {
        reset_config();
        parse_config_json(
            br#"{"hidden_state": "show", "workspace_buttons": false, "binding_mode_indicator": false,
                 "strip_workspace_numbers": true, "strip_workspace_name": true}"#,
        );
        config(|c| {
            assert_eq!(c.hidden_state, hidden_state_t::S_SHOW);
            assert!(c.disable_ws);
            assert!(c.disable_binding_mode_indicator);
            assert!(c.strip_ws_numbers);
            assert!(c.strip_ws_name);
        });
    }

    #[test]
    fn workspace_command_dropped_when_workspace_buttons_disabled() {
        reset_config();
        parse_config_json(br#"{"workspace_buttons": false, "workspace_command": "my-ws-client"}"#);
        config(|c| {
            assert!(c.disable_ws);
            assert_eq!(c.workspace_command, None);
        });

        reset_config();
        parse_config_json(br#"{"workspace_buttons": true, "workspace_command": "my-ws-client"}"#);
        config(|c| assert_eq!(c.workspace_command.as_deref(), Some("my-ws-client")));
    }

    #[test]
    fn parse_get_first_i3bar_config_takes_first_string() {
        reset_config();
        parse_get_first_i3bar_config(br#"["bar-0", "bar-1"]"#);
        config(|c| assert_eq!(c.bar_id.as_deref(), Some("bar-0")));
    }

    #[test]
    fn free_colors_clears_every_field() {
        let mut colors = xcb_color_strings_t {
            bar_fg: Some("#fff".into()),
            bar_bg: Some("#000".into()),
            sep_fg: Some("#666".into()),
            focus_bar_fg: Some("#fff".into()),
            focus_bar_bg: Some("#000".into()),
            focus_sep_fg: Some("#666".into()),
            active_ws_fg: Some("#fff".into()),
            active_ws_bg: Some("#333".into()),
            active_ws_border: Some("#333".into()),
            inactive_ws_fg: Some("#888".into()),
            inactive_ws_bg: Some("#222".into()),
            inactive_ws_border: Some("#333".into()),
            focus_ws_bg: Some("#285577".into()),
            focus_ws_fg: Some("#fff".into()),
            focus_ws_border: Some("#4c7899".into()),
            urgent_ws_bg: Some("#900000".into()),
            urgent_ws_fg: Some("#fff".into()),
            urgent_ws_border: Some("#2f343a".into()),
            binding_mode_bg: Some("#900000".into()),
            binding_mode_fg: Some("#fff".into()),
            binding_mode_border: Some("#2f343a".into()),
        };
        free_colors(&mut colors);
        assert_eq!(colors, xcb_color_strings_t::default());
    }

    #[test]
    fn statusline_global_is_reachable() {
        // The C global statusline_head lives in child.c; the port keeps it in
        // common::statusline so config/child/xcb share it.
        statusline::reset();
        statusline::head(|h| assert!(h.is_empty()));
    }
}
