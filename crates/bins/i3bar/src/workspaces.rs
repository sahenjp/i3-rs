#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! 1:1 port of i3bar/src/workspaces.c (312 lines) — maintains the workspace
//! lists of every output.
//!
//! NOTE on JSON key order: the C code is a yajl callback parser, so the
//! callbacks fire in document order and the "name" handler relies on "num"
//! having been parsed already. serde_json's Value does not preserve document
//! order (its map is sorted), so the two order-dependent keys ("num", "name")
//! are applied first, in the order i3 emits them (see dump_workspaces in
//! i3/src/ipc.c: num, name, visible, focused, urgent, rect, output).

use serde_json::Value;

use libi3::string::{i3String, i3string_from_markup, i3string_from_markup_with_length};

use crate::config::config;
use crate::outputs::{get_output_by_name, outputs};
use crate::xcb::predict_text_width;
use crate::{DLOG, ELOG};

/// C: workspaces.h:27 struct i3_ws
#[derive(Default)]
pub struct i3_ws {
    /// C: uintptr_t id — C pointer to a workspace container
    pub id: u64,
    /// C: int num — the internal number of the ws
    pub num: i32,
    /// C: char *canonical_name — the true name of the ws according to the ipc
    pub canonical_name: Option<String>,
    /// C: i3String *name — the name of the ws displayed on the bar
    pub name: Option<i3String>,
    /// C: int name_width
    pub name_width: i32,
    /// C: bool visible
    pub visible: bool,
    /// C: bool focused
    pub focused: bool,
    /// C: bool urgent
    pub urgent: bool,
    /// C: struct i3_output *output — the current output of the ws
    ///
    /// C stores a pointer; the Rust port stores the output's ptr_id (a stable
    /// handle, see crate::outputs::i3_output::ptr_id).
    pub output: Option<u32>,
}

impl i3_ws {
    /// C: workspaces.c:239 scalloc(1, sizeof(i3_ws)) followed by
    /// new_workspace->num = -1.
    pub fn default_ws() -> i3_ws {
        i3_ws { num: -1, ..Default::default() }
    }

    /// Convenience for tests and DLOG output.
    pub fn name_utf8(&mut self) -> Option<String> {
        self.name.as_mut().and_then(|n| libi3::i3string_as_utf8(n)).map(|s| s.to_string())
    }
}

/// C: workspaces.c:63-118 workspaces_boolean_cb / workspaces_integer_cb
fn apply_boolean(ws: &mut i3_ws, key: &str, val: bool) {
    match key {
        "visible" => ws.visible = val,
        "focused" => ws.focused = val,
        "urgent" => ws.urgent = val,
        _ => {}
    }
}

fn apply_integer(ws: &mut i3_ws, key: &str, val: i64) {
    match key {
        "id" => ws.id = val as u64,
        "num" => ws.num = val as i32,
        // C: the rect members are unused, so they are parsed and discarded.
        "x" | "y" | "width" | "height" => {}
        _ => {}
    }
}

/// C: workspaces.c:127-192 workspaces_string_cb() for "name".
fn apply_name(ws: &mut i3_ws, ws_name: &[u8]) {
    let len = ws_name.len();
    ws.canonical_name = Some(String::from_utf8_lossy(ws_name).into_owned());

    if (config(|c| c.strip_ws_numbers || c.strip_ws_name)) && ws.num >= 0 {
        // Special case: strip off the workspace number/name.
        let ws_num = format!("{}", ws.num);

        // Calculate the length of the number str in the name
        // C: strspn(ws_name, ws_num) — skip the leading characters that occur
        // in ws_num (note: strspn takes a *set* of characters).
        let mut offset = 0usize;
        while offset < len && ws_num.as_bytes().contains(&ws_name[offset]) {
            offset += 1;
        }

        // Also strip off the conventional ws name delimiter
        if offset > 0 && offset < len && ws_name[offset] == b':' {
            offset += 1;
        }

        if config(|c| c.strip_ws_numbers) {
            // Offset may be equal to length, in which case display the number
            ws.name = Some(if offset < len {
                i3string_from_markup_with_length(&ws_name[offset..], len - offset)
            } else {
                i3string_from_markup(&ws_num)
            });
        } else {
            ws.name = Some(i3string_from_markup(&ws_num));
        }
    } else {
        // Default case: just save the name
        ws.name = Some(i3string_from_markup_with_length(ws_name, len));
    }

    // Save its rendered width
    ws.name_width = match ws.name.as_mut() {
        Some(name) => predict_text_width(name),
        None => 0,
    };

    let name_for_log = ws.name_utf8();
    DLOG!(
        "Got workspace canonical: {:?}, name: {:?}, name_width: {}",
        ws.canonical_name,
        name_for_log,
        ws.name_width
    );
}

/// C: workspaces.c:193 workspaces_string_cb() for "output" — looks the output
/// up. The C code inserts the workspace into the output's TAILQ right here;
/// since the callbacks keep mutating the same struct afterwards, the Rust port
/// applies the remaining fields first and appends once at the end (same final
/// state, no aliasing).
fn find_output(output_name: &str) -> Option<usize> {
    get_output_by_name(Some(output_name))
}

