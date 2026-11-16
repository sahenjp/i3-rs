#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! 1:1 port of i3bar/src/outputs.c (344 lines) — maintains the list of outputs.

use serde_json::Value;

use crate::common::rect;
use crate::workspaces::{free_workspaces, i3_ws};
use crate::xcb::{destroy_window, trayclient, XConnLike};
use crate::{DLOG, ELOG};

/// C: libi3/draw_util.h surface_t — the fields i3bar touches.
///
/// TODO(X11 port): draw_util_surface_init/free (cairo) are not ported yet, so
/// the struct only carries the ids/sizes the layout code needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct surface_t {
    /// C: xcb_drawable_t id
    pub id: u32,
    /// C: int width
    pub width: i32,
    /// C: int height
    pub height: i32,
}

/// C: outputs.h:31 typedef struct i3_output i3_output
#[derive(Default)]
pub struct i3_output {
    /// C: char* name
    pub name: Option<String>,
    /// C: bool active
    pub active: bool,
    /// C: bool primary
    pub primary: bool,
    /// C: bool visible
    pub visible: bool,
    /// C: int ws — the number of the currently visible ws
    pub ws: i32,
    /// C: rect rect — the rect (relative to the root window)
    pub rect: rect,

    /// C: surface_t buffer — off-screen buffer for the bar
    pub buffer: surface_t,
    /// C: surface_t statusline_buffer
    pub statusline_buffer: surface_t,
    /// C: int statusline_width
    pub statusline_width: i32,
    /// C: surface_t bar — the actual window we draw on
    pub bar: surface_t,

    /// C: struct ws_head* workspaces
    pub workspaces: Vec<i3_ws>,
    /// C: struct tc_head* trayclients
    pub trayclients: Vec<trayclient>,

    /// Rust-only stable handle standing in for the C 'struct i3_output *'
    /// pointer identity (the C code compares and stores these pointers).
    pub ptr_id: u32,
}

thread_local! {
    /// C: outputs.c:236 struct outputs_head *outputs;
    static OUTPUTS: std::cell::RefCell<Vec<i3_output>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Rust-only: the next ptr_id to hand out (see i3_output::ptr_id).
    static NEXT_PTR_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}

/// Access to the global output list.
pub fn outputs<R>(f: impl FnOnce(&mut Vec<i3_output>) -> R) -> R {
    OUTPUTS.with(|o| f(&mut o.borrow_mut()))
}

fn next_ptr_id() -> u32 {
    NEXT_PTR_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    })
}

/// C: outputs.c:241 void init_outputs(void)
pub fn init_outputs() {
    OUTPUTS.with(|o| {
        let mut o = o.borrow_mut();
        o.clear();
    });
}

/// C: outputs.c:300 void free_outputs(void)
pub fn free_outputs(x: &mut dyn XConnLike) {
    free_workspaces();

    let count = outputs(|outputs| outputs.len());
    for idx in 0..count {
        destroy_window(x, idx);
        outputs(|outputs| {
            outputs[idx].trayclients.clear();
            outputs[idx].name = None;
            outputs[idx].workspaces.clear();
        });
    }
    outputs(|outputs| outputs.clear());
}

/// C: outputs.c:322 i3_output *get_output_by_name(char *name)
///
/// Returns the index into the global list (the C code returns the pointer; the
/// index is the Rust equivalent — see i3_output::ptr_id for the stable handle).
pub fn get_output_by_name(name: Option<&str>) -> Option<usize> {
    let name = name?;
    let is_primary = libi3::strcasecmp(name, "primary") == 0;

    outputs(|outputs| {
        let mut found = None;
        for (idx, walk) in outputs.iter().enumerate() {
            let same_name = walk.name.as_deref() == Some(name);
            if (is_primary && walk.primary) || same_name {
                found = Some(idx);
                break;
            }
        }
        found
    })
}

/// Look up an output by its ptr_id.
pub fn output_index_by_ptr(id: u32) -> Option<usize> {
    outputs(|outputs| outputs.iter().position(|o| o.ptr_id == id))
}

/// C: outputs.c:337 bool output_has_focus(i3_output *output)
pub fn output_has_focus(output: &i3_output) -> bool {
    output.workspaces.iter().any(|ws| ws.focused)
}

