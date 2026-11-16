//! 1:1 port of src/load_layout.c (766 lines, i3 4.25 @ 8a97b0f):
//! restoring (parts of) the layout, e.g. after an in-place restart or for
//! 'append_layout'.
//!
//! The C file uses yajl callbacks plus a set of file-static variables
//! (last_key, incomplete, json_node, to_focus, the parsing_* flags,
//! current_swallow, num_marks, marks, focus_mappings). The port keeps the very
//! same callbacks and moves the statics into LoadLayout, so a parse has no
//! global state and can run concurrently in tests.
//!
//! yajl itself is not available, but only a small, well-defined part of it is
//! used here (streaming SAX callbacks, allow_comments, allow_multiple_values).
//! parse_stream implements exactly that subset and returns yajl-shaped error
//! strings, so the "Could not parse ..." messages of cmd_append_layout keep
//! working unchanged.

use crate::backend::ConBackend;
use crate::con::{
    con_activate, con_attach, con_fix_percent, con_free, con_get_workspace, con_is_split,
    con_mark, ConId,
};
use crate::data::{
    regex, BorderStyle, ConType, FloatingState, Layout, MarkMode, Match, MatchDock,
    MatchInsertWhere, MatchUrgency, MatchWindowMode, Rect, ScratchpadState, XCB_WINDOW_NONE,
};
use crate::floating::floating_enable;
use crate::tree::Tree;
use crate::util::{rect_equals, ws_name_to_number};
use crate::workspace::get_existing_workspace_by_name;

// ---------------------------------------------------------------------------
// The JSON parser (yajl subset)
// ---------------------------------------------------------------------------

/// C: include/load_layout.h:18 json_content_t
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum json_content_t {
    JSON_CONTENT_UNKNOWN = 0,
    JSON_CONTENT_CON = 1,
    JSON_CONTENT_WORKSPACE = 2,
}

