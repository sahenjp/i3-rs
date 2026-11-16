//! 1:1 port of the GET_TREE reply JSON generation of i3.
//!
//! There is no include/dump.c in the i3 sources: the dump code lives in
//! C: src/ipc.c (dump_rect:234, dump_gaps:248, dump_node:361). The yajl
//! generator itself is replaced by a hand-written [JsonGen] so that the
//! field order of the C output is preserved exactly (yajl emits the values
//! in the order the C code generates them; a serde_json::Value with a
//! BTreeMap would sort the keys alphabetically instead).
//!
//! yajl generator semantics (yajl_gen.c, flags == 0, i.e. no beautify):
//! compact output, "," between map entries / array elements, ":" after
//! map keys, doubles via "%.20g" (appending ".0" when the result contains
//! no fraction/exponent marker), strings escaped like yajl_string_encode.

use crate::con::{con_get_output, con_is_split, con_orientation};
use crate::data::{
    ConType, FloatingState, Layout, Rect, ScratchpadState,
};
use crate::tree::Tree;

use slotmap::Key as _;

/// The interned '_NET_WM_WINDOW_TYPE_*' atoms.
///
/// C: src/atoms.xmacro (A__NET_WM_WINDOW_TYPE_*) — in C these are X11 atoms
/// interned at startup (src/xcb.c get_atom); their numeric values only exist
/// at runtime, but the config parser has to fill match criteria before i3
/// talks to X. The port therefore compares stable ids instead of atoms:
/// crate::manage::window_type_from_atom maps the interned atom that
/// window_update_type reads onto these ids, and this table turns them back
/// into the strings the C ipc.c:552 dump emits.
pub const A__NET_WM_WINDOW_TYPE_NORMAL: u32 = 1;
pub const A__NET_WM_WINDOW_TYPE_DOCK: u32 = 2;
pub const A__NET_WM_WINDOW_TYPE_DIALOG: u32 = 3;
pub const A__NET_WM_WINDOW_TYPE_UTILITY: u32 = 4;
pub const A__NET_WM_WINDOW_TYPE_TOOLBAR: u32 = 5;
pub const A__NET_WM_WINDOW_TYPE_SPLASH: u32 = 6;
pub const A__NET_WM_WINDOW_TYPE_MENU: u32 = 7;
pub const A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU: u32 = 8;
pub const A__NET_WM_WINDOW_TYPE_POPUP_MENU: u32 = 9;
pub const A__NET_WM_WINDOW_TYPE_TOOLTIP: u32 = 10;
pub const A__NET_WM_WINDOW_TYPE_NOTIFICATION: u32 = 11;

/// yajl generator state (yajl_gen.c:14-25 struct yajl_gen_t / yajl_gen_state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    Start,
    MapStart,
    MapKey,
    MapVal,
    ArrayStart,
    InArray,
    Complete,
}

/// Hand-written equivalent of the yajl generator the C code uses
/// (C: src/ipc.c 'yajl_gen gen = ygenalloc()'), with flags == 0.
pub struct JsonGen {
    buf: String,
    state: Vec<St>,
}

impl Default for JsonGen {
    fn default() -> JsonGen {
        JsonGen::new()
    }
}

impl JsonGen {
    /// C: yajl_gen.c yajl_gen_alloc(NULL) — flags are memset to 0.
    pub fn new() -> JsonGen {
        JsonGen {
            buf: String::new(),
            state: vec![St::Start],
        }
    }

    /// C: yajl_gen.c APPENDED_ATOM — update the state after emitting a value.
    fn appended(&mut self) {
        let cur = self.state[self.state.len() - 1];
        let last = self.state.len() - 1;
        self.state[last] = match cur {
            St::Start => St::Complete,
            St::MapStart | St::MapKey => St::MapVal,
            St::ArrayStart => St::InArray,
            St::MapVal => St::MapKey,
            // C: yajl_gen.c APPENDED_ATOM — the "default: break" branch keeps
            // yajl_gen_in_array / yajl_gen_complete unchanged, so that the next
            // array element still gets its ',' separator.
            St::InArray | St::Complete => cur,
        };
    }

    /// C: yajl_gen.c INSERT_SEP (without the beautify whitespace).
    fn sep(&mut self) {
        match self.state[self.state.len() - 1] {
            St::MapKey | St::InArray => self.buf.push(','),
            St::MapVal => self.buf.push(':'),
            _ => {}
        }
    }

    /// C: #define y(map_open) — yajl_gen_map_open.
    pub fn map_open(&mut self) {
        self.sep();
        self.buf.push('{');
        self.state.push(St::MapStart);
    }