/// C: outputs.c:181 static void clear_output(i3_output *output)
fn clear_output(output: &mut i3_output) {
    output.name = None;
    output.workspaces.clear();
    output.trayclients.clear();
}

/// C: outputs.c:196-234 static int outputs_end_map_cb() — the "do we handle
/// this output?" filter plus the merge with an already known output.
fn outputs_end_map(outputs_walk: i3_output) {
    let num_outputs = crate::config::config(|c| c.num_outputs);
    if num_outputs > 0 {
        let is_primary = outputs_walk.primary;
        let name = outputs_walk.name.clone().unwrap_or_default();
        let mut handle_output = false;
        crate::config::config(|c| {
            for configured in c.outputs.iter() {
                if libi3::strcasecmp(&name, configured) == 0
                    || (libi3::strcasecmp(configured, "primary") == 0 && is_primary)
                    || (libi3::strcasecmp(configured, "nonprimary") == 0 && !is_primary)
                {
                    handle_output = true;
                    break;
                }
            }
        });
        if !handle_output {
            DLOG!("Ignoring output {:?}, not configured to handle it.", name);
            let mut dropped = outputs_walk;
            clear_output(&mut dropped);
            return;
        }
    }

    let target = get_output_by_name(outputs_walk.name.as_deref());
    outputs(|outputs| match target {
        None => {
            // C: SLIST_INSERT_HEAD(outputs, params->outputs_walk, slist)
            outputs.insert(0, outputs_walk);
        }
        Some(idx) => {
            let target = &mut outputs[idx];
            target.active = outputs_walk.active;
            target.primary = outputs_walk.primary;
            target.ws = outputs_walk.ws;
            target.rect = outputs_walk.rect;

            let mut dropped = outputs_walk;
            clear_output(&mut dropped);
        }
    });
}