/// The yajl callback set used by load_layout.c
/// (C: static yajl_callbacks callbacks, load_layout.c:694-703 — yajl only
/// calls the members that are non-NULL, so every callback defaults to a
/// no-op here).
trait JsonCallbacks {
    fn on_bool(&mut self, _val: bool) -> Result<(), String> {
        Ok(())
    }
    fn on_int(&mut self, _val: i64) -> Result<(), String> {
        Ok(())
    }
    fn on_double(&mut self, _val: f64) -> Result<(), String> {
        Ok(())
    }
    fn on_string(&mut self, _val: &str) -> Result<(), String> {
        Ok(())
    }
    fn on_key(&mut self, _val: &str) -> Result<(), String> {
        Ok(())
    }
    fn on_start_map(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn on_end_map(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// C: yajl_start_array — load_layout.c's callback set has no member for
    /// it, but json_determine_content's does (json_determine_content_deeper is
    /// used for both start_array and start_map, so the level accounting stays
    /// balanced). The parser therefore asks every callback set.
    fn on_start_array(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn on_end_array(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// C: a callback returning 0 makes yajl stop with
    /// yajl_status_client_canceled (load_layout.c:622).
    fn canceled(&self) -> bool {
        false
    }
}

/// yajl parses incrementally: when the buffer ends in the middle of a value,
/// yajl_parse() reports yajl_status_ok and only yajl_complete_parse() notices.
/// load_layout.c checks yajl_parse() only, so a truncated layout file is
/// accepted (cmd_append_layout replies success) — the value simply never
/// completes and no container is attached. This sentinel transports that
/// "ran out of input" case out of the recursive descent parser (the exact
/// C behaviour, verified against i3 4.25.1: '{ "a": 1' is accepted).
const EOF: &str = "\u{1}eof";

fn eof() -> String {
    EOF.to_string()
}

fn is_eof(e: &str) -> bool {
    e == EOF
}

/// Mutable parser position. C: the yajl_handle internals.
struct Lexer<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    /// C: the yajl lexer — whitespace plus /* */ and // comments
    /// (yajl_allow_comments, load_layout.c:706).
    fn skip_ws(&mut self) -> Result<(), String> {
        loop {
            while self.pos < self.buf.len() && (self.buf[self.pos] as char).is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.pos + 1 < self.buf.len() && self.buf[self.pos] == b'/' && self.buf[self.pos + 1] == b'/' {
                while self.pos < self.buf.len() && self.buf[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            if self.pos + 1 < self.buf.len() && self.buf[self.pos] == b'/' && self.buf[self.pos + 1] == b'*' {
                self.pos += 2;
                while self.pos + 1 < self.buf.len()
                    && !(self.buf[self.pos] == b'*' && self.buf[self.pos + 1] == b'/')
                {
                    self.pos += 1;
                }
                if self.pos + 1 >= self.buf.len() {
                    return Err(eof());
                }
                self.pos += 2;
                continue;
            }
            return Ok(());
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// C: yajl lexer string token (with escapes). yajl_dont_validate_strings
    /// is set in tree_append_json (load_layout.c:719), so invalid UTF-8 is
    /// preserved here as well (via from_utf8_lossy, the closest equivalent of
    /// a String in Rust).
    fn lex_string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err("expected a string".to_string());
        }
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let b = match self.peek() {
                Some(b) => b,
                None => return Err(eof()),
            };
            self.pos += 1;
            match b {
                b'"' => return Ok(String::from_utf8_lossy(&out).into_owned()),
                b'\\' => {
                    let esc = match self.peek() {
                        Some(c) => c,
                        None => return Err(eof()),
                    };
                    self.pos += 1;
                    match esc {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let cp = self.lex_hex4()?;
                            // surrogate pair
                            let ch = if (0xD800..0xDC00).contains(&cp) {
                                if self.peek() == Some(b'\\') {
                                    self.pos += 1;
                                    if self.peek() != Some(b'u') {
                                        return Err("invalid unicode escape".to_string());
                                    }
                                    self.pos += 1;
                                    let lo = self.lex_hex4()?;
                                    if !(0xDC00..0xE000).contains(&lo) {
                                        return Err("invalid unicode escape".to_string());
                                    }
                                    let combined = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    char::from_u32(combined).unwrap_or('\u{FFFD}')
                                } else {
                                    '\u{FFFD}'
                                }
                            } else {
                                char::from_u32(cp).unwrap_or('\u{FFFD}')
                            };
                            let mut utf8 = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                        }
                        _ => return Err("invalid escape".to_string()),
                    }
                }
                _ => out.push(b),
            }
        }
    }

    fn lex_hex4(&mut self) -> Result<u32, String> {
        if self.pos + 4 > self.buf.len() {
            return Err("invalid unicode escape".to_string());
        }
        let s = std::str::from_utf8(&self.buf[self.pos..self.pos + 4])
            .map_err(|_| "invalid unicode escape".to_string())?;
        let v = u32::from_str_radix(s, 16).map_err(|_| "invalid unicode escape".to_string())?;
        self.pos += 4;
        Ok(v)
    }

    /// C: yajl number token. Returns the raw token text.
    fn lex_number(&mut self) -> Result<&'a str, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return Err("invalid number".to_string()),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err("invalid number".to_string());
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err("invalid number".to_string());
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        Ok(std::str::from_utf8(&self.buf[start..self.pos]).unwrap_or("0"))
    }
}

/// C: yajl_parse + yajl_allow_multiple_values (load_layout.c:708) — parse one
/// or more JSON values from buf and drive the callback set.
fn parse_stream(buf: &[u8], cb: &mut impl JsonCallbacks) -> Result<(), String> {
    let mut lex = Lexer { buf, pos: 0 };
    loop {
        if let Err(e) = lex.skip_ws() {
            return if is_eof(&e) { Ok(()) } else { Err(e) };
        }
        if lex.at_end() {
            return Ok(());
        }
        if let Err(e) = parse_value(&mut lex, cb) {
            return if is_eof(&e) { Ok(()) } else { Err(e) };
        }
        if cb.canceled() {
            return Ok(());
        }
    }
}

fn parse_value(lex: &mut Lexer<'_>, cb: &mut impl JsonCallbacks) -> Result<(), String> {
    lex.skip_ws()?;
    let b = match lex.peek() {
        Some(b) => b,
        None => return Err(eof()),
    };
    match b {
        b'{' => {
            lex.pos += 1;
            cb.on_start_map()?;
            if cb.canceled() {
                return Ok(());
            }
            lex.skip_ws()?;
            if lex.peek() == Some(b'}') {
                lex.pos += 1;
                return cb.on_end_map();
            }
            loop {
                lex.skip_ws()?;
                if lex.at_end() {
                    // yajl: EOF where the next key would start is "needs more
                    // input" (tok_eof), not a syntax error.
                    return Err(eof());
                }
                let key = lex.lex_string()?;
                cb.on_key(&key)?;
                if cb.canceled() {
                    return Ok(());
                }
                lex.skip_ws()?;
                if lex.peek() != Some(b':') {
                    return Err("expected ':'".to_string());
                }
                lex.pos += 1;
                parse_value(lex, cb)?;
                if cb.canceled() {
                    return Ok(());
                }
                lex.skip_ws()?;
                match lex.peek() {
                    Some(b',') => lex.pos += 1,
                    Some(b'}') => {
                        lex.pos += 1;
                        return cb.on_end_map();
                    }
                    Some(_) => return Err("expected ',' or '}'".to_string()),
                    None => return Err(eof()),
                }
            }
        }
        b'[' => {
            lex.pos += 1;
            // C: the callback sets differ: load_layout.c registers no
            // yajl_start_array, json_determine_content registers
            // json_determine_content_deeper for it. The default trait member
            // is the no-op load_layout needs.
            cb.on_start_array()?;
            if cb.canceled() {
                return Ok(());
            }
            lex.skip_ws()?;
            if lex.peek() == Some(b']') {
                lex.pos += 1;
                return cb.on_end_array();
            }
            loop {
                parse_value(lex, cb)?;
                if cb.canceled() {
                    return Ok(());
                }
                lex.skip_ws()?;
                match lex.peek() {
                    Some(b',') => lex.pos += 1,
                    Some(b']') => {
                        lex.pos += 1;
                        return cb.on_end_array();
                    }
                    Some(_) => return Err("expected ',' or ']'".to_string()),
                    None => return Err(eof()),
                }
            }
        }
        b'"' => {
            let s = lex.lex_string()?;
            cb.on_string(&s)
        }
        b't' => {
            if buf_starts_with(lex, b"true") {
                lex.pos += 4;
                cb.on_bool(true)
            } else {
                Err("invalid token".to_string())
            }
        }
        b'f' => {
            if buf_starts_with(lex, b"false") {
                lex.pos += 5;
                cb.on_bool(false)
            } else {
                Err("invalid token".to_string())
            }
        }
        b'n' => {
            // yajl passes null values through without calling any callback,
            // which the layout dumps rely on: dump_node writes
            // "percent": null for a container without a percentage
            // (dump.c dump_node / the C dump_node), and an in-place restart
            // has to parse that back.
            if buf_starts_with(lex, b"null") {
                lex.pos += 4;
                Ok(())
            } else {
                Err("invalid token".to_string())
            }
        }
        b'-' | b'0'..=b'9' => {
            let token = lex.lex_number()?;
            if token.contains('.') || token.contains('e') || token.contains('E') {
                let v: f64 = token.parse().map_err(|_| "invalid number".to_string())?;
                cb.on_double(v)
            } else {
                let v: i64 = token.parse().map_err(|_| "invalid number".to_string())?;
                cb.on_int(v)
            }
        }
        _ => Err("invalid token".to_string()),
    }
}

fn buf_starts_with(lex: &Lexer<'_>, lit: &[u8]) -> bool {
    lex.buf.len() >= lex.pos + lit.len() && &lex.buf[lex.pos..lex.pos + lit.len()] == lit
}

/// C: yajl_get_error — the error string that ends up in the IPC reply of
/// cmd_append_layout ("Could not parse ..." is the caller's text, this is the
/// ELOG/errormsg payload).
fn yajl_error_string(msg: &str) -> String {
    format!("parse error: {}\n", msg)
}

// ---------------------------------------------------------------------------
// json_validate / json_determine_content
// ---------------------------------------------------------------------------

struct ValidateCb;
impl JsonCallbacks for ValidateCb {}

/// C: src/load_layout.c:629 json_validate — true when the buffer is valid
/// JSON as far as yajl is concerned (comments and multiple values allowed).
pub fn json_validate(buf: &[u8], _len: usize) -> bool {
    let mut cb = ValidateCb;
    match parse_stream(buf, &mut cb) {
        Ok(()) => true,
        Err(e) => {
            crate::log::errorlog(&format!("JSON parsing error: {}\n", yajl_error_string(&e)));
            false
        }
    }
}

/// C: src/load_layout.c:600-691 — the state of json_determine_content.
struct ContentCb {
    content_result: json_content_t,
    content_level: i32,
    last_key: String,
    canceled: bool,
}

impl JsonCallbacks for ContentCb {
    /// C: json_determine_content_deeper (registered for start_map *and*
    /// start_array)
    fn on_start_map(&mut self) -> Result<(), String> {
        self.content_level += 1;
        Ok(())
    }
    fn on_start_array(&mut self) -> Result<(), String> {
        self.content_level += 1;
        Ok(())
    }
    /// C: json_determine_content_shallower
    fn on_end_map(&mut self) -> Result<(), String> {
        self.content_level -= 1;
        Ok(())
    }
    fn on_end_array(&mut self) -> Result<(), String> {
        self.content_level -= 1;
        Ok(())
    }
    /// C: json_key (shared with tree_append_json)
    fn on_key(&mut self, val: &str) -> Result<(), String> {
        self.last_key = val.to_string();
        Ok(())
    }
    /// C: load_layout.c:613 json_determine_content_string
    fn on_string(&mut self, val: &str) -> Result<(), String> {
        if !self.last_key.eq_ignore_ascii_case("type") || self.content_level > 1 {
            return Ok(());
        }
        // C: strncasecmp(val, "workspace", len) == 0
        if val.len() >= "workspace".len()
            && val.as_bytes()[.."workspace".len()].eq_ignore_ascii_case(b"workspace")
        {
            self.content_result = json_content_t::JSON_CONTENT_WORKSPACE;
        }
        // C: return 0 (client_canceled)
        self.canceled = true;
        Ok(())
    }
    fn canceled(&self) -> bool {
        self.canceled
    }
}

/// C: src/load_layout.c:658 json_determine_content
///
/// Parses the given JSON file until it encounters the first "type" property to
/// determine whether the file contains workspaces or regular containers.
pub fn json_determine_content(buf: &[u8], _len: usize) -> json_content_t {
    let mut cb = ContentCb {
        // C: We default to JSON_CONTENT_CON because it is legal to not include
        // "type": "con" in the JSON files for better readability.
        content_result: json_content_t::JSON_CONTENT_CON,
        content_level: 0,
        last_key: String::new(),
        canceled: false,
    };
    if let Err(e) = parse_stream(buf, &mut cb) {
        crate::log::errorlog(&format!("JSON parsing error: {}\n", yajl_error_string(&e)));
    }
    cb.content_result
}

// ---------------------------------------------------------------------------
// slurp (C: libi3/slurp.c / src/util.c:443)
// ---------------------------------------------------------------------------

/// C: src/util.c:443 slurp — read path in its entirety.
///
/// C opens the file twice (fopen + fstat) and logs "Cannot open file ..." on
/// failure; the port reads through std::fs and produces the same ELOG text.
pub fn slurp(path: &str) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(buf) => Some(buf),
        Err(e) => {
            crate::log::errorlog(&format!("Cannot open file \"{}\": {}\n", path, e));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// tree_append_json
// ---------------------------------------------------------------------------

/// C: struct pending_marks (load_layout.c:40)
struct PendingMark {
    mark: String,
    con_to_be_marked: ConId,
}

/// C: load_layout.c:19-43 — the file-static parser state.
pub struct LoadLayout<'t, 'b> {
    t: &'t mut Tree,
    b: &'b mut dyn ConBackend,

    /// C: static char *last_key
    last_key: String,
    /// C: static int incomplete
    incomplete: i32,
    /// C: static Con *json_node
    json_node: ConId,
    /// C: static Con *to_focus
    to_focus: Option<ConId>,
    /// C: static bool parsing_*
    parsing_gaps: bool,
    parsing_swallows: bool,
    parsing_rect: bool,
    parsing_actual_deco_rect: bool,
    parsing_deco_rect: bool,
    parsing_window_rect: bool,
    parsing_geometry: bool,
    parsing_focus: bool,
    parsing_marks: bool,
    /// C: Match *current_swallow — the swallow being filled; pushed into
    /// json_node->swallow_head when its map ends (C inserts it at start and
    /// fills the queue entry in place, which is equivalent because JSON
    /// objects are atomic).
    current_swallow: Option<Match>,
    /// C: static bool swallow_is_empty
    swallow_is_empty: bool,
    /// C: static int num_marks + struct pending_marks *marks
    marks: Vec<PendingMark>,
    /// C: static TAILQ_HEAD(focus_mappings_head, focus_mapping) focus_mappings
    focus_mappings: Vec<i32>,
}

impl<'t, 'b> LoadLayout<'t, 'b> {
    fn new(t: &'t mut Tree, b: &'b mut dyn ConBackend, con: ConId) -> LoadLayout<'t, 'b> {
        LoadLayout {
            t,
            b,
            last_key: String::new(),
            incomplete: 0,
            json_node: con,
            to_focus: None,
            parsing_gaps: false,
            parsing_swallows: false,
            parsing_rect: false,
            parsing_actual_deco_rect: false,
            parsing_deco_rect: false,
            parsing_window_rect: false,
            parsing_geometry: false,
            parsing_focus: false,
            parsing_marks: false,
            current_swallow: None,
            swallow_is_empty: false,
            marks: Vec::new(),
            focus_mappings: Vec::new(),
        }
    }

    fn rect_of(&mut self) -> Option<&mut Rect> {
        if self.parsing_rect {
            Some(&mut self.t.cons[self.json_node].rect)
        } else if self.parsing_window_rect {
            Some(&mut self.t.cons[self.json_node].window_rect)
        } else if self.parsing_geometry {
            Some(&mut self.t.cons[self.json_node].geometry)
        } else {
            None
        }
    }

    fn in_rect_ish(&self) -> bool {
        self.parsing_rect
            || self.parsing_actual_deco_rect
            || self.parsing_deco_rect
            || self.parsing_window_rect
            || self.parsing_geometry
            || self.parsing_gaps
    }

    /// C: load_layout.c:742-750 — free the containers of a partially parsed
    /// map chain.
    fn cleanup_incomplete(&mut self) {
        while self.incomplete > 0 {
            let parent = self.t.cons[self.json_node].parent;
            if self.to_focus == Some(self.json_node) {
                self.to_focus = None;
            }
            con_free(self.t, self.json_node);
            match parent {
                Some(p) => self.json_node = p,
                None => break,
            }
            self.incomplete -= 1;
        }
    }
}

impl<'t, 'b> JsonCallbacks for LoadLayout<'t, 'b> {
    /// C: src/load_layout.c:55 json_start_map
    fn on_start_map(&mut self) -> Result<(), String> {
        if self.parsing_swallows {
            // C: creating new swallow
            let mut m = match_init();
            m.dock = MatchDock::M_DONTCHECK;
            self.current_swallow = Some(m);
            self.swallow_is_empty = true;
        } else if !self.in_rect_ish() {
            if self.last_key.eq_ignore_ascii_case("floating_nodes") {
                let ws = con_get_workspace(self.t, self.json_node);
                let new = crate::con::con_new_skeleton(self.t, self.b, None, None);
                self.t.cons[new].name = None;
                self.t.cons[new].parent = ws;
                self.json_node = new;
            } else {
                let parent = self.json_node;
                let new = crate::con::con_new_skeleton(self.t, self.b, None, None);
                self.t.cons[new].name = None;
                self.t.cons[new].parent = Some(parent);
                self.json_node = new;
            }
            /* json_node is incomplete and should be removed if parsing fails */
            self.incomplete += 1;
        }
        Ok(())
    }

    /// C: src/load_layout.c:92 json_end_map
    fn on_end_map(&mut self) -> Result<(), String> {
        if self.parsing_swallows {
            if self.swallow_is_empty {
                /* We parsed an empty swallow definition. This is an invalid
                 * layout definition, hence we reject it. */
                crate::log::errorlog("Layout file is invalid: found an empty swallow definition.\n");
                return Err("found an empty swallow definition".to_string());
            }
            let m = self.current_swallow.take().unwrap_or_else(match_init);
            self.t.cons[self.json_node].swallow_head.push_back(m);
            self.swallow_is_empty = false;
        } else if !self.in_rect_ish() {
            /* Set a few default values to simplify manually crafted layout
             * files. */
            if self.t.cons[self.json_node].layout == Layout::L_DEFAULT {
                self.t.cons[self.json_node].layout = Layout::L_SPLITH;
            }

            /* Sanity check: swallow criteria don't make any sense on a split
             * container. */
            if con_is_split(self.t, self.json_node)
                && !self.t.cons[self.json_node].swallow_head.is_empty()
            {
                self.t.cons[self.json_node].swallow_head.clear();
            }

            if self.t.cons[self.json_node].r#type == ConType::CT_WORKSPACE {
                /* Ensure the workspace has a name. */
                let empty = self.t.cons[self.json_node]
                    .name
                    .as_deref()
                    .map(|n| n.is_empty())
                    .unwrap_or(true);
                if empty {
                    self.t.cons[self.json_node].name = Some("unnamed".to_string());
                }

                /* Prevent name clashes when appending a workspace, e.g. when
                 * the user tries to restore a workspace called "1" but
                 * already has a workspace called "1". */
                let base = self.t.cons[self.json_node].name.clone().unwrap_or_default();
                let mut cnt = 1;
                loop {
                    let name = self.t.cons[self.json_node].name.clone().unwrap_or_default();
                    if get_existing_workspace_by_name(self.t, &name).is_none() {
                        break;
                    }
                    self.t.cons[self.json_node].name = Some(format!("{}_{}", base, cnt));
                    cnt += 1;
                }

                /* Set num accordingly so that i3bar will properly sort it. */
                let name = self.t.cons[self.json_node].name.clone().unwrap_or_default();
                self.t.cons[self.json_node].num = ws_name_to_number(&name);
            }

            // When appending JSON layout files that only contain the workspace
            // _contents_, we might not have an upfront signal that the
            // container we're currently parsing is a floating container (like
            // the "floating_nodes" key of the workspace container itself).
            // That's why we make sure the con is attached at the right place
            // in the hierarchy in case it's floating.
            if self.t.cons[self.json_node].r#type == ConType::CT_FLOATING_CON {
                let old_parent = self.t.cons[self.json_node].parent;
                let ws_parent = old_parent.and_then(|p| con_get_workspace(self.t, p));
                self.t.cons[self.json_node].parent = ws_parent;

                // Also set a size if none was supplied, otherwise the
                // placeholder window cannot be created as X11 requests with
                // width=0 or height=0 are invalid.
                if rect_equals(self.t.cons[self.json_node].rect, Rect::new(0, 0, 0, 0)) {
                    let children: Vec<ConId> =
                        self.t.cons[self.json_node].nodes_head.iter().copied().collect();
                    for child in children {
                        let (w, h) = (
                            self.t.cons[child].geometry.width,
                            self.t.cons[child].geometry.height,
                        );
                        self.t.cons[self.json_node].rect.width += w;
                        self.t.cons[self.json_node].rect.height =
                            self.t.cons[self.json_node].rect.height.max(h);
                    }
                }

                floating_check_size(self.t, self.json_node, false);
            }

            if !self.marks.is_empty() {
                let marks: Vec<(String, ConId)> = self
                    .marks
                    .drain(..)
                    .map(|m| (m.mark, m.con_to_be_marked))
                    .collect();
                for (mark, con) in marks {
                    con_mark(self.t, self.b, con, &mark, MarkMode::MM_ADD);
                }
            }

            if let Some(parent) = self.t.cons[self.json_node].parent {
                con_attach(self.t, self.b, self.json_node, parent, true);
            }
            self.b.x_con_init(self.t, self.json_node);

            /* Fix erroneous JSON input regarding floating containers to avoid
             * crashing, see #3901. */
            let old_floating_mode = self.t.cons[self.json_node].floating;
            if old_floating_mode >= FloatingState::FLOATING_AUTO_ON {
                let parent = self.t.cons[self.json_node].parent;
                let parent_is_floating_con = parent
                    .map(|p| self.t.cons[p].r#type == ConType::CT_FLOATING_CON)
                    .unwrap_or(false);
                if !parent_is_floating_con {
                    /* Force floating_enable to work */
                    self.t.cons[self.json_node].floating = FloatingState::FLOATING_AUTO_OFF;
                    floating_enable(self.t, self.b, self.json_node, false);
                    self.t.cons[self.json_node].floating = old_floating_mode;
                }
            }

            if let Some(parent) = self.t.cons[self.json_node].parent {
                self.json_node = parent;
            }
            self.incomplete -= 1;
        }

        self.parsing_gaps = false;
        self.parsing_rect = false;
        self.parsing_actual_deco_rect = false;
        self.parsing_deco_rect = false;
        self.parsing_window_rect = false;
        self.parsing_geometry = false;
        Ok(())
    }

    /// C: src/load_layout.c:217 json_end_array
    fn on_end_array(&mut self) -> Result<(), String> {
        if !self.parsing_swallows && !self.parsing_focus && !self.parsing_marks {
            con_fix_percent(self.t, self.json_node);
        }
        if self.parsing_swallows {
            self.parsing_swallows = false;
        }
        if self.parsing_marks {
            self.parsing_marks = false;
        }

        if self.parsing_focus {
            /* Clear the list of focus mappings */
            for old_id in self.focus_mappings.iter().rev() {
                let children: Vec<ConId> =
                    self.t.cons[self.json_node].focus_head.iter().copied().collect();
                for con in children {
                    if self.t.cons[con].old_id != *old_id {
                        continue;
                    }
                    /* Move this entry to the top of the focus list. */
                    self.t.focus_remove(self.json_node, con);
                    self.t.focus_insert_head(self.json_node, con);
                    break;
                }
            }
            self.focus_mappings.clear();
            self.parsing_focus = false;
        }
        Ok(())
    }

    /// C: src/load_layout.c:256 json_key
    fn on_key(&mut self, val: &str) -> Result<(), String> {
        self.last_key = val.to_string();
        if self.last_key.eq_ignore_ascii_case("swallows") {
            self.parsing_swallows = true;
        }
        if self.last_key.eq_ignore_ascii_case("gaps") {
            self.parsing_gaps = true;
        }
        if self.last_key.eq_ignore_ascii_case("rect") {
            self.parsing_rect = true;
        }
        if self.last_key.eq_ignore_ascii_case("actual_deco_rect") {
            self.parsing_actual_deco_rect = true;
        }
        if self.last_key.eq_ignore_ascii_case("deco_rect") {
            self.parsing_deco_rect = true;
        }
        if self.last_key.eq_ignore_ascii_case("window_rect") {
            self.parsing_window_rect = true;
        }
        if self.last_key.eq_ignore_ascii_case("geometry") {
            self.parsing_geometry = true;
        }
        if self.last_key.eq_ignore_ascii_case("focus") {
            self.parsing_focus = true;
        }
        if self.last_key.eq_ignore_ascii_case("marks") {
            self.marks.clear();
            self.parsing_marks = true;
        }
        Ok(())
    }

    /// C: src/load_layout.c:301 json_string
    fn on_string(&mut self, val: &str) -> Result<(), String> {
        if self.parsing_swallows {
            let key = self.last_key.to_ascii_lowercase();
            let m = match self.current_swallow.as_mut() {
                Some(m) => m,
                None => return Ok(()),
            };
            match key.as_str() {
                "class" => {
                    m.class = Some(Box::new(regex::new(val)));
                    self.swallow_is_empty = false;
                }
                "instance" => {
                    m.instance = Some(Box::new(regex::new(val)));
                    self.swallow_is_empty = false;
                }
                "window_role" => {
                    m.window_role = Some(Box::new(regex::new(val)));
                    self.swallow_is_empty = false;
                }
                "title" => {
                    m.title = Some(Box::new(regex::new(val)));
                    self.swallow_is_empty = false;
                }
                "machine" => {
                    m.machine = Some(Box::new(regex::new(val)));
                    self.swallow_is_empty = false;
                }
                _ => {
                    crate::log::errorlog(&format!("swallow key {} unknown\n", self.last_key));
                }
            }
            return Ok(());
        }

        if self.parsing_marks {
            self.marks.push(PendingMark {
                mark: val.to_string(),
                con_to_be_marked: self.json_node,
            });
            return Ok(());
        }

        let con = self.json_node;
        let key = self.last_key.to_ascii_lowercase();
        match key.as_str() {
            "name" => self.t.cons[con].name = Some(val.to_string()),
            "title_format" => self.t.cons[con].title_format = Some(val.to_string()),
            "sticky_group" => self.t.cons[con].sticky_group = Some(val.to_string()),
            "orientation" => {
                /* Upgrade path from older versions of i3 (doing an inplace
                 * restart to a newer version): "orientation" is dumped before
                 * "layout". Therefore, we store whether the orientation was
                 * horizontal or vertical in the last_split_layout. When we
                 * then encounter layout == "default", we will use the
                 * last_split_layout as layout instead. */
                if val.eq_ignore_ascii_case("none") || val.eq_ignore_ascii_case("horizontal") {
                    self.t.cons[con].last_split_layout = Layout::L_SPLITH;
                } else if val.eq_ignore_ascii_case("vertical") {
                    self.t.cons[con].last_split_layout = Layout::L_SPLITV;
                }
            }
            "border" => {
                if val.eq_ignore_ascii_case("none") {
                    self.t.cons[con].max_user_border_style = BorderStyle::BS_NONE;
                    self.t.cons[con].border_style = BorderStyle::BS_NONE;
                } else if val.eq_ignore_ascii_case("1pixel") {
                    self.t.cons[con].max_user_border_style = BorderStyle::BS_PIXEL;
                    self.t.cons[con].border_style = BorderStyle::BS_PIXEL;
                    self.t.cons[con].current_border_width = 1;
                } else if val.eq_ignore_ascii_case("pixel") {
                    self.t.cons[con].max_user_border_style = BorderStyle::BS_PIXEL;
                    self.t.cons[con].border_style = BorderStyle::BS_PIXEL;
                } else if val.eq_ignore_ascii_case("normal") {
                    self.t.cons[con].max_user_border_style = BorderStyle::BS_NORMAL;
                    self.t.cons[con].border_style = BorderStyle::BS_NORMAL;
                }
            }
            "type" => {
                let ty = if val.eq_ignore_ascii_case("root") {
                    Some(ConType::CT_ROOT)
                } else if val.eq_ignore_ascii_case("output") {
                    Some(ConType::CT_OUTPUT)
                } else if val.eq_ignore_ascii_case("con") {
                    Some(ConType::CT_CON)
                } else if val.eq_ignore_ascii_case("floating_con") {
                    Some(ConType::CT_FLOATING_CON)
                } else if val.eq_ignore_ascii_case("workspace") {
                    Some(ConType::CT_WORKSPACE)
                } else if val.eq_ignore_ascii_case("dockarea") {
                    Some(ConType::CT_DOCKAREA)
                } else {
                    None
                };
                if let Some(ty) = ty {
                    self.t.cons[con].r#type = ty;
                }
            }
            "layout" => {
                let layout = if val.eq_ignore_ascii_case("default") {
                    /* This set above when we read "orientation". */
                    Some(self.t.cons[con].last_split_layout)
                } else if val.eq_ignore_ascii_case("stacked") {
                    Some(Layout::L_STACKED)
                } else if val.eq_ignore_ascii_case("tabbed") {
                    Some(Layout::L_TABBED)
                } else if val.eq_ignore_ascii_case("dockarea") {
                    Some(Layout::L_DOCKAREA)
                } else if val.eq_ignore_ascii_case("output") {
                    Some(Layout::L_OUTPUT)
                } else if val.eq_ignore_ascii_case("splith") {
                    Some(Layout::L_SPLITH)
                } else if val.eq_ignore_ascii_case("splitv") {
                    Some(Layout::L_SPLITV)
                } else {
                    None
                };
                if let Some(layout) = layout {
                    self.t.cons[con].layout = layout;
                }
            }
            "workspace_layout" => {
                let layout = if val.eq_ignore_ascii_case("default") {
                    Some(Layout::L_DEFAULT)
                } else if val.eq_ignore_ascii_case("stacked") {
                    Some(Layout::L_STACKED)
                } else if val.eq_ignore_ascii_case("tabbed") {
                    Some(Layout::L_TABBED)
                } else {
                    None
                };
                if let Some(layout) = layout {
                    self.t.cons[con].workspace_layout = layout;
                }
            }
            "last_split_layout" => {
                if val.eq_ignore_ascii_case("splith") {
                    self.t.cons[con].last_split_layout = Layout::L_SPLITH;
                } else if val.eq_ignore_ascii_case("splitv") {
                    self.t.cons[con].last_split_layout = Layout::L_SPLITV;
                }
            }
            "mark" => {
                /* deprecated key */
                con_mark(self.t, self.b, con, val, MarkMode::MM_REPLACE);
            }
            "floating" => {
                let state = if val.eq_ignore_ascii_case("auto_off") {
                    Some(FloatingState::FLOATING_AUTO_OFF)
                } else if val.eq_ignore_ascii_case("auto_on") {
                    Some(FloatingState::FLOATING_AUTO_ON)
                } else if val.eq_ignore_ascii_case("user_off") {
                    Some(FloatingState::FLOATING_USER_OFF)
                } else if val.eq_ignore_ascii_case("user_on") {
                    Some(FloatingState::FLOATING_USER_ON)
                } else {
                    None
                };
                if let Some(state) = state {
                    self.t.cons[con].floating = state;
                }
            }
            "scratchpad_state" => {
                let state = if val.eq_ignore_ascii_case("none") {
                    Some(ScratchpadState::SCRATCHPAD_NONE)
                } else if val.eq_ignore_ascii_case("fresh") {
                    Some(ScratchpadState::SCRATCHPAD_FRESH)
                } else if val.eq_ignore_ascii_case("changed") {
                    Some(ScratchpadState::SCRATCHPAD_CHANGED)
                } else {
                    None
                };
                if let Some(state) = state {
                    self.t.cons[con].scratchpad_state = state;
                }
            }
            "previous_workspace_name" => {
                self.t.previous_workspace_name = Some(val.to_string());
            }
            _ => {}
        }
        Ok(())
    }

    /// C: src/load_layout.c:481 json_int
    fn on_int(&mut self, val: i64) -> Result<(), String> {
        let con = self.json_node;
        let key = self.last_key.to_ascii_lowercase();

        /* For backwards compatibility with i3 < 4.8 */
        if key == "type" {
            self.t.cons[con].r#type = contype_from_i64(val);
        }
        if key == "fullscreen_mode" {
            self.t.cons[con].fullscreen_mode = fullscreen_mode_from_i64(val);
        }
        if key == "num" {
            self.t.cons[con].num = val as i32;
        }
        if key == "current_border_width" {
            self.t.cons[con].current_border_width = val as i32;
        }
        if key == "window_icon_padding" {
            self.t.cons[con].window_icon_padding = val as i32;
        }
        if key == "depth" {
            self.t.cons[con].depth = val as u16;
        }
        if !self.parsing_swallows && key == "id" {
            self.t.cons[con].old_id = val as i32;
        }
        if self.parsing_focus {
            self.focus_mappings.push(val as i32);
        }
        if self.parsing_rect || self.parsing_window_rect || self.parsing_geometry {
            let mut unknown = false;
            {
                if let Some(r) = self.rect_of() {
                    if key == "x" {
                        r.x = val as u32;
                    } else if key == "y" {
                        r.y = val as u32;
                    } else if key == "width" {
                        r.width = val as u32;
                    } else if key == "height" {
                        r.height = val as u32;
                    } else {
                        unknown = true;
                    }
                }
            }
            if unknown {
                crate::log::errorlog(&format!(
                    "WARNING: unknown key {} in rect\n",
                    self.last_key
                ));
            }
        }
        if self.parsing_swallows {
            if let Some(m) = self.current_swallow.as_mut() {
                if key == "id" {
                    m.id = val as u32;
                    self.swallow_is_empty = false;
                }
                if key == "dock" {
                    m.dock = match_dock_from_i64(val);
                    self.swallow_is_empty = false;
                }
                if key == "insert_where" {
                    m.insert_where = match_insert_where_from_i64(val);
                    self.swallow_is_empty = false;
                }
            }
        }
        if self.parsing_gaps {
            if key == "inner" {
                self.t.cons[con].gaps.inner = val as i32;
            } else if key == "top" {
                self.t.cons[con].gaps.top = val as i32;
            } else if key == "right" {
                self.t.cons[con].gaps.right = val as i32;
            } else if key == "bottom" {
                self.t.cons[con].gaps.bottom = val as i32;
            } else if key == "left" {
                self.t.cons[con].gaps.left = val as i32;
            }
        }
        Ok(())
    }

    /// C: src/load_layout.c:572 json_bool
    fn on_bool(&mut self, val: bool) -> Result<(), String> {
        let con = self.json_node;
        if self.last_key.eq_ignore_ascii_case("focused") && val {
            self.to_focus = Some(con);
        }
        if self.last_key.eq_ignore_ascii_case("sticky") {
            self.t.cons[con].sticky = val;
        }
        if self.parsing_swallows && self.last_key.eq_ignore_ascii_case("restart_mode") {
            if let Some(m) = self.current_swallow.as_mut() {
                m.restart_mode = val;
            }
            self.swallow_is_empty = false;
        }
        Ok(())
    }

    /// C: src/load_layout.c:592 json_double
    fn on_double(&mut self, val: f64) -> Result<(), String> {
        if self.last_key.eq_ignore_ascii_case("percent") {
            self.t.cons[self.json_node].percent = val;
        }
        Ok(())
    }
}

/// C: src/match.c:26 match_init
fn match_init() -> Match {
    Match {
        urgent: MatchUrgency::U_DONTCHECK,
        window_mode: MatchWindowMode::WM_ANY,
        /* we use this as the placeholder value for "not set". */
        window_type: u32::MAX,
        id: XCB_WINDOW_NONE,
        ..Default::default()
    }
}

fn contype_from_i64(v: i64) -> ConType {
    match v {
        0 => ConType::CT_ROOT,
        1 => ConType::CT_OUTPUT,
        2 => ConType::CT_CON,
        3 => ConType::CT_FLOATING_CON,
        4 => ConType::CT_WORKSPACE,
        5 => ConType::CT_DOCKAREA,
        _ => ConType::CT_CON,
    }
}

fn fullscreen_mode_from_i64(v: i64) -> crate::data::FullscreenMode {
    match v {
        1 => crate::data::FullscreenMode::CF_OUTPUT,
        2 => crate::data::FullscreenMode::CF_GLOBAL,
        _ => crate::data::FullscreenMode::CF_NONE,
    }
}

fn match_dock_from_i64(v: i64) -> MatchDock {
    match v {
        1 => MatchDock::M_DOCK_ANY,
        2 => MatchDock::M_DOCK_TOP,
        3 => MatchDock::M_DOCK_BOTTOM,
        0 => MatchDock::M_NODOCK,
        _ => MatchDock::M_DONTCHECK,
    }
}

fn match_insert_where_from_i64(v: i64) -> MatchInsertWhere {
    match v {
        1 => MatchInsertWhere::M_ASSIGN_WS,
        2 => MatchInsertWhere::M_BELOW,
        _ => MatchInsertWhere::M_HERE,
    }
}

/// C: src/floating.c:76 floating_check_size
///
/// floating.rs has no port of this function yet; load_layout.c:164 is the only
/// caller in the files this task owns, so the function lives here until the
/// floating module ports it (the C file has 3 more callers).
///
/// The config values (floating_minimum_height / _width) are not part of
/// data.rs's Config yet; with the C defaults (memset 0) the branch
/// "config.floating_minimum_* == 0" is taken, i.e. the sane minimum of 50x75.
fn floating_check_size(t: &mut Tree, floating_con: ConId, prefer_height: bool) {
    /* Define reasonable minimal and maximal sizes for floating windows */
    const FLOATING_SANE_MIN_HEIGHT: i64 = 50;
    const FLOATING_SANE_MIN_WIDTH: i64 = 75;

    let focused_con = crate::con::con_descend_focused(t, floating_con);
    let mut border_rect = crate::con::con_border_style_rect(t, focused_con);
    /* We have to do the opposite calculations that render_con() do
     * to get the exact size we want. */
    border_rect.width = (-(border_rect.width as i32)) as u32;
    border_rect.height = (-(border_rect.height as i32)) as u32;
    /* undo x11 border */
    border_rect.width += (2 * t.cons[focused_con].border_width) as u32;
    border_rect.height += (2 * t.cons[focused_con].border_width) as u32;

    if let Some(window) = t.cons[focused_con].window {
        let mut rect = t.cons[floating_con].rect;
        let bw = border_rect.width as i64;
        let bh = border_rect.height as i64;
        let (min_width, min_height, base_width, base_height, max_width, max_height, min_ar, max_ar, width_inc, height_inc) = {
            let w = &t.windows[window];
            /* ICCCM says: If a base size is not provided, the minimum size is
             * to be used in its place and vice versa. */
            (
                if w.min_width != 0 { w.min_width } else { w.base_width } as i64,
                if w.min_height != 0 { w.min_height } else { w.base_height } as i64,
                if w.base_width != 0 { w.base_width } else { w.min_width } as i64,
                if w.base_height != 0 { w.base_height } else { w.min_height } as i64,
                w.max_width as i64,
                w.max_height as i64,
                w.min_aspect_ratio,
                w.max_aspect_ratio,
                w.width_increment as i64,
                w.height_increment as i64,
            )
        };

        if min_width != 0 {
            let v = (rect.width as i64 - bw).max(min_width);
            rect.width = (v + bw) as u32;
        }
        if min_height != 0 {
            let v = (rect.height as i64 - bh).max(min_height);
            rect.height = (v + bh) as u32;
        }
        if max_width != 0 {
            let v = (rect.width as i64 - bw).min(max_width);
            rect.width = (v + bw) as u32;
        }
        if max_height != 0 {
            let v = (rect.height as i64 - bh).min(max_height);
            rect.height = (v + bh) as u32;
        }

        /* Obey the aspect ratio, if any, unless we are in fullscreen mode. */
        if t.cons[floating_con].fullscreen_mode == crate::data::FullscreenMode::CF_NONE
            && (min_ar > 0.0 || max_ar > 0.0)
        {
            /* The ICCCM says to subtract the base size from the window size
             * for aspect ratio calculations. However, unlike determining the
             * base size itself we must not fall back to using the minimum size
             * in this case according to the ICCCM. */
            let mut width = rect.width as f64 - base_width as f64 - bw as f64;
            let mut height = rect.height as f64 - base_height as f64 - bh as f64;
            let ar = width / height;
            let mut new_ar = -1.0;
            if min_ar > 0.0 && ar < min_ar {
                new_ar = min_ar;
            } else if max_ar > 0.0 && ar > max_ar {
                new_ar = max_ar;
            }
            if new_ar > 0.0 {
                if prefer_height {
                    width = (height * new_ar).round();
                    height = (width / new_ar).round();
                } else {
                    height = (width / new_ar).round();
                    width = (height * new_ar).round();
                }
                rect.width = (width + base_width as f64 + bw as f64) as u32;
                rect.height = (height + base_height as f64 + bh as f64) as u32;
            }
        }

        if height_inc != 0 && rect.height as i64 >= base_height + bh {
            let mut v = rect.height as i64 - base_height - bh;
            v -= v % height_inc;
            rect.height = (v + base_height + bh) as u32;
        }
        if width_inc != 0 && rect.width as i64 >= base_width + bw {
            let mut v = rect.width as i64 - base_width - bw;
            v -= v % width_inc;
            rect.width = (v + base_width + bw) as u32;
        }
        t.cons[floating_con].rect = rect;
    }

    /* Unless user requests otherwise (-1), raise the width/height to
     * reasonable minimum dimensions. The config default is 0 (C: memset in
     * src/config.c), which selects the sane minimum. */
    let bw = border_rect.width as i64;
    let bh = border_rect.height as i64;
    let mut rect = t.cons[floating_con].rect;
    let v = (rect.height as i64 - bh).max(FLOATING_SANE_MIN_HEIGHT);
    rect.height = (v + bh) as u32;
    let v = (rect.width as i64 - bw).max(FLOATING_SANE_MIN_WIDTH);
    rect.width = (v + bw) as u32;
    t.cons[floating_con].rect = rect;
}

/// C: src/load_layout.c:693 tree_append_json
///
/// Appends the layout in buf below con. Returns the yajl error string when the
/// JSON could not be parsed completely (C: the errormsg out-parameter, which
/// cmd_append_layout turns into an IPC error reply). On error the partially
/// created containers are freed, exactly like C.
pub fn tree_append_json(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    con: ConId,
    buf: &[u8],
) -> Option<String> {
    let mut loader = LoadLayout::new(t, b, con);
    let stat = parse_stream(buf, &mut loader);

    if let Err(e) = stat {
        let msg = yajl_error_string(&e);
        crate::log::errorlog(&format!("JSON parsing error: {}\n", msg));
        loader.cleanup_incomplete();
        /* In case not all containers were restored, we need to fix the
         * percentages, otherwise i3 will crash immediately when rendering the
         * next time. */
        con_fix_percent(loader.t, con);
        let to_focus = loader.to_focus;
        if let Some(f) = to_focus {
            con_activate(loader.t, loader.b, f);
        }
        return Some(msg);
    }

    /* A truncated file leaves a chain of incomplete containers behind (C never
     * frees them, which also leaves them in all_cons with a dead parent); the
     * port frees them so the tree invariants hold. */
    if loader.incomplete > 0 {
        crate::log::debuglog("layout ended in the middle of a container, dropping incomplete ones\n");
        loader.cleanup_incomplete();
    }

    con_fix_percent(loader.t, con);
    let to_focus = loader.to_focus;
    if let Some(f) = to_focus {
        con_activate(loader.t, loader.b, f);
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::con::{con_new, con_new_skeleton};
    use crate::dump::dump_node;
    use crate::tree::{tree_init, Tree};

    /// A tree with a root con (C: tree_restore creates a throw-away croot and
    /// appends the layout below it).
    fn setup() -> (Tree, NullBackend, ConId) {
        let mut t = Tree::new();
        let mut b = NullBackend;
        let croot = con_new(&mut t, &mut b, None, None);
        t.croot = Some(croot);
        t.focused = Some(croot);
        (t, b, croot)
    }

    /// Builds a small real tree: root -> output -> content -> workspace ->
    /// two tiled cons, used for the store/restore round trip.
    fn sample_tree() -> (Tree, NullBackend, ConId) {
        let mut t = Tree::new();
        let mut b = NullBackend;
        tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));
        let croot = t.croot.unwrap();
        let output = con_new(&mut t, &mut b, Some(croot), None);
        t.cons[output].name = Some("X11-1".to_string());
        t.cons[output].r#type = ConType::CT_OUTPUT;
        t.cons[output].layout = Layout::L_OUTPUT;
        let content = con_new(&mut t, &mut b, Some(output), None);
        t.cons[content].name = Some("content".to_string());
        t.cons[content].layout = Layout::L_SPLITH;
        let ws = con_new(&mut t, &mut b, Some(content), None);
        t.cons[ws].name = Some("1".to_string());
        t.cons[ws].num = 1;
        t.cons[ws].r#type = ConType::CT_WORKSPACE;
        t.cons[ws].layout = Layout::L_SPLITH;
        let a = con_new(&mut t, &mut b, Some(ws), None);
        t.cons[a].name = Some("a".to_string());
        t.cons[a].layout = Layout::L_SPLITH;
        t.cons[a].rect = Rect::new(0, 0, 640, 800);
        let c = con_new(&mut t, &mut b, Some(ws), None);
        t.cons[c].name = Some("c".to_string());
        t.cons[c].layout = Layout::L_SPLITH;
        t.cons[c].rect = Rect::new(640, 0, 640, 800);
        // con_fix_percent assigns 0.5 / 0.5
        con_fix_percent(&mut t, ws);
        (t, b, croot)
    }

    fn dump(t: &Tree, croot: ConId) -> String {
        let mut gen = crate::dump::JsonGen::new();
        dump_node(t, &mut gen, croot, true);
        gen.get_buf().to_string()
    }

    /// Structural comparison: names, types, layouts, percents and rects.
    fn compare(a: &Tree, ac: ConId, b: &Tree, bc: ConId) {
        assert_eq!(a.cons[ac].name, b.cons[bc].name, "name of con");
        assert_eq!(a.cons[ac].r#type, b.cons[bc].r#type, "type of {:?}", a.cons[ac].name);
        assert_eq!(a.cons[ac].layout, b.cons[bc].layout, "layout of {:?}", a.cons[ac].name);
        assert_eq!(a.cons[ac].rect, b.cons[bc].rect, "rect of {:?}", a.cons[ac].name);
        // Percentages are recomputed by con_fix_percent() at the end of
        // tree_append_json (C: load_layout.c:756), so they are not part of the
        // structural comparison; the round-trip test asserts them explicitly.
        assert_eq!(
            a.cons[ac].swallow_head.len(),
            b.cons[bc].swallow_head.len(),
            "swallows of {:?}",
            a.cons[ac].name
        );
        let an: Vec<ConId> = a.cons[ac].nodes_head.iter().copied().collect();
        let bn: Vec<ConId> = b.cons[bc].nodes_head.iter().copied().collect();
        assert_eq!(an.len(), bn.len(), "child count of {:?}", a.cons[ac].name);
        for (x, y) in an.iter().zip(bn.iter()) {
            compare(a, *x, b, *y);
        }
        let af: Vec<ConId> = a.cons[ac].floating_head.iter().copied().collect();
        let bf: Vec<ConId> = b.cons[bc].floating_head.iter().copied().collect();
        assert_eq!(af.len(), bf.len(), "floating count of {:?}", a.cons[ac].name);
        for (x, y) in af.iter().zip(bf.iter()) {
            compare(a, *x, b, *y);
        }
    }

    /// C: src/load_layout.c:693 tree_append_json — a plain container with name,
    /// rect and percent.
    #[test]
    fn appends_single_con_with_geometry() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{
            "type": "con",
            "name": "a",
            "percent": 0.5,
            "rect": { "x": 10, "y": 20, "width": 300, "height": 400 }
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);

        assert_eq!(t.cons[croot].nodes_head.len(), 1);
        let con = t.cons[croot].nodes_head[0];
        assert_eq!(t.cons[con].name.as_deref(), Some("a"));
        // C: tree_append_json ends with con_fix_percent(con); a single child
        // gets 100% (src/con.c:con_fix_percent).
        assert_eq!(t.cons[con].percent, 1.0);
        assert_eq!(t.cons[con].rect, Rect::new(10, 20, 300, 400));
        // C: json_end_map defaults a missing layout to L_SPLITH
        assert_eq!(t.cons[con].layout, Layout::L_SPLITH);
        // the appended con is attached below croot and registered in all_cons
        assert!(t.all_cons.contains(&con));
        assert_eq!(t.cons[con].parent, Some(croot));
    }

    /// C: load_layout.c:55-215 — nested maps create the whole subtree at once.
    #[test]
    fn appends_nested_split() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{
            "type": "con",
            "name": "outer",
            "layout": "splitv",
            "nodes": [
                { "type": "con", "name": "left", "percent": 0.25 },
                { "type": "con", "name": "right", "percent": 0.75 }
            ]
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);

        let outer = t.cons[croot].nodes_head[0];
        assert_eq!(t.cons[outer].name.as_deref(), Some("outer"));
        assert_eq!(t.cons[outer].layout, Layout::L_SPLITV);
        assert_eq!(t.cons[outer].nodes_head.len(), 2);
        let left = t.cons[outer].nodes_head[0];
        let right = t.cons[outer].nodes_head[1];
        assert_eq!(t.cons[left].name.as_deref(), Some("left"));
        assert_eq!(t.cons[left].percent, 0.25);
        assert_eq!(t.cons[right].name.as_deref(), Some("right"));
        assert_eq!(t.cons[right].percent, 0.75);
        assert_eq!(t.cons[left].parent, Some(outer));
    }

