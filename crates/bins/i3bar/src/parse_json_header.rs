#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

//! 1:1 port of i3bar/src/parse_json_header.c (119 lines) — parses the JSON
//! protocol header to determine protocol version and features.
//!
//! C: i3bar/src/parse_json_header.c
//!
//! yajl specifics the C code relies on:
//!  * yajl_config(handle, yajl_allow_trailing_garbage, 1): parsing stops after
//!    the first complete JSON value, everything behind it is ignored;
//!  * yajl_get_bytes_consumed(): the number of bytes the first value (plus its
//!    leading whitespace) occupied — the caller continues reading right behind
//!    it (i3status sends the header followed by an infinite JSON array).
//! Both are reproduced here: [first_json_value_end] finds the end of the first
//! complete value (0 when the buffer does not start with one), the slice is
//! then deserialized with serde_json.

use serde_json::Value;

use crate::child::i3bar_child;

/// C: parse_json_header.c:17 the static current_key state of the callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum key {
    KEY_VERSION,
    KEY_STOP_SIGNAL,
    KEY_CONT_SIGNAL,
    KEY_CLICK_EVENTS,
    NO_KEY,
}

/// C: parse_json_header.c:88 static void child_init(i3bar_child *child)
pub fn child_init(child: &mut i3bar_child) {
    child.version = 0;
    child.stop_signal = libc::SIGSTOP;
    child.cont_signal = libc::SIGCONT;
}