    /// C: #define y(map_close) — yajl_gen_map_close.
    pub fn map_close(&mut self) {
        self.state.pop();
        self.buf.push('}');
        self.appended();
    }

    /// C: #define y(array_open) — yajl_gen_array_open.
    pub fn array_open(&mut self) {
        self.sep();
        self.buf.push('[');
        self.state.push(St::ArrayStart);
    }

    /// C: #define y(array_close) — yajl_gen_array_close.
    pub fn array_close(&mut self) {
        self.state.pop();
        self.buf.push(']');
        self.appended();
    }

    /// C: ystr(str) — yajl_gen_string with yajl_string_encode escaping.
    pub fn string(&mut self, s: &str) {
        self.sep();
        self.buf.push('"');
        encode_string(&mut self.buf, s.as_bytes());
        self.buf.push('"');
        self.appended();
    }

    /// C: yajl_gen_string over raw bytes — used by IPC_HANDLER(send_tick)
    /// (C: src/ipc.c:1329 yajl_gen_string(gen, message, message_size)).
    pub fn string_bytes(&mut self, bytes: &[u8]) {
        self.sep();
        self.buf.push('"');
        encode_string(&mut self.buf, bytes);
        self.buf.push('"');
        self.appended();
    }

    /// C: y(integer, x) — yajl_gen_integer.
    pub fn integer(&mut self, v: i64) {
        self.sep();
        self.buf.push_str(&v.to_string());
        self.appended();
    }

    /// C: y(bool, x) — yajl_gen_bool.
    pub fn boolean(&mut self, v: bool) {
        self.sep();
        self.buf.push_str(if v { "true" } else { "false" });
        self.appended();
    }

    /// C: y(null) — yajl_gen_null.
    pub fn null(&mut self) {
        self.sep();
        self.buf.push_str("null");
        self.appended();
    }

    /// C: y(double, x) — yajl_gen_double ("%.20g" + ".0" fixup).
    pub fn double(&mut self, v: f64) {
        self.sep();
        let s = format_g20(v);
        // C: if (strspn(i, "0123456789-") == strlen(i)) strcat(i, ".0");
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
            self.buf.push_str(&s);
            self.buf.push_str(".0");
        } else {
            self.buf.push_str(&s);
        }
        self.appended();
    }

    /// C: yajl_gen_get_buf(gen, &payload, &length).
    pub fn get_buf(&self) -> &str {
        &self.buf
    }

    /// C: y(free) — the buffer is dropped together with the generator.
    pub fn take_buf(&mut self) -> String {
        std::mem::take(&mut self.buf)
    }
}

/// C: yajl_encode.c yajl_string_encode — escape " and backslash, emit
/// backslash b f n r t for the well-known control characters and
/// backslash u00xx for the rest, pass every other byte through (the input
/// is UTF-8).
pub fn encode_string(out: &mut String, bytes: &[u8]) {
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x0C => out.push_str("\\f"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x00..=0x1F => out.push_str(&format!("\\u{:04x}", b)),
            0x20..=0x7E => {
                out.push(b as char);
            }
            _ => {
                /* Bytes >= 0x7F are passed through untouched by yajl (it does
                 * not validate UTF-8). A Rust String must stay valid UTF-8, so
                 * the sequence is copied verbatim when it decodes; a stray
                 * non-UTF-8 byte is emitted as the Latin-1 character of the
                 * same value (the only representation a String allows). */
                let rest = &bytes[i..];
                match std::str::from_utf8(rest) {
                    Ok(s) => {
                        out.push_str(s);
                        return;
                    }
                    Err(err) => {
                        let valid = err.valid_up_to();
                        if valid > 0 {
                            // SAFETY-free: valid_up_to guarantees this prefix.
                            out.push_str(std::str::from_utf8(&rest[..valid]).unwrap());
                            i += valid;
                            continue;
                        }
                        out.push(b as char);
                    }
                }
            }
        }
        i += 1;
    }
}