    /// C: load_layout.c:107-117 — swallow criteria on a split container are
    /// dropped (they make no sense there).
    #[test]
    fn swallows_are_removed_from_split_containers() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{
            "type": "con",
            "name": "outer",
            "nodes": [ { "type": "con", "name": "x" } ],
            "swallows": [ { "class": "^XTerm$" } ]
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        let outer = t.cons[croot].nodes_head[0];
        assert!(t.cons[outer].swallow_head.is_empty());
    }

    /// C: load_layout.c:301-331 — swallow criteria on a leaf container.
    #[test]
    fn leaf_swallows_are_restored() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{
            "type": "con",
            "name": "xterm",
            "swallows": [
                { "class": "^XTerm$", "instance": "^xterm$" },
                { "title": "second" }
            ]
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        let con = t.cons[croot].nodes_head[0];
        assert_eq!(t.cons[con].swallow_head.len(), 2);
        let first = &t.cons[con].swallow_head[0];
        assert_eq!(first.class.as_ref().unwrap().pattern, "^XTerm$");
        assert_eq!(first.instance.as_ref().unwrap().pattern, "^xterm$");
        assert_eq!(first.dock, MatchDock::M_DONTCHECK);
        assert_eq!(first.id, XCB_WINDOW_NONE);
        let second = &t.cons[con].swallow_head[1];
        assert_eq!(second.title.as_ref().unwrap().pattern, "second");
    }

