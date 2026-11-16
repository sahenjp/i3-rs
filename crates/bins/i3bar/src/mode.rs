#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! 1:1 port of i3bar/src/mode.c (132 lines) — handles the "mode" event and
//! shows the current binding mode in the bar.

use serde_json::Value;

use libi3::string::{i3String, i3string_as_utf8, i3string_from_utf8, i3string_set_markup};

use crate::xcb::{predict_text_width, set_current_mode};
use crate::{DLOG, ELOG};

/// C: mode.h:18 struct mode — name of the current binding mode and its render
/// width.
#[derive(Default)]
pub struct mode {
    pub name: Option<i3String>,
    pub name_width: i32,
}

impl mode {
    /// Convenience for tests and DLOG output.
    pub fn name_utf8(&mut self) -> Option<String> {
        self.name.as_mut().and_then(|n| i3string_as_utf8(n)).map(|s| s.to_string())
    }
}

/// C: mode.c:23 struct mode_json_params — the parse state.
#[derive(Debug, Default)]
struct mode_json_params {
    cur_key: Option<String>,
    name: Option<String>,
    pango_markup: bool,
}

/// C: mode.c:112 void parse_mode_json(const unsigned char *json, size_t size)
pub fn parse_mode_json(json: &[u8]) {
    let mut params = mode_json_params::default();

    let value: Value = match serde_json::from_slice(json) {
        Ok(v) => v,
        Err(_) => {
            // C: case yajl_status_client_canceled / yajl_status_error
            ELOG!("Could not parse mode event!");
            std::process::exit(1);
        }
    };

    if let Some(obj) = value.as_object() {
        for (key, val) in obj {
            // C: mode_map_key_cb() keeps the key in params->cur_key.
            params.cur_key = Some(key.clone());

            // C: mode_string_cb() — only "change" is accepted.
            if let Value::String(s) = val {
                if params.cur_key.as_deref() == Some("change") {
                    params.name = Some(s.clone());
                }
            }

            // C: mode_boolean_cb() — only "pango_markup" is accepted.
            if let Value::Bool(b) = val {
                if params.cur_key.as_deref() == Some("pango_markup") {
                    DLOG!("Setting pango_markup to {}.", if *b { 1 } else { 0 });
                    params.pango_markup = *b;
                }
            }
        }
    }

    // C: mode_end_map_cb() — Save the name and its rendered width.
    let mut binding = mode {
        name: params.name.as_deref().map(i3string_from_utf8),
        name_width: 0,
    };
    if let Some(name) = binding.name.as_mut() {
        i3string_set_markup(name, params.pango_markup);
    }
    if let Some(name) = binding.name.as_mut() {
        binding.name_width = predict_text_width(name);
    }

    if let Some(name) = binding.name.as_mut() {
        DLOG!("Got mode change: {}", i3string_as_utf8(name).unwrap_or(""));
    }

    // C: We don't want to indicate the default binding mode.
    let is_default = binding.name_utf8().as_deref() == Some("default");
    if is_default {
        let name = binding.name.take();
        libi3::i3string_free(name);
    }

    // C: set_current_mode(&binding);
    set_current_mode(&mut binding);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xcb::{current_mode_utf8, current_mode_width};

    const MODE_DEFAULT_JSON: &str =
        r#"{"change":"default","pango_markup":true}"#;
    const MODE_RESIZE_JSON: &str =
        r#"{"change":"resize","pango_markup":true}"#;

    #[test]
    fn parse_mode_sets_binding_mode() {
        parse_mode_json(MODE_RESIZE_JSON.as_bytes());
        assert_eq!(current_mode_utf8().as_deref(), Some("resize"));
        assert!(current_mode_width() > 0);
    }

    #[test]
    fn parse_mode_ignores_default() {
        // C: strcmp("default", ...) == 0 -> I3STRING_FREE. Only the name is
        // freed; binding.name_width keeps the measured width, and draw_bars()
        // skips the indicator because binding.name is NULL.
        parse_mode_json(MODE_DEFAULT_JSON.as_bytes());
        assert_eq!(current_mode_utf8(), None);
        assert_eq!(current_mode_width(), "default".len() as i32 * crate::xcb::MONOSPACE_ADVANCE);
    }

    #[test]
    fn parse_mode_unknown_keys_are_ignored() {
        parse_mode_json(br#"{"change":"launcher","pango_markup":false,"other":"x"}"#);
        assert_eq!(current_mode_utf8().as_deref(), Some("launcher"));
    }

    #[test]
    fn parse_mode_without_change_clears_indicator() {
        parse_mode_json(br#"{"pango_markup":true}"#);
        assert_eq!(current_mode_utf8(), None);
    }
}
