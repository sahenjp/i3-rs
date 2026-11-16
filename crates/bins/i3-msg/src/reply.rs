//! C: i3-msg/main.c:31-148 — the yajl reply/config callbacks and the
//! yajl_parse invocations of main().
//!
//! yajl is a streaming parser; serde_json's Value tree is walked in document
//! order here so the same callbacks fire with the same state transitions.

use serde_json::Value;
use std::io::Write;
use std::sync::atomic::Ordering;

use crate::exit_code;

/// C: i3-msg/main.c:51 static char *last_key — kept as a field of the
/// parser state (C file statics have one instance per process; each parse
/// runs once).
struct ReplyParser {
    /// C: i3-msg/main.c:51 static char *last_key
    last_key: Option<String>,
    /// C: i3-msg/main.c:61 static reply_t last_reply
    last_reply: reply_t,
}

/// C: i3-msg/main.c:53 typedef struct reply_t
struct reply_t {
    success: bool,
    error: Option<String>,
    input: Option<String>,
    errorposition: Option<String>,
}

impl Default for reply_t {
    fn default() -> Self {
        reply_t { success: false, error: None, input: None, errorposition: None }
    }
}

impl ReplyParser {
    fn new() -> ReplyParser {
        ReplyParser { last_key: None, last_reply: reply_t::default() }
    }

    /// C: i3-msg/main.c:63 reply_boolean_cb
    fn reply_boolean_cb(&mut self, val: bool) {
        if self.last_key.as_deref() == Some("success") {
            self.last_reply.success = val;
        }
    }

    /// C: i3-msg/main.c:70 reply_string_cb — sstrndup(val, len) keeps raw
    /// bytes; serde_json gives valid UTF-8 strings.
    fn reply_string_cb(&mut self, val: &str) {
        if self.last_key.as_deref() == Some("error") {
            self.last_reply.error = Some(val.to_string());
        } else if self.last_key.as_deref() == Some("input") {
            self.last_reply.input = Some(val.to_string());
        } else if self.last_key.as_deref() == Some("errorposition") {
            self.last_reply.errorposition = Some(val.to_string());
        } else {
            // C: free(str) — untracked strings are dropped.
        }
    }

    /// C: i3-msg/main.c:85 reply_start_map_cb
    fn reply_start_map_cb(&mut self) {}

    /// C: i3-msg/main.c:89 reply_end_map_cb — nicely format the error.
    /// Note: glibc's printf prints "(null)" for a NULL %s argument; the
    /// Option::None cases below reproduce that.
    fn reply_end_map_cb(&mut self) {
        if !self.last_reply.success {
            if let Some(input) = self.last_reply.input.as_deref() {
                eprintln!("ERROR: Your command: {}", input);
                eprintln!(
                    "ERROR:               {}",
                    self.last_reply.errorposition.as_deref().unwrap_or("(null)")
                );
            }
            eprintln!("ERROR: {}", self.last_reply.error.as_deref().unwrap_or("(null)"));
            // C: exit_code = 2;
            exit_code.store(2, Ordering::SeqCst);
        }
    }

    /// C: i3-msg/main.c:101 reply_map_key_cb
    fn reply_map_key_cb(&mut self, key: &str) {
        self.last_key = Some(key.to_string());
    }
}

/// C: i3-msg/main.c:119 static char *config_last_key
struct ConfigParser {
    config_last_key: Option<String>,
}

impl ConfigParser {
    fn new() -> ConfigParser {
        ConfigParser { config_last_key: None }
    }

    /// C: i3-msg/main.c:121 config_string_cb
    fn config_string_cb(&mut self, val: &str) {
        if self.config_last_key.as_deref() == Some("config") {
            // C: fprintf(stdout, "%s", str); — no trailing newline.
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(val.as_bytes());
        }
    }

    /// C: i3-msg/main.c:138 config_map_key_cb — note the C code never frees
    /// the previous key (leak); ownership makes that moot here.
    fn config_map_key_cb(&mut self, key: &str) {
        self.config_last_key = Some(key.to_string());
    }
}

/// C: i3-msg/main.c:272-274 yajl_alloc(&reply_callbacks, ...) +
/// yajl_parse(handle, reply, reply_length) — returns false on
/// yajl_status_error / yajl_status_client_canceled.
pub fn yajl_parse_reply(reply: &[u8]) -> bool {
    // C: yajl_parse with 0 bytes returns yajl_status_ok (streaming parser,
    // no input yet); serde_json would report EOF — keep the C behavior.
    if reply.is_empty() {
        return true;
    }
    let v = match serde_json::from_slice::<Value>(reply) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let mut parser = ReplyParser::new();
    walk_reply(&v, &mut parser);
    true
}

/// The document-order walk matching reply_callbacks.
fn walk_reply(v: &Value, parser: &mut ReplyParser) {
    match v {
        Value::Object(map) => {
            parser.reply_start_map_cb();
            for (k, val) in map {
                parser.reply_map_key_cb(k);
                walk_reply(val, parser);
            }
            parser.reply_end_map_cb();
        }
        Value::Array(arr) => {
            for item in arr {
                walk_reply(item, parser);
            }
        }
        Value::Bool(b) => parser.reply_boolean_cb(*b),
        Value::String(s) => parser.reply_string_cb(s),
        _ => {}
    }
}

/// C: i3-msg/main.c:292-294 yajl_alloc(&config_callbacks, ...) +
/// yajl_parse — returns false on parse errors.
pub fn yajl_parse_config(reply: &[u8]) -> bool {
    if reply.is_empty() {
        return true;
    }
    let v = match serde_json::from_slice::<Value>(reply) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let mut parser = ConfigParser::new();
    walk_config(&v, &mut parser);
    true
}

/// The document-order walk matching config_callbacks.
fn walk_config(v: &Value, parser: &mut ConfigParser) {
    match v {
        Value::Object(map) => {
            // C: config_start_map_cb — no-op.
            for (k, val) in map {
                parser.config_map_key_cb(k);
                walk_config(val, parser);
            }
            // C: config_end_map_cb — no-op.
        }
        Value::Array(arr) => {
            for item in arr {
                walk_config(item, parser);
            }
        }
        Value::String(s) => parser.config_string_cb(s),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_reply_sets_nothing() {
        assert!(yajl_parse_reply(br#"[{"success": true}]"#));
        assert_eq!(exit_code.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_reply_sets_exit_code_2_and_prints_error() {
        // exit_code is per-process; in-process tests only check the parser
        // behavior of a fresh full walk.
        assert!(yajl_parse_reply(
            br#"[{"success": false, "error": "BAD", "input": "nop", "errorposition": "oops"}]"#
        ));
        assert_eq!(exit_code.load(Ordering::SeqCst), 2);
        exit_code.store(0, Ordering::SeqCst);
    }

    #[test]
    fn invalid_json_returns_false() {
        assert!(!yajl_parse_reply(b"not json"));
    }

    #[test]
    fn empty_reply_is_ok_like_yajl() {
        assert!(yajl_parse_reply(b""));
    }

    #[test]
    fn config_prints_config_value() {
        // config_string_cb writes the "config" value without a newline.
        assert!(yajl_parse_config(br#"{"success": true, "config": "cfg-body"}"#));
    }

    #[test]
    fn config_invalid_json_returns_false() {
        assert!(!yajl_parse_config(b"\x01"));
    }
}