    /// C: load_layout.c:201-206 — an empty swallow definition is an error.
    #[test]
    fn empty_swallow_is_rejected() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{ "type": "con", "name": "x", "swallows": [ {} ] }"#;
        let err = tree_append_json(&mut t, &mut b, croot, json).expect("error expected");
        assert!(err.contains("empty swallow definition"), "got {:?}", err);
        // the partially created container is freed again (C: incomplete)
        assert!(t.cons[croot].nodes_head.is_empty());
    }

    /// C: load_layout.c:734-751 — a JSON error returns the yajl error string
    /// and frees the incomplete containers.
    #[test]
    fn invalid_json_reports_error_and_rolls_back() {
        let (mut t, mut b, croot) = setup();
        let before = t.all_cons.len();
        let err = tree_append_json(&mut t, &mut b, croot, b"{ \"type\": }").expect("error");
        assert!(err.starts_with("parse error: "), "got {:?}", err);
        assert!(t.cons[croot].nodes_head.is_empty());
        assert_eq!(t.all_cons.len(), before);
    }

    /// C: load_layout.c:734 — yajl is a streaming parser and load_layout.c only
    /// inspects yajl_parse() (not yajl_complete_parse()), so a file that ends in
    /// the middle of a value is accepted: the value never completes and no
    /// container is attached. Verified against i3 4.25.1 (cmd_append_layout
    /// replies success for "{ \"type\": \"con\", ").
    #[test]
    fn truncated_json_is_accepted_like_yajl() {
        let (mut t, mut b, croot) = setup();
        let before = t.all_cons.len();
        assert_eq!(tree_append_json(&mut t, &mut b, croot, b"{ \"type\": \"con\", "), None);
        assert!(t.cons[croot].nodes_head.is_empty());
        // the port frees the incomplete containers (C leaks them)
        assert_eq!(t.all_cons.len(), before);
        // a truncated array is accepted as well
        assert_eq!(tree_append_json(&mut t, &mut b, croot, b"[1,2,3"), None);
    }

    /// C: load_layout.c:706-708 — comments and multiple top-level values are
    /// allowed (both are yajl options).
    #[test]
    fn comments_and_multiple_values_are_accepted() {
        let (mut t, mut b, croot) = setup();
        let json = br#"
            // a line comment
            { "type": "con", "name": "a" /* inline */ }
            { "type": "con", "name": "b" }
        "#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        assert_eq!(t.cons[croot].nodes_head.len(), 2);
        let a = t.cons[croot].nodes_head[0];
        let bcon = t.cons[croot].nodes_head[1];
        assert_eq!(t.cons[a].name.as_deref(), Some("a"));
        assert_eq!(t.cons[bcon].name.as_deref(), Some("b"));
    }

    /// C: load_layout.c:377-395 / :396-417 — type, layout and border strings.
    #[test]
    fn string_properties_are_mapped() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{
            "type": "con",
            "name": "n",
            "border": "1pixel",
            "layout": "tabbed",
            "orientation": "vertical",
            "last_split_layout": "splitv",
            "scratchpad_state": "changed",
            "sticky": true,
            "sticky_group": "grp",
            "title_format": "%title"
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        let con = t.cons[croot].nodes_head[0];
        assert_eq!(t.cons[con].border_style, crate::data::BorderStyle::BS_PIXEL);
        assert_eq!(t.cons[con].current_border_width, 1);
        assert_eq!(t.cons[con].layout, Layout::L_TABBED);
        assert_eq!(t.cons[con].last_split_layout, Layout::L_SPLITV);
        assert_eq!(t.cons[con].scratchpad_state, ScratchpadState::SCRATCHPAD_CHANGED);
        assert!(t.cons[con].sticky);
        assert_eq!(t.cons[con].sticky_group.as_deref(), Some("grp"));
        assert_eq!(t.cons[con].title_format.as_deref(), Some("%title"));
    }

    /// C: load_layout.c:119-139 — workspaces get a name, a unique name and a
    /// number, and the workspace_layout key is honoured.
    #[test]
    fn workspace_name_clash_is_renamed() {
        let (mut t, mut b, croot) = setup();
        // an existing workspace called "1" below the same tree
        // an existing workspace called "1" (get_existing_workspace_by_name
        // only looks below outputs, so build root -> output -> content -> ws)
        let output = con_new_skeleton(&mut t, &mut b, Some(croot), None);
        t.cons[output].r#type = ConType::CT_OUTPUT;
        t.cons[output].name = Some("X11-1".to_string());
        let content = con_new_skeleton(&mut t, &mut b, Some(output), None);
        t.cons[content].r#type = ConType::CT_CON;
        t.cons[content].name = Some("content".to_string());
        let existing = con_new_skeleton(&mut t, &mut b, Some(content), None);
        t.cons[existing].r#type = ConType::CT_WORKSPACE;
        t.cons[existing].name = Some("1".to_string());

        let json = br#"{ "type": "workspace", "name": "1", "num": 1 }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        let ws = *t.cons[croot].nodes_head.back().unwrap();
        assert_eq!(t.cons[ws].r#type, ConType::CT_WORKSPACE);
        assert_eq!(t.cons[ws].name.as_deref(), Some("1_1"));
        assert_eq!(t.cons[ws].num, 1);
    }

    /// C: load_layout.c:119-139 — a nameless workspace becomes "unnamed".
    #[test]
    fn unnamed_workspace_gets_a_name() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{ "type": "workspace" }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        let ws = t.cons[croot].nodes_head[0];
        assert_eq!(t.cons[ws].name.as_deref(), Some("unnamed"));
    }

    /// C: load_layout.c:217-254 — the focus array reorders the focus stack by
    /// the old container ids.
    #[test]
    fn focus_array_reorders_the_focus_stack() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{
            "type": "con",
            "name": "outer",
            "nodes": [
                { "type": "con", "name": "a", "id": 101 },
                { "type": "con", "name": "b", "id": 102 }
            ],
            "focus": [ 102, 101 ]
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        let outer = t.cons[croot].nodes_head[0];
        let order: Vec<i32> = t.cons[outer]
            .focus_head
            .iter()
            .map(|c| t.cons[*c].old_id)
            .collect();
        assert_eq!(order, vec![102, 101]);
    }

    /// C: load_layout.c:167-177 — marks are only applied when the whole map is
    /// parsed (#2511); non-leaf containers are supported.
    #[test]
    fn marks_are_applied_after_the_map() {
        let (mut t, mut b, croot) = setup();
        // NOTE: i3-save-tree writes "marks" last; a "marks" key before
        // "nodes" resets the pending list of the parent container — exactly as
        // in C (json_key sets num_marks = 0).
        let json = br#"{
            "type": "con",
            "name": "outer",
            "nodes": [ { "type": "con", "name": "inner", "marks": [ "three" ] } ],
            "marks": [ "one", "two" ]
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        let outer = t.cons[croot].nodes_head[0];
        let inner = t.cons[outer].nodes_head[0];
        let names: Vec<&str> = t.cons[outer].marks_head.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["one", "two"]);
        let inner_names: Vec<&str> = t.cons[inner].marks_head.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(inner_names, vec!["three"]);
    }

    /// C: load_layout.c:442-476 — the gaps object and the deprecated "mark"
    /// key.
    #[test]
    fn gaps_and_deprecated_mark_are_parsed() {
        let (mut t, mut b, croot) = setup();
        let json = br#"{
            "type": "con",
            "name": "n",
            "gaps": { "inner": 3, "top": 1, "right": 2, "bottom": 3, "left": 4 },
            "mark": "deprecated"
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, croot, json), None);
        let con = t.cons[croot].nodes_head[0];
        assert_eq!(t.cons[con].gaps.inner, 3);
        assert_eq!(t.cons[con].gaps.top, 1);
        assert_eq!(t.cons[con].gaps.right, 2);
        assert_eq!(t.cons[con].gaps.bottom, 3);
        assert_eq!(t.cons[con].gaps.left, 4);
        assert_eq!(t.cons[con].marks_head.len(), 1);
        assert_eq!(t.cons[con].marks_head[0].name, "deprecated");
    }

    /// C: load_layout.c:147-165 — floating containers are attached to the
    /// workspace and get a size when none was supplied.
    #[test]
    fn floating_con_gets_a_size_and_workspace_parent() {
        let (mut t, mut b, croot) = setup();
        let ws = con_new_skeleton(&mut t, &mut b, Some(croot), None);
        t.cons[ws].r#type = ConType::CT_WORKSPACE;
        t.cons[ws].name = Some("1".to_string());
        t.focused = Some(ws);

        let json = br#"{
            "type": "floating_con",
            "name": "float",
            "nodes": [ { "type": "con", "name": "child", "geometry": { "width": 120, "height": 80 } } ]
        }"#;
        assert_eq!(tree_append_json(&mut t, &mut b, ws, json), None);
        assert_eq!(t.cons[ws].floating_head.len(), 1);
        let fc = t.cons[ws].floating_head[0];
        assert_eq!(t.cons[fc].r#type, ConType::CT_FLOATING_CON);
        assert_eq!(t.cons[fc].parent, Some(ws));
        // C: rect is combined from the children, then floating_check_size
        // raises it to the sane minimum (75x50).
        assert!(t.cons[fc].rect.width >= 75, "width = {}", t.cons[fc].rect.width);
        assert!(t.cons[fc].rect.height >= 50, "height = {}", t.cons[fc].rect.height);
    }

    /// C: load_layout.c:629 json_validate
    #[test]
    fn json_validate_accepts_and_rejects() {
        assert!(json_validate(br#"{ "type": "con" }"#, 15));
        assert!(json_validate(b"// comment\n{ }", 13));
        assert!(!json_validate(b"{ \"type\": }", 11));
        assert!(!json_validate(b"{} trailing", 11));
    }

    /// C: load_layout.c:658 json_determine_content
    ///
    /// json_determine_content_string (load_layout.c:613) ignores the "type"
    /// value when content_level > 1, and json_determine_content_deeper is
    /// registered for start_map *and* start_array - so a workspace object
    /// wrapped in an array is classified as CON. Verified against the 4.25.1
    /// baseline binary: append_layout of a one element array holding a
    /// workspace object creates no workspace there either.
    #[test]
    fn json_determine_content_distinguishes_workspaces() {
        assert_eq!(
            json_determine_content(br#"[{ "type": "workspace" }]"#, 24),
            json_content_t::JSON_CONTENT_CON
        );
        // A top level (unwrapped) workspace object is the form the layout
        // files use; that one is detected (t/278, t/542).
        assert_eq!(
            json_determine_content(br#"{ "type": "workspace" }"#, 23),
            json_content_t::JSON_CONTENT_WORKSPACE
        );
        assert_eq!(
            json_determine_content(br#"[{ "type": "con" }]"#, 19),
            json_content_t::JSON_CONTENT_CON
        );
        // no "type" at all defaults to CON
        assert_eq!(
            json_determine_content(br#"[{ "name": "x" }]"#, 16),
            json_content_t::JSON_CONTENT_CON
        );
    }

    /// store_restart_layout -> tree_restore_from_file round trip: the dumped
    /// layout must reproduce the same tree structure.
    #[test]
    fn store_and_restore_round_trip() {
        let (mut t, mut b, croot) = sample_tree();
        let json = dump(&t, croot);
        assert!(json.contains("\"type\""));

        let (mut t2, mut b2, _) = setup();
        let croot2 = t2.croot.unwrap();
        assert_eq!(tree_append_json(&mut t2, &mut b2, croot2, json.as_bytes()), None);

        // tree_restore uses the first child of the throw-away root as croot
        let new_root = t2.cons[croot2].nodes_head[0];
        compare(&t, croot, &t2, new_root);

        // The percentages of the restored workspace are recomputed to 0.5/0.5
        // by con_fix_percent() (C: src/con.c:con_fix_percent).
        let output = t2.cons[new_root]
            .nodes_head
            .iter()
            .copied()
            .find(|&c| t2.cons[c].name.as_deref() == Some("X11-1"))
            .expect("output X11-1");
        let content = t2.cons[output].nodes_head[0];
        let ws = t2.cons[content].nodes_head[0];
        assert_eq!(t2.cons[ws].name.as_deref(), Some("1"));
        let percents: Vec<f64> = t2.cons[ws]
            .nodes_head
            .iter()
            .map(|&c| t2.cons[c].percent)
            .collect();
        assert_eq!(percents, vec![0.5, 0.5]);
    }

    /// The dump of a restored tree must be stable (second round trip).
    #[test]
    fn restore_is_idempotent() {
        let (mut t, mut b, croot) = sample_tree();
        let json1 = dump(&t, croot);
        let (mut t2, mut b2, _) = setup();
        let croot2 = t2.croot.unwrap();
        assert_eq!(tree_append_json(&mut t2, &mut b2, croot2, json1.as_bytes()), None);
        let new_root = t2.cons[croot2].nodes_head[0];
        let json2 = dump(&t2, new_root);

        let (mut t3, mut b3, _) = setup();
        let croot3 = t3.croot.unwrap();
        assert_eq!(tree_append_json(&mut t3, &mut b3, croot3, json2.as_bytes()), None);
        let new_root3 = t3.cons[croot3].nodes_head[0];
        let json3 = dump(&t3, new_root3);
        assert_eq!(json2, json3);
    }
}