/// C: workspaces.c:194-207 workspaces_start_map_cb / :222-261
/// workspaces_end_map_cb — the per-workspace bookkeeping.
fn finish_workspace(ws: i3_ws, need_output: bool) {
    // C: if (params->parsing_rect) { ...; return; }
    //     i3_ws *ws = params->workspaces_walk;
    //     if (!ws || ws->output) return 1; /* workspace already assigned */
    if ws.output.is_some() {
        return;
    }

    // If we processed the output field but did not find the output, the
    // workspace belongs to an output this bar instance does not manage.
    if !need_output {
        return;
    }

    // C: if (!ws->name || SLIST_EMPTY(outputs)) /* Invalid state */
    let outputs_empty = outputs(|outputs| outputs.is_empty());
    if ws.name.is_none() || outputs_empty {
        return;
    }

    // Handle no output case — fallback to primary
    let mut target = get_output_by_name(Some("primary"));
    if target.is_none() {
        target = outputs(|outputs| if outputs.is_empty() { None } else { Some(0) });
    }
    if let Some(idx) = target {
        let ptr_id = outputs(|outputs| outputs[idx].ptr_id);
        let mut moved = ws;
        moved.output = Some(ptr_id);
        outputs(|outputs| outputs[idx].workspaces.push(moved));
    }
}

/// C: workspaces.c:290 void parse_workspaces_json(const unsigned char *json, const size_t size)
pub fn parse_workspaces_json(json: &[u8]) {
    free_workspaces();

    let value: Value = match serde_json::from_slice(json) {
        Ok(v) => v,
        Err(_) => {
            // C: yajl_status_error -> ELOG + (workspace_command ? error button
            // : exit(EXIT_FAILURE))
            ELOG!("Could not parse workspaces reply, json:---{}---", String::from_utf8_lossy(json));
            if config(|c| c.workspace_command.is_some()) {
                crate::child::kill_ws_child();
                crate::child::set_workspace_button_error("Could not parse workspace_command's JSON");
            } else {
                std::process::exit(1);
            }
            return;
        }
    };

    let items = match value.as_array() {
        Some(a) => a,
        None => {
            ELOG!("Could not parse workspaces reply");
            if config(|c| c.workspace_command.is_some()) {
                crate::child::kill_ws_child();
                crate::child::set_workspace_button_error("Could not parse workspace_command's JSON");
            } else {
                std::process::exit(1);
            }
            return;
        }
    };

    for item in items {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };

        // C: workspaces_start_map_cb() — scalloc(1, sizeof(i3_ws)); num = -1
        let mut ws = i3_ws::default_ws();
        let mut need_output = true;

        // 1. "num" first: the "name" handler strips the number off the name.
        if let Some(num) = obj.get("num") {
            if let Some(n) = num.as_i64() {
                apply_integer(&mut ws, "num", n);
            }
        }

        // 2. "name" — uses ws.num, see the module comment.
        if let Some(Value::String(name)) = obj.get("name") {
            apply_name(&mut ws, name.as_bytes());
        }

        // 3. "output" — which output the workspace belongs to.
        let mut found_output: Option<usize> = None;
        if let Some(Value::String(output_name)) = obj.get("output") {
            found_output = find_output(output_name);
            need_output = false;
        }

        // 4. Everything else (id, visible, focused, urgent, rect members).
        for (key, val) in obj {
            match key.as_str() {
                "num" | "name" | "output" => {}
                "id" => {
                    if let Some(n) = val.as_i64() {
                        apply_integer(&mut ws, "id", n);
                    }
                }
                "visible" | "focused" | "urgent" => {
                    apply_boolean(&mut ws, key, val.as_bool().unwrap_or(false));
                }
                "rect" => {
                    if let Some(rect_obj) = val.as_object() {
                        for (rk, rv) in rect_obj {
                            if let Some(n) = rv.as_i64() {
                                apply_integer(&mut ws, rk, n);
                            }
                        }
                    }
                }
                "x" | "y" | "width" | "height" => {
                    if let Some(n) = val.as_i64() {
                        apply_integer(&mut ws, key, n);
                    }
                }
                _ => {}
            }
        }

        // 5. TAILQ_INSERT_TAIL of the end_map callback.
        match found_output {
            Some(idx) => {
                let ptr_id = outputs(|outputs| outputs[idx].ptr_id);
                ws.output = Some(ptr_id);
                outputs(|outputs| outputs[idx].workspaces.push(ws));
            }
            None => finish_workspace(ws, need_output),
        }
    }
}

/// C: workspaces.c:300 void free_workspaces(void)
pub fn free_workspaces() {
    outputs(|outputs| {
        for output in outputs.iter_mut() {
            output.workspaces.clear();
        }
    });
}