/// C: outputs.c:249 void parse_outputs_json(const unsigned char* json, size_t size)
pub fn parse_outputs_json(json: &[u8]) {
    let value: Value = match serde_json::from_slice(json) {
        Ok(v) => v,
        Err(_) => {
            // C: case yajl_status_client_canceled / yajl_status_error
            ELOG!("Could not parse outputs reply!");
            std::process::exit(1);
        }
    };

    let items = match value.as_array() {
        Some(a) => a,
        None => {
            ELOG!("Could not parse outputs reply!");
            std::process::exit(1);
        }
    };

    for item in items {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };

        // C: outputs_start_map_cb() with cur_key == NULL — a new output.
        let mut new_output = i3_output { ptr_id: next_ptr_id(), ..Default::default() };
        // C: new_output->ws = 0, statusline_width = 0, surfaces/rect zeroed.

        for (key, val) in obj {
            match key.as_str() {
                // C: outputs_boolean_cb()
                "active" => new_output.active = val.as_bool().unwrap_or(false),
                "primary" => new_output.primary = val.as_bool().unwrap_or(false),
                // C: outputs_integer_cb()
                "current_workspace" => match val {
                    Value::Number(n) => new_output.ws = n.as_i64().unwrap_or(0) as i32,
                    // C: outputs_string_cb() — strtol() with a full-string check.
                    Value::String(s) => {
                        if let Some(num) = parse_strtol(s) {
                            new_output.ws = num as i32;
                        }
                    }
                    _ => {}
                },
                // C: outputs_string_cb() for "name"
                "name" => {
                    if let Value::String(s) = val {
                        new_output.name = Some(s.clone());
                    }
                }
                // C: outputs_start_map_cb() — the nested "rect" object is
                // parsed with in_rect set: its x/y/width/height land in
                // outputs_walk->rect.
                "rect" => {
                    if let Some(rect_obj) = val.as_object() {
                        for (rk, rv) in rect_obj {
                            let num = match rv.as_i64() {
                                Some(n) => n as i32,
                                None => continue,
                            };
                            match rk.as_str() {
                                "x" => new_output.rect.x = num,
                                "y" => new_output.rect.y = num,
                                "width" => new_output.rect.w = num,
                                "height" => new_output.rect.h = num,
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        outputs_end_map(new_output);
    }
}

/// C: strtol(copy, &end, 10) with errno == 0 && *end == '\0'.
fn parse_strtol(s: &str) -> Option<i64> {
    let trimmed = s.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<i64>().ok()
}

/// C: outputs.c:101 outputs_string_cb() — the current_workspace branch.
#[allow(dead_code)]
pub fn current_workspace_from_string(s: &str) -> Option<i32> {
    parse_strtol(s).map(|n| n as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::reset_config;
    use crate::xcb::test_support::NullConn;

    fn setup() {
        reset_config();
        init_outputs();
        crate::common::statusline::reset();
    }

    /// A GET_OUTPUTS reply as i3 4.25 sends it.
    const OUTPUTS_JSON: &str = r#"[
        {"name":"eDP-1","active":true,"primary":true,"rect":{"x":0,"y":0,"width":1920,"height":1080},
         "current_workspace":"1"},
        {"name":"HDMI-1","active":false,"primary":false,"rect":{"x":1920,"y":0,"width":1280,"height":1024},
         "current_workspace":null}
    ]"#;

    #[test]
    fn parse_outputs_creates_and_merges() {
        setup();
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        outputs(|outputs| {
            assert_eq!(outputs.len(), 2);
            // SLIST_INSERT_HEAD: the last parsed output ends up first.
            assert_eq!(outputs[0].name.as_deref(), Some("HDMI-1"));
            assert_eq!(outputs[1].name.as_deref(), Some("eDP-1"));
            assert!(outputs[1].active);
            assert!(outputs[1].primary);
            assert_eq!(outputs[1].ws, 1);
            assert_eq!(outputs[1].rect, rect { x: 0, y: 0, w: 1920, h: 1080 });
            assert!(!outputs[0].active);
            assert_eq!(outputs[0].ws, 0);
        });

        // Re-parsing merges into the existing entries instead of duplicating.
        let before = outputs(|o| o[1].ptr_id);
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        outputs(|outputs| {
            assert_eq!(outputs.len(), 2);
            assert_eq!(outputs[1].ptr_id, before);
        });
    }

    #[test]
    fn parse_outputs_respects_configured_output_list() {
        setup();
        crate::config::config_mut(|c| {
            c.outputs = vec!["HDMI-1".to_string()];
            c.num_outputs = 1;
        });
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        outputs(|outputs| {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].name.as_deref(), Some("HDMI-1"));
        });
    }

    #[test]
    fn parse_outputs_primary_keyword() {
        setup();
        crate::config::config_mut(|c| {
            c.outputs = vec!["primary".to_string()];
            c.num_outputs = 1;
        });
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        outputs(|outputs| {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].name.as_deref(), Some("eDP-1"));
        });
    }

    #[test]
    fn get_output_by_name_and_primary() {
        setup();
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        assert_eq!(get_output_by_name(Some("eDP-1")), Some(1));
        assert_eq!(get_output_by_name(Some("primary")), Some(1));
        assert_eq!(get_output_by_name(Some("nonexistent")), None);
        assert_eq!(get_output_by_name(None), None);
    }

    #[test]
    fn output_has_focus_scans_workspaces() {
        setup();
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        outputs(|outputs| {
            let mut ws = crate::workspaces::i3_ws::default_ws();
            ws.focused = true;
            outputs[1].workspaces.push(ws);
            assert!(output_has_focus(&outputs[1]));
            assert!(!output_has_focus(&outputs[0]));
        });
    }

    #[test]
    fn current_workspace_string_parsing_matches_strtol() {
        // C: outputs_string_cb() only takes the value when the whole string is
        // a number (errno == 0 && end points at the terminator).
        assert_eq!(current_workspace_from_string("3"), Some(3));
        assert_eq!(current_workspace_from_string("3x"), None);
        assert_eq!(current_workspace_from_string(""), None);
        assert_eq!(current_workspace_from_string("12"), Some(12));
    }

    #[test]
    fn free_outputs_clears_list() {
        setup();
        parse_outputs_json(OUTPUTS_JSON.as_bytes());
        let mut x = NullConn::default();
        free_outputs(&mut x);
        outputs(|o| assert!(o.is_empty()));
    }
}