/// Find the end (exclusive byte index) of the first complete JSON value in
/// [buffer], or None when the buffer does not start with a (possibly
/// incomplete) JSON value.
///
/// This is the yajl_allow_trailing_garbage behaviour: the scan stops as soon as
/// the outermost value is complete.
pub fn first_json_value_end(buffer: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    // yajl skips leading whitespace.
    while i < buffer.len() && matches!(buffer[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    if i >= buffer.len() {
        return None;
    }

    let first = buffer[i];
    match first {
        b'{' | b'[' => {
            let mut depth = 0i32;
            let mut in_string = false;
            let mut escaped = false;
            while i < buffer.len() {
                let c = buffer[i];
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if c == b'\\' {
                        escaped = true;
                    } else if c == b'"' {
                        in_string = false;
                    }
                } else {
                    match c {
                        b'"' => in_string = true,
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(i + 1);
                            }
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
            None
        }
        b'"' => {
            let start = i;
            i += 1;
            let mut escaped = false;
            while i < buffer.len() {
                let c = buffer[i];
                if escaped {
                    escaped = false;
                } else if c == b'\\' {
                    escaped = true;
                } else if c == b'"' {
                    return Some(i + 1);
                }
                i += 1;
            }
            let _ = start;
            None
        }
        b'-' | b'0'..=b'9' => {
            // A bare number: consume the number characters.
            while i < buffer.len()
                && matches!(buffer[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
            {
                i += 1;
            }
            Some(i)
        }
        b't' if buffer[i..].starts_with(b"true") => Some(i + 4),
        b'f' if buffer[i..].starts_with(b"false") => Some(i + 5),
        b'n' if buffer[i..].starts_with(b"null") => Some(i + 4),
        _ => None,
    }
}

/// Find the first complete JSON *array* in the buffer (the outermost complete
/// array starting at any '[' position).
///
/// The C statusline parser is a streaming yajl parser: i3status sends one
/// infinite outer array, and each inner array is one statusline, whose
/// yajl_end_array callback swaps the statusline. Reproducing that means looking
/// for the next complete array anywhere in the buffer, not just at offset 0.
pub fn first_complete_array(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut start = 0usize;
    while let Some(offset) = buffer[start..].iter().position(|&b| b == b'[') {
        let begin = start + offset;
        if let Some(end) = matching_brace(buffer, begin) {
            return Some((begin, end));
        }
        start = begin + 1;
    }
    None
}

/// The index just past the brace that closes the value opened at begin
/// (string-aware), or None when it is not closed yet.
fn matching_brace(buffer: &[u8], begin: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = begin;
    while i < buffer.len() {
        let c = buffer[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// C: parse_json_header.c:93 void parse_json_header(i3bar_child *child,
/// const unsigned char *buffer, int length, unsigned int *consumed)
///
/// Returns the yajl bytes-consumed value (0 on an invalid header). The child is
/// (re-)initialized in both cases, exactly like the two child_init() calls of
/// the C function.
pub fn parse_json_header(child: &mut i3bar_child, buffer: &[u8]) -> u32 {
    child_init(child);

    let end = match first_json_value_end(buffer) {
        Some(end) => end,
        None => {
            // C: state != yajl_status_ok -> child_init() + *consumed = 0
            return 0;
        }
    };

    let value: Value = match serde_json::from_slice(&buffer[..end]) {
        Ok(v) => v,
        Err(_) => {
            // C: state != yajl_status_ok -> child_init() + *consumed = 0
            child_init(child);
            return 0;
        }
    };

    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            // A valid JSON value that is not a map: the header simply carries
            // no fields, so version stays 0 (exactly like yajl's callbacks).
            return end as u32;
        }
    };

    for (key_name, val) in obj {
        // C: header_map_key() — CHECK_KEY is a length + case-insensitive
        // prefix comparison; the length equality is what the C macro really
        // tests for the four known keys.
        let current_key = match key_name.as_str() {
            "version" => key::KEY_VERSION,
            "stop_signal" => key::KEY_STOP_SIGNAL,
            "cont_signal" => key::KEY_CONT_SIGNAL,
            "click_events" => key::KEY_CLICK_EVENTS,
            _ => key::NO_KEY,
        };

        match (current_key, val) {
            (key::KEY_VERSION, Value::Number(n)) => child.version = n.as_u64().unwrap_or(0) as u32,
            (key::KEY_STOP_SIGNAL, Value::Number(n)) => child.stop_signal = n.as_i64().unwrap_or(0) as i32,
            (key::KEY_CONT_SIGNAL, Value::Number(n)) => child.cont_signal = n.as_i64().unwrap_or(0) as i32,
            (key::KEY_CLICK_EVENTS, Value::Bool(b)) => child.click_events = *b,
            _ => {}
        }
    }

    end as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child() -> i3bar_child {
        i3bar_child::default()
    }

    #[test]
    fn header_version_and_click_events() {
        let mut c = child();
        let buf = br#"{"version":1,"click_events":true}"#;
        let consumed = parse_json_header(&mut c, buf);
        assert_eq!(c.version, 1);
        assert!(c.click_events);
        assert_eq!(consumed as usize, buf.len());
        // The yajl default for the signals.
        assert_eq!(c.stop_signal, libc::SIGSTOP);
        assert_eq!(c.cont_signal, libc::SIGCONT);
    }

    #[test]
    fn header_stop_and_cont_signal() {
        let mut c = child();
        let buf = br#"{"version":1,"stop_signal":10,"cont_signal":12,"click_events":false}"#;
        parse_json_header(&mut c, buf);
        assert_eq!(c.version, 1);
        assert_eq!(c.stop_signal, 10);
        assert_eq!(c.cont_signal, 12);
        assert!(!c.click_events);
    }

    #[test]
    fn header_consumed_stops_at_first_value() {
        let mut c = child();
        // i3status sends the header followed by an infinite array.
        let buf = b"{\"version\":1}\n[{\"full_text\":\"x\"}";
        let consumed = parse_json_header(&mut c, buf);
        assert_eq!(consumed as usize, "{\"version\":1}".len());
        assert_eq!(c.version, 1);
    }

    #[test]
    fn header_plain_text_is_not_json() {
        let mut c = child();
        let buf = b"hello world\n";
        let consumed = parse_json_header(&mut c, buf);
        assert_eq!(consumed, 0);
        assert_eq!(c.version, 0);
        assert_eq!(c.stop_signal, libc::SIGSTOP);
        assert_eq!(c.cont_signal, libc::SIGCONT);
    }

    #[test]
    fn header_empty_buffer() {
        let mut c = child();
        assert_eq!(parse_json_header(&mut c, b""), 0);
        assert_eq!(c.version, 0);
    }

    #[test]
    fn header_without_version_stays_zero() {
        let mut c = child();
        let consumed = parse_json_header(&mut c, br#"{"click_events":true}"#);
        assert_eq!(c.version, 0);
        // yajl still consumed the complete value.
        assert_eq!(consumed as usize, "{\"click_events\":true}".len());
    }

    #[test]
    fn header_invalid_json_is_rejected() {
        let mut c = child();
        c.version = 7;
        let consumed = parse_json_header(&mut c, b"{not json");
        assert_eq!(consumed, 0);
        assert_eq!(c.version, 0);
    }

    #[test]
    fn header_objects_inside_strings_are_balanced() {
        // The scanner must not count braces inside strings.
        let mut c = child();
        let buf = br#"{"version":1,"x":"}{"}"#;
        assert_eq!(parse_json_header(&mut c, buf) as usize, buf.len());
        assert_eq!(c.version, 1);
    }

    #[test]
    fn first_value_end_handles_scalars() {
        assert_eq!(first_json_value_end(b"42 abc"), Some(2));
        assert_eq!(first_json_value_end(b" true "), Some(5));
        assert_eq!(first_json_value_end(b"null,"), Some(4));
        assert_eq!(first_json_value_end(b"  \n"), None);
    }
}