/// C: yajl_gen.c yajl_gen_double — sprintf(buf, "%.20g", number).
///
/// %g with precision 20 (significant digits): fixed notation when the
/// decimal exponent X satisfies -4 <= X < 20, scientific notation otherwise,
/// trailing zeros of the fraction removed (nearest the decimal point first).
/// Rust's "{:.19e}" supplies the 20 correctly rounded significant digits; the
/// style switch and the zero stripping below reproduce printf.
pub fn format_g20(v: f64) -> String {
    let neg = v.is_sign_negative();
    if v == 0.0 {
        /* %g of a zero is "0"; a negative zero keeps its sign ("-0"). */
        return if neg { "-0".to_string() } else { "0".to_string() };
    }
    if v.is_nan() || v.is_infinite() {
        /* C: isnan/isinf check in yajl_gen_double (caller errors out); the
         * callers in dump_node never pass such values. */
        return "0".to_string();
    }

    let a = v.abs();

    /* 20 significant digits in scientific form: "d.dddddddddddddddddde±x" */
    let sci = format!("{:.19e}", a);
    let epos = sci.find('e').unwrap();
    let mantissa = &sci[..epos];
    let exp10: i32 = sci[epos + 1..].parse().unwrap();
    let digits: Vec<char> = mantissa.chars().filter(|c| *c != '.').collect();

    /// Strip the trailing zeros of the fractional part (and the decimal
    /// point when nothing follows) — printf %g does this to the fraction,
    /// never to a scientific exponent.
    fn strip_zero_fraction(int_part: &str, frac: &str) -> String {
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            int_part.to_string()
        } else {
            format!("{}.{}", int_part, frac)
        }
    }

    const P: i32 = 20;
    let mut s = if exp10 < P && exp10 >= -4 {
        /* fixed style: P-1-exp10 digits after the decimal point */
        if exp10 >= 0 {
            let int_part: String = digits[..(exp10 + 1) as usize].iter().collect();
            let frac: String = digits[(exp10 + 1) as usize..].iter().collect();
            strip_zero_fraction(&int_part, &frac)
        } else {
            /* exp10 < 0: the decimal point sits before the first significant
             * digit, so all P significant digits end up behind it after
             * (-exp10 - 1) leading zeros. */
            let zeros = "0".repeat((-exp10 - 1) as usize);
            let frac: String = digits[..P as usize].iter().collect();
            strip_zero_fraction("0", &format!("{}{}", zeros, frac))
        }
    } else {
        /* scientific style: d.ddd…e±XX (at least two exponent digits) */
        let mant =
            strip_zero_fraction(&digits[0].to_string(), &digits[1..].iter().collect::<String>());
        let exp_sign = if exp10 < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", mant, exp_sign, exp10.abs())
    };

    if neg {
        s.insert(0, '-');
    }
    s
}

/// C: src/ipc.c:234 'dump_rect'
pub fn dump_rect(gen: &mut JsonGen, name: &str, r: Rect) {
    gen.string(name);
    gen.map_open();
    gen.string("x");
    gen.integer(r.x as i32 as i64);
    gen.string("y");
    gen.integer(r.y as i32 as i64);
    gen.string("width");
    gen.integer(r.width as i64);
    gen.string("height");
    gen.integer(r.height as i64);
    gen.map_close();
}

/// C: src/ipc.c:248 'dump_gaps'
pub fn dump_gaps(gen: &mut JsonGen, name: &str, gaps: crate::data::gaps_t) {
    gen.string(name);
    gen.map_open();
    gen.string("inner");
    gen.integer(gaps.inner as i64);

    // TODO: the i3ipc Python modules recognize gaps, but only inner/outer
    // This is currently here to preserve compatibility with that
    gen.string("outer");
    gen.integer(gaps.top as i64);

    gen.string("top");
    gen.integer(gaps.top as i64);
    gen.string("right");
    gen.integer(gaps.right as i64);
    gen.string("bottom");
    gen.integer(gaps.bottom as i64);
    gen.string("left");
    gen.integer(gaps.left as i64);
    gen.map_close();
}