/// The workspace that carries a given id, if any (helper for callers that only
/// have the C pointer identity of a workspace).
#[allow(dead_code)]
pub fn output_of_workspace(id: u64) -> Option<usize> {
    outputs(|outputs| {
        outputs
            .iter()
            .position(|o| o.workspaces.iter().any(|ws| ws.id == id && ws.output == Some(o.ptr_id)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{config_mut, reset_config};
    use crate::outputs::{init_outputs, parse_outputs_json};

    const OUTPUTS: &str = r#"[
        {"name":"eDP-1","active":true,"primary":true,"rect":{"x":0,"y":0,"width":1920,"height":1080}},
        {"name":"HDMI-1","active":true,"primary":false,"rect":{"x":1920,"y":0,"width":1280,"height":1024}}
    ]"#;

    /// A GET_WORKSPACES reply as i3 4.25 dumps it.
    const WORKSPACES: &str = r#"[
        {"num":1,"name":"1","visible":true,"focused":true,"urgent":false,
         "rect":{"x":0,"y":0,"width":1920,"height":1080},"output":"eDP-1"},
        {"num":2,"name":"2: www","visible":false,"focused":false,"urgent":true,
         "rect":{"x":0,"y":0,"width":1920,"height":1080},"output":"eDP-1"},
        {"num":10,"name":"10","visible":true,"focused":false,"urgent":false,
         "rect":{"x":1920,"y":0,"width":1280,"height":1024},"output":"HDMI-1"}
    ]"#;

    fn setup() {
        reset_config();
        init_outputs();
        parse_outputs_json(OUTPUTS.as_bytes());
    }

    #[test]
    fn parse_workspaces_and_attach_to_outputs() {
        setup();
        parse_workspaces_json(WORKSPACES.as_bytes());
        outputs(|outputs| {
            let edp_idx = outputs.iter().position(|o| o.name.as_deref() == Some("eDP-1")).unwrap();
            let hdmi_idx = outputs.iter().position(|o| o.name.as_deref() == Some("HDMI-1")).unwrap();
            assert_eq!(outputs[edp_idx].workspaces.len(), 2);
            assert_eq!(outputs[hdmi_idx].workspaces.len(), 1);

            let edp = &mut outputs[edp_idx];
            let ptr_id = edp.ptr_id;
            assert_eq!(edp.workspaces[0].num, 1);
            assert_eq!(edp.workspaces[0].canonical_name.as_deref(), Some("1"));
            assert!(edp.workspaces[0].visible);
            assert!(edp.workspaces[0].focused);
            assert!(!edp.workspaces[0].urgent);
            assert_eq!(edp.workspaces[0].output, Some(ptr_id));
            assert!(edp.workspaces[0].name_width > 0);

            assert_eq!(edp.workspaces[1].num, 2);
            assert_eq!(edp.workspaces[1].canonical_name.as_deref(), Some("2: www"));
            assert!(edp.workspaces[1].urgent);
        });
    }

    #[test]
    fn strip_workspace_numbers() {
        setup();
        config_mut(|c| c.strip_ws_numbers = true);
        parse_workspaces_json(WORKSPACES.as_bytes());
        outputs(|outputs| {
            let edp_idx = outputs.iter().position(|o| o.name.as_deref() == Some("eDP-1")).unwrap();
            let edp = &mut outputs[edp_idx];
            // "1" consists of the number only, so the number is displayed.
            assert_eq!(edp.workspaces[0].name_utf8().as_deref(), Some("1"));
            // "2: www" -> " www": the C code strips the number and the
            // delimiter colon, the space stays part of the workspace name.
            assert_eq!(edp.workspaces[1].name_utf8().as_deref(), Some(" www"));
        });
    }

    #[test]
    fn strip_workspace_name() {
        setup();
        config_mut(|c| c.strip_ws_name = true);
        parse_workspaces_json(WORKSPACES.as_bytes());
        outputs(|outputs| {
            let edp_idx = outputs.iter().position(|o| o.name.as_deref() == Some("eDP-1")).unwrap();
            for ws in outputs[edp_idx].workspaces.iter_mut() {
                let expected = ws.num.to_string();
                assert_eq!(ws.name_utf8().as_deref(), Some(expected.as_str()));
            }
        });
    }

    #[test]
    fn workspace_of_unknown_output_is_dropped() {
        setup();
        let json = r#"[{"num":3,"name":"3","visible":true,"focused":true,"urgent":false,"output":"DP-9"}]"#;
        parse_workspaces_json(json.as_bytes());
        outputs(|outputs| {
            assert!(outputs.iter().all(|o| o.workspaces.is_empty()));
        });
    }

    #[test]
    fn workspace_without_output_falls_back_to_first_output() {
        setup();
        // No "output" key at all -> get_output_by_name("primary") is tried
        // first, then SLIST_FIRST(outputs).
        let json = r#"[{"num":4,"name":"4","visible":true,"focused":true,"urgent":false}]"#;
        parse_workspaces_json(json.as_bytes());
        outputs(|outputs| {
            let primary = outputs.iter().find(|o| o.primary).unwrap();
            assert_eq!(primary.workspaces.len(), 1);
            assert_eq!(primary.workspaces[0].num, 4);
        });
    }

    #[test]
    fn free_workspaces_empties_every_output() {
        setup();
        parse_workspaces_json(WORKSPACES.as_bytes());
        free_workspaces();
        outputs(|outputs| assert!(outputs.iter().all(|o| o.workspaces.is_empty())));
    }
}