/// C: src/ipc.c:361 'dump_node'
///
/// Generates the JSON object for one Con (recursively for nodes /
/// floating_nodes / focus). 'con' addresses t.cons like the C 'Con *'.
pub fn dump_node(t: &Tree, gen: &mut JsonGen, con: crate::ConId, inplace_restart: bool) {
    let c = &t.cons[con];
    gen.map_open();
    gen.string("id");
    gen.integer(con.data().as_ffi() as i64);

    gen.string("type");
    match c.r#type {
        ConType::CT_ROOT => gen.string("root"),
        ConType::CT_OUTPUT => gen.string("output"),
        ConType::CT_CON => gen.string("con"),
        ConType::CT_FLOATING_CON => gen.string("floating_con"),
        ConType::CT_WORKSPACE => gen.string("workspace"),
        ConType::CT_DOCKAREA => gen.string("dockarea"),
    }

    /* provided for backwards compatibility only. */
    gen.string("orientation");
    if !con_is_split(t, con) {
        gen.string("none");
    } else {
        if con_orientation(t, con) == crate::data::Orientation::HORIZ {
            gen.string("horizontal");
        } else {
            gen.string("vertical");
        }
    }

    gen.string("scratchpad_state");
    match c.scratchpad_state {
        ScratchpadState::SCRATCHPAD_NONE => gen.string("none"),
        ScratchpadState::SCRATCHPAD_FRESH => gen.string("fresh"),
        ScratchpadState::SCRATCHPAD_CHANGED => gen.string("changed"),
    }

    gen.string("percent");
    if c.percent == 0.0 {
        gen.null();
    } else {
        gen.double(c.percent);
    }

    gen.string("urgent");
    gen.boolean(c.urgent);

    gen.string("marks");
    gen.array_open();
    for mark in &c.marks_head {
        gen.string(&mark.name);
    }
    gen.array_close();

    gen.string("focused");
    gen.boolean(t.focused == Some(con));

    if c.r#type != ConType::CT_ROOT && c.r#type != ConType::CT_OUTPUT {
        gen.string("output");
        let output = con_get_output(t, con).expect("dump_node: con has no output");
        gen.string(t.cons[output].name.as_deref().unwrap_or(""));
    }

    gen.string("layout");
    match c.layout {
        Layout::L_DEFAULT => {
            // C: DLOG("About to dump layout=default, this is a bug in the code.\n");
            //    assert(false);
            panic!("BUG: About to dump layout=default, this is a bug in the code.");
        }
        Layout::L_SPLITV => gen.string("splitv"),
        Layout::L_SPLITH => gen.string("splith"),
        Layout::L_STACKED => gen.string("stacked"),
        Layout::L_TABBED => gen.string("tabbed"),
        Layout::L_DOCKAREA => gen.string("dockarea"),
        Layout::L_OUTPUT => gen.string("output"),
    }

    gen.string("workspace_layout");
    match c.workspace_layout {
        Layout::L_DEFAULT => gen.string("default"),
        Layout::L_STACKED => gen.string("stacked"),
        Layout::L_TABBED => gen.string("tabbed"),
        _ => panic!(
            "BUG: About to dump workspace_layout={:?} (none of default/stacked/tabbed), this is a bug.",
            c.workspace_layout
        ),
    }

    gen.string("last_split_layout");
    match c.layout {
        Layout::L_SPLITV => gen.string("splitv"),
        _ => gen.string("splith"),
    }

    gen.string("border");
    match c.border_style {
        crate::data::BorderStyle::BS_NORMAL => gen.string("normal"),
        crate::data::BorderStyle::BS_NONE => gen.string("none"),
        crate::data::BorderStyle::BS_PIXEL => gen.string("pixel"),
    }

    gen.string("current_border_width");
    gen.integer(c.current_border_width as i64);

    dump_rect(gen, "rect", c.rect);
    if crate::con::con_draw_decoration_into_frame(t, con) {
        let mut simulated_deco_rect = c.deco_rect;
        let parent = c.parent.expect("dump_node: con_draw_decoration_into_frame without parent");
        simulated_deco_rect.x = c.rect.x.wrapping_sub(t.cons[parent].rect.x);
        simulated_deco_rect.y = c.rect.y.wrapping_sub(t.cons[parent].rect.y);
        dump_rect(gen, "deco_rect", simulated_deco_rect);
        dump_rect(gen, "actual_deco_rect", c.deco_rect);
    } else {
        dump_rect(gen, "deco_rect", c.deco_rect);
    }
    dump_rect(gen, "window_rect", c.window_rect);
    dump_rect(gen, "geometry", c.geometry);

    gen.string("name");
    if let Some(window) = c.window {
        if let Some(name) = &t.windows[window].name {
            gen.string(name);
        } else if let Some(name) = &c.name {
            gen.string(name);
        } else {
            gen.null();
        }
    } else if let Some(name) = &c.name {
        gen.string(name);
    } else {
        gen.null();
    }

    if let Some(title_format) = &c.title_format {
        gen.string("title_format");
        gen.string(title_format);
    }

    gen.string("window_icon_padding");
    gen.integer(c.window_icon_padding as i64);

    if c.r#type == ConType::CT_WORKSPACE {
        gen.string("num");
        gen.integer(c.num as i64);

        dump_gaps(gen, "gaps", c.gaps);
    }

    gen.string("window");
    if let Some(window) = c.window {
        gen.integer(t.windows[window].id as i64);
    } else {
        gen.null();
    }

    gen.string("window_type");
    if let Some(window) = c.window {
        match t.windows[window].window_type {
            A__NET_WM_WINDOW_TYPE_NORMAL => gen.string("normal"),
            A__NET_WM_WINDOW_TYPE_DOCK => gen.string("dock"),
            A__NET_WM_WINDOW_TYPE_DIALOG => gen.string("dialog"),
            A__NET_WM_WINDOW_TYPE_UTILITY => gen.string("utility"),
            A__NET_WM_WINDOW_TYPE_TOOLBAR => gen.string("toolbar"),
            A__NET_WM_WINDOW_TYPE_SPLASH => gen.string("splash"),
            A__NET_WM_WINDOW_TYPE_MENU => gen.string("menu"),
            A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU => gen.string("dropdown_menu"),
            A__NET_WM_WINDOW_TYPE_POPUP_MENU => gen.string("popup_menu"),
            A__NET_WM_WINDOW_TYPE_TOOLTIP => gen.string("tooltip"),
            A__NET_WM_WINDOW_TYPE_NOTIFICATION => gen.string("notification"),
            _ => gen.string("unknown"),
        }
    } else {
        gen.null();
    }

    if c.window.is_some() && !inplace_restart {
        /* Window properties are useless to preserve when restarting because
         * they will be queried again anyway. However, for i3-save-tree(1),
         * they are very useful and save i3-save-tree dealing with X11. */
        gen.string("window_properties");
        gen.map_open();

        let window = c.window.unwrap();
        let w = &t.windows[window];

        macro_rules! DUMP_PROPERTY {
            ($key:expr, $prop_name:ident) => {
                if let Some(prop) = &w.$prop_name {
                    gen.string($key);
                    gen.string(prop);
                }
            };
        }

        DUMP_PROPERTY!("class", class_class);
        DUMP_PROPERTY!("instance", class_instance);
        DUMP_PROPERTY!("window_role", role);
        DUMP_PROPERTY!("machine", machine);

        if let Some(name) = &w.name {
            gen.string("title");
            gen.string(name);
        }

        gen.string("transient_for");
        if w.transient_for == crate::data::XCB_ATOM_NONE {
            gen.null();
        } else {
            gen.integer(w.transient_for as i64);
        }

        gen.map_close();
    }

    gen.string("nodes");
    gen.array_open();
    if c.r#type != ConType::CT_DOCKAREA || !inplace_restart {
        let nodes: Vec<crate::ConId> = c.nodes_head.iter().copied().collect();
        for node in nodes {
            dump_node(t, gen, node, inplace_restart);
        }
    }
    gen.array_close();

    gen.string("floating_nodes");
    gen.array_open();
    let floating: Vec<crate::ConId> = c.floating_head.iter().copied().collect();
    for node in floating {
        dump_node(t, gen, node, inplace_restart);
    }
    gen.array_close();

    gen.string("focus");
    gen.array_open();
    for node in &c.focus_head {
        gen.integer(node.data().as_ffi() as i64);
    }
    gen.array_close();

    gen.string("fullscreen_mode");
    gen.integer(c.fullscreen_mode as i32 as i64);

    gen.string("sticky");
    gen.boolean(c.sticky);

    gen.string("floating");
    match c.floating {
        FloatingState::FLOATING_AUTO_OFF => gen.string("auto_off"),
        FloatingState::FLOATING_AUTO_ON => gen.string("auto_on"),
        FloatingState::FLOATING_USER_OFF => gen.string("user_off"),
        FloatingState::FLOATING_USER_ON => gen.string("user_on"),
    }

    gen.string("swallows");
    gen.array_open();
    for match_ in &c.swallow_head {
        /* We will generate a new restart_mode match specification after this
         * loop, so skip this one. */
        if match_.restart_mode {
            continue;
        }
        gen.map_open();
        if match_.dock != crate::data::MatchDock::M_DONTCHECK {
            gen.string("dock");
            gen.integer(match_.dock as i32 as i64);
            gen.string("insert_where");
            gen.integer(match_.insert_where as i32 as i64);
        }

        macro_rules! DUMP_REGEX {
            ($re_name:ident) => {
                if let Some(re) = &match_.$re_name {
                    gen.string(stringify!($re_name));
                    gen.string(&re.pattern);
                }
            };
        }

        DUMP_REGEX!(class);
        DUMP_REGEX!(instance);
        DUMP_REGEX!(window_role);
        DUMP_REGEX!(title);
        DUMP_REGEX!(machine);

        gen.map_close();
    }

    if inplace_restart {
        if let Some(window) = c.window {
            gen.map_open();
            gen.string("id");
            gen.integer(t.windows[window].id as i64);
            gen.string("restart_mode");
            gen.boolean(true);
            gen.map_close();
        }
    }
    gen.array_close();

    if inplace_restart && c.window.is_some() {
        gen.string("depth");
        gen.integer(c.depth as i64);
    }

    if inplace_restart && c.r#type == ConType::CT_ROOT && t.previous_workspace_name.is_some() {
        gen.string("previous_workspace_name");
        gen.string(t.previous_workspace_name.as_deref().unwrap());
    }

    gen.map_close();
}

/// C: src/ipc.c:911 'IPC_HANDLER(tree)' — the GET_TREE payload
/// (the locale dance and the ipc_send_client_message call live in ipc.rs).
pub fn tree_payload(t: &Tree) -> String {
    let mut gen = JsonGen::new();
    dump_node(t, &mut gen, t.croot.expect("tree_payload: no croot"), false);
    gen.take_buf()
}

/// C: src/ipc.c:1589 'ipc_marshal_workspace_event' — the payload of a
/// workspace event ("change"/"current"/"old"). Returns the serialized JSON.
pub fn marshal_workspace_event(
    t: &Tree,
    change: &str,
    current: Option<crate::ConId>,
    old: Option<crate::ConId>,
) -> String {
    let mut gen = JsonGen::new();

    gen.map_open();

    gen.string("change");
    gen.string(change);

    gen.string("current");
    match current {
        None => gen.null(),
        Some(current) => dump_node(t, &mut gen, current, false),
    }

    gen.string("old");
    match old {
        None => gen.null(),
        Some(old) => dump_node(t, &mut gen, old, false),
    }

    gen.map_close();

    gen.take_buf()
}

/// C: src/ipc.c:1642 'ipc_send_window_event' — payload builder part.
pub fn window_event_payload(t: &Tree, property: &str, con: crate::ConId) -> String {
    let mut gen = JsonGen::new();

    gen.map_open();

    gen.string("change");
    gen.string(property);

    gen.string("container");
    dump_node(t, &mut gen, con, false);

    gen.map_close();

    gen.take_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::data::Window;
    use crate::tree::{tree_init, tree_open_con};

    /// Test fixture: the same tree layout the i3 test suite starts from
    /// (C: testcases use a 1280x800 screen with one output "fake-0" and a
    /// workspace "1" created on its content container).
    fn test_tree() -> (Tree, crate::ConId, crate::ConId) {
        let mut t = Tree::new();
        let mut b = NullBackend;
        tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));

        let croot = t.croot;
        let output = crate::con::con_new(&mut t, &mut b, croot, None);
        t.cons[output].r#type = ConType::CT_OUTPUT;
        // C: src/randr.c:371 output_init_con — con->layout = L_OUTPUT;
        t.cons[output].layout = Layout::L_OUTPUT;
        t.cons[output].name = Some("fake-0".to_string());
        t.cons[output].rect = Rect::new(0, 0, 1280, 800);
        t.focus_insert_tail(t.croot.unwrap(), output);

        let output_id = t.outputs.len();
        let mut output_data = crate::data::Output::default();
        output_data.active = true;
        output_data.primary = true;
        output_data.rect = Rect::new(0, 0, 1280, 800);
        output_data.con = Some(output);
        output_data.names_head.push_back(crate::data::output_name {
            name: "fake-0".to_string(),
        });
        t.outputs.push_back(output_data);

        let content = crate::con::con_new(&mut t, &mut b, Some(output), None);
        t.cons[content].r#type = ConType::CT_CON;
        t.cons[content].layout = Layout::L_SPLITH;
        t.cons[content].rect = Rect::new(0, 0, 1280, 800);
        t.nodes_insert_tail(output, content);

        let ws = create_workspace_on_output(&mut t, &mut b, output_id, content);
        t.focused = Some(t.cons[ws].nodes_head.front().copied().unwrap_or(ws));
        (t, output, ws)
    }

    use crate::workspace::create_workspace_on_output;

    /// C: the JSON emitted by dump_node starts with exactly the keys in the
    /// C order of C: src/ipc.c:361 dump_node (checked for the CT_ROOT).
    #[test]
    fn dump_node_single_con_key_order() {
        let (mut t, _output, _ws) = test_tree();
        let mut b = NullBackend;
        let con = tree_open_con(&mut t, &mut b, None, None);

        let root = t.croot.unwrap();
        let json = tree_payload(&t);

        let expected_prefix = format!(
            r#"{{"id":{},"type":"root","orientation":"horizontal","scratchpad_state":"none","percent":null,"urgent":false,"marks":[],"focused":false,"layout":"splith","workspace_layout":"default","last_split_layout":"splith","border":"normal","current_border_width":-1,"rect":{{"x":0,"y":0,"width":1280,"height":800}},"deco_rect":{{"x":0,"y":0,"width":0,"height":0}},"window_rect":{{"x":0,"y":0,"width":0,"height":0}},"geometry":{{"x":0,"y":0,"width":0,"height":0}},"name":"root","window_icon_padding":-1,"window":null,"window_type":null,"nodes":["#,
            root.data().as_ffi() as i64
        );
        assert!(json.starts_with(&expected_prefix), "{}", json);

        // The opened con is a leaf: orientation none.
        let con_json_prefix = format!(
            r#"{{"id":{},"type":"con","orientation":"none""#,
            con.data().as_ffi() as i64
        );
        assert!(json.contains(&con_json_prefix), "{}", json);
    }

    /// C: dump_node emits a window con with the window properties in the
    /// C order (class, instance, window_role, machine, title, transient_for);
    /// only the non-NULL ones appear (C: ipc.c:585-604 DUMP_PROPERTY).
    #[test]
    fn dump_node_window_properties() {
        let (mut t, _output, _ws) = test_tree();
        let mut b = NullBackend;

        let window = t.windows.insert(Window {
            id: 0x00c0ffee,
            class_class: Some("XTerm".to_string()),
            class_instance: Some("xterm".to_string()),
            name: Some("title".to_string()),
            ..Default::default()
        });
        t.window_index.insert(0x00c0ffee, window);

        let con = tree_open_con(&mut t, &mut b, None, Some(window));
        t.cons[con].window = Some(window);

        let json = tree_payload(&t);
        let expected = r#""window_properties":{"class":"XTerm","instance":"xterm","title":"title","transient_for":null}"#;
        assert!(json.contains(expected), "{}", json);
    }

    /// C: dump_node "percent" — a double is formatted like yajl
    /// (yajl_gen.c: sprintf(buf, "%.20g", number)), i.e. 20 significant
    /// digits with %g's fixed/scientific style switch and trailing-zero
    /// removal. Every expected value below is the output of glibc's
    /// printf("%.20g", value) for the same double.
    #[test]
    fn format_g20_matches_printf() {
        assert_eq!(format_g20(0.5), "0.5");
        assert_eq!(format_g20(1.0), "1");
        assert_eq!(format_g20(2.0 / 3.0), "0.66666666666666662966");
        assert_eq!(format_g20(-0.25), "-0.25");
        assert_eq!(format_g20(1e20), "1e+20");
        assert_eq!(format_g20(0.0), "0");
        // %.20g of 1.0/3.0, 0.1, 1e-5, 1e-4 (glibc).
        assert_eq!(format_g20(1.0 / 3.0), "0.33333333333333331483");
        assert_eq!(format_g20(0.1), "0.10000000000000000555");
        assert_eq!(format_g20(1e-5), "1.0000000000000000818e-05");
        assert_eq!(format_g20(1e-4), "0.00010000000000000000479");
        // 1234.5 — fixed style, trailing zeros removed.
        assert_eq!(format_g20(1234.5), "1234.5");
        // 1e19 stays in fixed style (X < P), 1e20 switches to scientific.
        assert_eq!(format_g20(1e19), "10000000000000000000");
        assert_eq!(format_g20(2.5), "2.5");
        assert_eq!(format_g20(-0.0), "-0");
    }

    /// C: JsonGen double() appends ".0" when the %.20g output has no
    /// fraction (yajl_gen.c yajl_gen_double strspn fixup).
    #[test]
    fn double_gets_dot_zero() {
        let mut gen = JsonGen::new();
        gen.map_open();
        gen.string("percent");
        gen.double(2.0);
        gen.map_close();
        assert_eq!(gen.get_buf(), r#"{"percent":2.0}"#);
    }

    /// C: yajl string escaping (yajl_encode.c yajl_string_encode): quote and
    /// backslash, plus the short escapes b/f/n/r/t and \u00XX for every other
    /// byte below 0x20. 0x01 has no short form, so it is emitted as \u0001.
    #[test]
    fn string_escaping() {
        let mut gen = JsonGen::new();
        gen.array_open();
        gen.string("a\"b\\c\nd\u{0001}e");
        gen.array_close();
        // The expected text is ["a\"b\\c\nd\u0001e"].
        // yajl_string_encode spells the 0x01 byte out as \u0001 (no short
        // escape exists below 0x20).
        let q = char::from_u32(34).unwrap();
        let bs = char::from_u32(92).unwrap();
        let expected = format!("[{q}a{bs}{q}b{bs}{bs}c{bs}nd{bs}u0001e{q}]", q = q, bs = bs);
        assert_eq!(gen.get_buf(), expected);
    }

    /// C: yajl_string_encode passes UTF-8 through unchanged (it does not
    /// escape bytes >= 0x7F, unlike serde_json which keeps them raw too but
    /// would re-encode per byte if the encoder were byte-naive).
    #[test]
    fn string_utf8_passthrough() {
        let mut gen = JsonGen::new();
        gen.array_open();
        gen.string("日本語 ünïcode");
        gen.array_close();
        assert_eq!(gen.get_buf(), "[\"日本語 ünïcode\"]");
    }

    /// C: src/ipc.c:1589 ipc_marshal_workspace_event — the workspace event
    /// payload is {"change":…,"current":<node|null>,"old":<node|null>}.
    #[test]
    fn marshal_workspace_event_shape() {
        let (t, _output, ws) = test_tree();
        let json = marshal_workspace_event(&t, "focus", Some(ws), None);
        assert!(json.starts_with("{\"change\":\"focus\",\"current\":{\"id\":"), "{}", json);
        assert!(json.ends_with(",\"old\":null}"), "{}", json);
        // The current node is dumped with the full dump_node object.
        assert!(json.contains(",\"type\":\"workspace\","), "{}", json);
        // No current/old node at all: both are null.
        let json = marshal_workspace_event(&t, "init", None, None);
        assert_eq!(json, r#"{"change":"init","current":null,"old":null}"#);
    }

    /// C: src/ipc.c:1642 ipc_send_window_event — {"change":<prop>,
    /// "container":<node>} with the container dumped like dump_node.
    #[test]
    fn window_event_payload_shape() {
        let (mut t, _output, ws) = test_tree();
        let con = t.cons[ws].nodes_head.front().copied().unwrap_or(ws);
        let json = window_event_payload(&t, "title", con);
        assert!(json.starts_with("{\"change\":\"title\",\"container\":{\"id\":"), "{}", json);
        assert!(json.ends_with('}'), "{}", json);
        assert!(json.contains("\"fullscreen_mode\":"), "{}", json);
    }

    /// C: src/ipc.c:456-460 and 706-728 — the CT_WORKSPACE extras ("num" and
    /// the "gaps" object in its C field order inner/outer/top/right/bottom/
    /// left) appear right after "window_icon_padding"; the depth and
    /// previous_workspace_name keys only exist for inplace restarts.
    #[test]
    fn dump_workspace_num_and_gaps_order() {
        let (mut t, _output, ws) = test_tree();
        t.cons[ws].gaps = crate::data::gaps_t {
            inner: 5,
            top: 1,
            right: 2,
            bottom: 3,
            left: 4,
            ..Default::default()
        };
        let mut gen = JsonGen::new();
        dump_node(&t, &mut gen, ws, false);
        let json = gen.take_buf();
        let expected = format!(
            "\"window_icon_padding\":-1,\"num\":{},\"gaps\":{{\"inner\":5,\"outer\":1,\"top\":1,\"right\":2,\"bottom\":3,\"left\":4}},\"window\":null",
            t.cons[ws].num
        );
        assert!(json.contains(&expected), "{}", json);
        assert!(!json.contains("\"depth\":"), "{}", json);
        assert!(!json.contains("previous_workspace_name"), "{}", json);
    }

    /// C: src/ipc.c:690-728 — with inplace_restart the swallows array gets a
    /// {"id":<window>,"restart_mode":true} entry and the window con gains
    /// "depth", while "window_properties" is omitted (i3-save-tree relies on
    /// the retaining restart_mode match instead).
    #[test]
    fn dump_node_inplace_restart() {
        let (mut t, _output, _ws) = test_tree();
        let mut b = NullBackend;
        let window = t.windows.insert(Window {
            id: 0x00c0ffee,
            class_class: Some("XTerm".to_string()),
            class_instance: Some("xterm".to_string()),
            name: Some("title".to_string()),
            ..Default::default()
        });
        t.window_index.insert(0x00c0ffee, window);
        let con = tree_open_con(&mut t, &mut b, None, Some(window));
        t.cons[con].window = Some(window);
        t.cons[con].depth = 24;

        let mut gen = JsonGen::new();
        dump_node(&t, &mut gen, con, true);
        let json = gen.take_buf();
        assert!(!json.contains("window_properties"), "{}", json);
        assert!(
            // 0x00c0ffee == 12648430
            json.contains("\"swallows\":[{\"id\":12648430,\"restart_mode\":true}]"),
            "{}",
            json
        );
        // "depth" is the last key of the object (C: ipc.c:706-710).
        assert!(json.ends_with(",\"depth\":24}"), "{}", json);
        // The same con without inplace_restart dumps the window properties
        // and no depth.
        let mut gen = JsonGen::new();
        dump_node(&t, &mut gen, con, false);
        let json = gen.take_buf();
        assert!(json.contains("\"window_properties\":"), "{}", json);
        assert!(!json.contains("\"depth\":"), "{}", json);
    }
}
