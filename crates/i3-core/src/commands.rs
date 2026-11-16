//! 1:1 port of src/commands.c — all command handler functions (see
//! commands_parser.c), implemented on top of the [CommandHooks] trait of the
//! i3-spec parser state machine.
//!
//! C: src/commands.c (2537 lines)
//!
//! C globals used by the handlers (focused, croot, config, …) live on
//! [Tree]; the yajl reply generator is an optional [JsonGen] (C: NULL
//! json_gen produces no reply) and the ipc_client is kept as an optional
//! fd (C: cmd_output->client).

use std::collections::VecDeque;

use slotmap::Key as _;

use i3_spec::commands_parser::{
    parse_command, CommandHooks, CommandParserCtx, CommandResult, Owindow,
};

use crate::backend::ConBackend;
use crate::con::{
    con_activate, con_activate_unblock, con_attach, con_close, con_descend_focused,
    con_detach, con_fix_percent, con_focus, con_fullscreen_permits_focusing, con_get_fullscreen_con,
    con_get_output, con_get_workspace, con_inside_floating, con_is_docked, con_is_floating,
    con_is_internal, con_mark, con_mark_toggle, con_move_to_mark, con_move_to_output,
    con_move_to_workspace, con_num_children, con_orientation, con_rect_size_in_orientation,
    con_set_border_style, con_toggle_layout, con_unmark, ConId,
};
// Only the test module below names FloatingState explicitly.
#[cfg(test)]
use crate::data::FloatingState;
use crate::data::{
    gaps_t, BorderStyle, ConType, Direction, FullscreenMode,
    Layout, MarkMode, Match, MatchDock, MatchUrgency, MatchWindowMode,
    Orientation, Position, Rect, ScratchpadState, WindowId, XCB_WINDOW_NONE, regex,
};
use crate::dump::JsonGen;
use crate::gaps::resize_find_tiling_participants;
use crate::move_::tree_move;
use crate::output::{
    get_output_for_con, get_output_from_string, output_get_content,
    output_primary_name, OutputId,
};
use crate::scratchpad::{scratchpad_move, scratchpad_show};
use crate::tree::{
    get_tree_next_sibling, level_down, level_up, tree_next, tree_open_con, tree_split, Tree,
};
use crate::util::{
    layout_from_name, orientation_from_direction, rect_equals,
    ws_name_to_number,
};
use crate::workspace::{
    get_assigned_output, get_existing_workspace_by_name,
    get_existing_workspace_by_num, workspace_back_and_forth, workspace_back_and_forth_get,
    workspace_move_to_output, workspace_next, workspace_next_on_output, workspace_prev,
    workspace_prev_on_output, workspace_show, workspace_show_by_name, workspace_get,
    workspace_is_visible,
};

/// C: libi3/dpi.c:85 logical_px — without an initialized DPI the C fallback
/// returns the raw value; the X layer (Xft.dpi) is not modelled yet.
/// TODO(X11 port): move to the libi3 port once DPI exists.
fn logical_px(logical: i32) -> i32 {
    logical
}

/// C: libi3/boolstr.c boolstr — true for "1", "yes", "on", "true".
fn boolstr(str: &str) -> bool {
    str.eq_ignore_ascii_case("1")
        || str.eq_ignore_ascii_case("yes")
        || str.eq_ignore_ascii_case("on")
        || str.eq_ignore_ascii_case("true")
}

/// C: libi3/resolve_tilde.c resolve_tilde — replace ~/ with $HOME.
fn resolve_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            let mut resolved = home;
            if !resolved.ends_with('/') {
                resolved.push('/');
            }
            resolved.push_str(rest);
            return resolved;
        }
    }
    path.to_string()
}

/// C: atoi(3) — strtol base 10 without error checking: leading white space
/// and sign are consumed, parsing stops at the first non-digit and an empty
/// digit run yields 0. Used where C calls atoi() on a parser *word* that may
/// carry a trailing "px" (src/commands.c:2479 `logical_px(atoi(value))`).
fn atoi(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        neg = bytes[i] == b'-';
        i += 1;
    }
    let digits_start = i;
    let mut acc: i128 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        if acc <= i32::MAX as i128 {
            acc = acc * 10 + (bytes[i] - b'0') as i128;
        }
        i += 1;
    }
    if i == digits_start {
        return 0;
    }
    let value = acc.clamp(i32::MIN as i128, i32::MAX as i128) as i32;
    if neg {
        value.wrapping_neg()
    } else {
        value
    }
}

/// C: src/util.c:425 parse_long — strtol wrapper; fails on range errors,
/// negative results and trailing garbage. Note the 1:1 quirk: an empty
/// string parses as 0 (strtol's end pointer is the NUL terminator).
pub fn parse_long(str: &str, base: i32) -> Option<i64> {
    let bytes = str.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        neg = bytes[i] == b'-';
        i += 1;
    }
    let mut radix = 10u32;
    if base == 0 {
        if i + 1 < bytes.len() && bytes[i] == b'0' && (bytes[i + 1] | 0x20) == b'x' {
            radix = 16;
            i += 2;
        } else if i < bytes.len() && bytes[i] == b'0' {
            radix = 8;
        }
    } else {
        radix = base as u32;
    }
    let digits_start = i;
    let mut acc: i128 = 0;
    let mut overflow = false;
    while i < bytes.len() {
        let c = bytes[i];
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' if radix == 16 => c - b'a' + 10,
            b'A'..=b'F' if radix == 16 => c - b'A' + 10,
            _ => break,
        };
        if d as u32 >= radix {
            break;
        }
        acc = acc * radix as i128 + d as i128;
        if acc > i64::MAX as i128 {
            overflow = true;
        }
        i += 1;
    }
    if i == digits_start {
        // C: end == str → no conversion; strtol returns 0 with *end == the
        // first character. If that character is NUL (empty input) the C
        // check *end != '\0' passes and the value 0 is returned.
        if i == bytes.len() {
            return Some(0);
        }
        return None;
    }
    if neg {
        acc = -acc;
    }
    if overflow || acc > i64::MAX as i128 || acc < i64::MIN as i128 {
        // C: result == LONG_MIN/LONG_MAX (set to *out, returns false)
        return None;
    }
    let result = acc as i64;
    if result < 0 {
        return None;
    }
    if i != bytes.len() {
        // C: *end != '\0'
        return None;
    }
    Some(result)
}

/// C: src/match.c:26 match_init
fn match_init(match_: &mut Match) {
    *match_ = Match {
        urgent: MatchUrgency::U_DONTCHECK,
        window_mode: MatchWindowMode::WM_ANY,
        /* we use this as the placeholder value for "not set". */
        window_type: u32::MAX,
        ..Default::default()
    };
}

/// C: src/match.c:39 match_is_empty
pub fn match_is_empty(match_: &Match) -> bool {
    match_.title.is_none()
        && match_.mark.is_none()
        && match_.application.is_none()
        && match_.class.is_none()
        && match_.instance.is_none()
        && match_.window_role.is_none()
        && match_.workspace.is_none()
        && match_.machine.is_none()
        && match_.urgent == MatchUrgency::U_DONTCHECK
        && match_.id == XCB_WINDOW_NONE
        && match_.window_type == u32::MAX
        && match_.con_id.is_none()
        && match_.dock == MatchDock::M_NODOCK
        && match_.window_mode == MatchWindowMode::WM_ANY
        && !match_.match_all_windows
}

/// C: src/match.c:295 match_parse_property
fn match_parse_property(t: &Tree, match_: &mut Match, ctype: &str, cvalue: Option<&str>) {
    if ctype == "class" {
        match_.class = Some(Box::new(regex::new(cvalue.unwrap_or(""))));
        return;
    }

    if ctype == "instance" {
        match_.instance = Some(Box::new(regex::new(cvalue.unwrap_or(""))));
        return;
    }

    if ctype == "window_role" {
        match_.window_role = Some(Box::new(regex::new(cvalue.unwrap_or(""))));
        return;
    }

    if ctype == "con_id" {
        let cvalue = cvalue.unwrap_or("");
        if cvalue == "__focused__" {
            match_.con_id = t.focused;
            return;
        }

        match parse_long(cvalue, 0) {
            None => {
                match_.error = Some("invalid con_id".to_string());
            }
            Some(parsed) => {
                /* C: match->con_id = (Con *)parsed — the pointer value comes
                 * from the client and is kept even when no such con exists
                 * (the criterion then simply never matches). */
                match_.con_id = Some(lookup_con_by_ffi(parsed as u64));
            }
        }
        return;
    }

    if ctype == "id" {
        match parse_long(cvalue.unwrap_or(""), 0) {
            None => {
                match_.error = Some("invalid id".to_string());
            }
            Some(parsed) => {
                match_.id = parsed as u32;
            }
        }
        return;
    }

    if ctype == "window_type" {
        let cvalue = cvalue.unwrap_or("");
        if cvalue.eq_ignore_ascii_case("normal") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_NORMAL;
        } else if cvalue.eq_ignore_ascii_case("dialog") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_DIALOG;
        } else if cvalue.eq_ignore_ascii_case("utility") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_UTILITY;
        } else if cvalue.eq_ignore_ascii_case("toolbar") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_TOOLBAR;
        } else if cvalue.eq_ignore_ascii_case("splash") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_SPLASH;
        } else if cvalue.eq_ignore_ascii_case("menu") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_MENU;
        } else if cvalue.eq_ignore_ascii_case("dropdown_menu") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU;
        } else if cvalue.eq_ignore_ascii_case("popup_menu") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_POPUP_MENU;
        } else if cvalue.eq_ignore_ascii_case("tooltip") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_TOOLTIP;
        } else if cvalue.eq_ignore_ascii_case("notification") {
            match_.window_type = crate::dump::A__NET_WM_WINDOW_TYPE_NOTIFICATION;
        } else {
            match_.error = Some("unknown window_type value".to_string());
        }

        return;
    }

    if ctype == "con_mark" {
        match_.mark = Some(Box::new(regex::new(cvalue.unwrap_or(""))));
        return;
    }

    if ctype == "title" {
        match_.title = Some(Box::new(regex::new(cvalue.unwrap_or(""))));
        return;
    }

    if ctype == "urgent" {
        let cvalue = cvalue.unwrap_or("");
        if cvalue.eq_ignore_ascii_case("latest")
            || cvalue.eq_ignore_ascii_case("newest")
            || cvalue.eq_ignore_ascii_case("recent")
            || cvalue.eq_ignore_ascii_case("last")
        {
            match_.urgent = MatchUrgency::U_LATEST;
        } else if cvalue.eq_ignore_ascii_case("oldest") || cvalue.eq_ignore_ascii_case("first") {
            match_.urgent = MatchUrgency::U_OLDEST;
        }
        return;
    }

    if ctype == "workspace" {
        match_.workspace = Some(Box::new(regex::new(cvalue.unwrap_or(""))));
        return;
    }

    if ctype == "machine" {
        match_.machine = Some(Box::new(regex::new(cvalue.unwrap_or(""))));
        return;
    }

    if ctype == "tiling" {
        match_.window_mode = MatchWindowMode::WM_TILING;
        return;
    }

    if ctype == "tiling_from" && cvalue == Some("auto") {
        match_.window_mode = MatchWindowMode::WM_TILING_AUTO;
        return;
    }

    if ctype == "tiling_from" && cvalue == Some("user") {
        match_.window_mode = MatchWindowMode::WM_TILING_USER;
        return;
    }

    if ctype == "floating" {
        match_.window_mode = MatchWindowMode::WM_FLOATING;
        return;
    }

    if ctype == "floating_from" && cvalue == Some("auto") {
        match_.window_mode = MatchWindowMode::WM_FLOATING_AUTO;
        return;
    }

    if ctype == "floating_from" && cvalue == Some("user") {
        match_.window_mode = MatchWindowMode::WM_FLOATING_USER;
        return;
    }

    /* match_matches_window() only checks negatively, so match_all_windows
     * won't actually be used there, but that's OK because if no negative
     * match is found (e.g. because of a more restrictive criterion) the
     * return value of match_matches_window() is true.
     * Setting it here only serves to cause match_is_empty() to return false,
     * otherwise empty criteria rules apply, and that's not what we want. */
    if ctype == "all" {
        match_.match_all_windows = true;
        return;
    }
}

/// Rebuilds a ConId from its serialized (as_ffi) representation — the Rust
/// replacement for C's (Con *)parsed pointer conversions. Like in C, the
/// value is kept even when no con with this id exists (comparisons simply
/// never match).
fn lookup_con_by_ffi(ffi: u64) -> ConId {
    ConId::from(slotmap::KeyData::from_ffi(ffi))
}

/// C: src/commands.c:83 maybe_back_and_forth
///
/// NULL handling: C calls `con_get_workspace(focused)` (commands.c:84) without
/// checking `focused`; a NULL `focused` would crash there (no early return,
/// no assert). The Rust port keeps that contract: a missing focused con is an
/// invariant violation, not a recoverable error, and the startup sequence
/// establishes the invariant like C does (src/main.c:1006 con_activate of the
/// first output's workspace). Returning an error reply here instead would hide
/// a broken tree and diverge from C, so the panic is deliberate.
fn maybe_back_and_forth(runner: &mut CommandRunner, needs_tree_render: &mut bool, name: &str) -> bool {
    let focused = runner.t.focused.expect("maybe_back_and_forth: no focused con");
    let ws = con_get_workspace(runner.t, focused);
    let ws_name = ws.and_then(|w| runner.t.cons[w].name.clone());

    /* If we switched to a different workspace, do nothing */
    if ws_name.as_deref() != Some(name) {
        return false;
    }

    // C: if (config.workspace_auto_back_and_forth) — the tree-layer config
    // subset carries it (configuration -> data::Config via apply_to_tree).
    let workspace_auto_back_and_forth = runner.t.config.workspace_auto_back_and_forth;
    if workspace_auto_back_and_forth {
        workspace_back_and_forth(runner.t, runner.b);
        *needs_tree_render = true;
    }
    true
}

/// C: src/commands.c:103 maybe_auto_back_and_forth_workspace
fn maybe_auto_back_and_forth_workspace(runner: &mut CommandRunner, workspace: ConId) -> ConId {
    // C: config.workspace_auto_back_and_forth (see maybe_back_and_forth).
    let workspace_auto_back_and_forth = runner.t.config.workspace_auto_back_and_forth;
    if !workspace_auto_back_and_forth {
        return workspace;
    }

    let focused = runner.t.focused.expect("no focused con");
    let current = con_get_workspace(runner.t, focused);
    if current == Some(workspace) {
        if let Some(baf) = workspace_back_and_forth_get(runner.t, runner.b) {
            return baf;
        }
    }

    workspace
}

/// C: src/commands.c:818 disable_global_fullscreen
fn disable_global_fullscreen(runner: &mut CommandRunner) {
    let croot = runner.t.croot.expect("disable_global_fullscreen: no croot");
    let fs = con_get_fullscreen_con(runner.t, croot, FullscreenMode::CF_GLOBAL);
    if let Some(fs) = fs {
        crate::con::con_disable_fullscreen(runner.t, runner.b, fs);
    }
}

type Ctx = CommandParserCtx<Match, ConId>;

/// The CommandHooks implementor of i3-core: one instance holds the tree,
/// the backend, the reply generator and the C statics of commands.c
/// (the two user_output_names lists).
/// C: Barconfig.mode is a `bar_mode_t` enum in include/configuration.h:315 and
/// "mode toggle" computes `(current->mode + 1) % 2` (commands.c:2229), i.e.
/// only the M_DOCK/M_HIDE values are produced. The reverse mapping keeps the
/// Rust enum instead of storing a bare int.
fn bar_mode_from_i32(mode: i32) -> crate::config::bar_mode_t {
    match mode {
        1 => crate::config::bar_mode_t::M_HIDE,
        2 => crate::config::bar_mode_t::M_INVISIBLE,
        _ => crate::config::bar_mode_t::M_DOCK,
    }
}

/// C: Barconfig.hidden_state is a `bar_hidden_state_t` enum
/// (include/configuration.h:320); "hidden_state toggle" computes
/// `(current->hidden_state + 1) % 2` (commands.c:2296).
fn bar_hidden_state_from_i32(state: i32) -> crate::config::bar_hidden_state_t {
    match state {
        1 => crate::config::bar_hidden_state_t::S_SHOW,
        _ => crate::config::bar_hidden_state_t::S_HIDE,
    }
}

pub struct CommandRunner<'t, 'b> {
    pub t: &'t mut Tree,
    pub b: &'b mut dyn ConBackend,
    /// C: cmd_output->json_gen (None == no json reply)
    pub gen: Option<JsonGen>,
    /// C: cmd_output->client (an ipc_client; kept as the fd for
    /// cmd_restart's exempt_fd).
    pub client: Option<i32>,
    /// C: src/commands.c:1075 static user_output_names_head names
    /// (cmd_move_con_to_output).
    pub move_output_names: VecDeque<String>,
    /// C: src/commands.c:1751 static user_output_names_head names
    /// (cmd_focus_output).
    pub focus_output_names: VecDeque<String>,
}

impl<'t, 'b> CommandRunner<'t, 'b> {
    pub fn new(t: &'t mut Tree, b: &'b mut dyn ConBackend, gen: Option<JsonGen>, client: Option<i32>) -> CommandRunner<'t, 'b> {
        CommandRunner {
            t,
            b,
            gen,
            client,
            move_output_names: VecDeque::new(),
            focus_output_names: VecDeque::new(),
        }
    }

    /// C: #define ysuccess(success) (commands.c:20)
    fn ysuccess(&mut self, success: bool) {
        if let Some(gen) = self.gen.as_mut() {
            gen.map_open();
            gen.string("success");
            gen.boolean(success);
            gen.map_close();
        }
    }

    /// C: #define yerror(format, ...) (commands.c:29)
    fn yerror(&mut self, message: String) {
        if let Some(gen) = self.gen.as_mut() {
            gen.map_open();
            gen.string("success");
            gen.boolean(false);
            gen.string("error");
            gen.string(&message);
            gen.map_close();
        }
    }

    /// C: #define HANDLE_EMPTY_MATCH (commands.c:59). Returns false when an
    /// error reply was emitted and the handler must return.
    fn handle_empty_match(&mut self, ctx: &mut Ctx) -> bool {
        // HANDLE_INVALID_MATCH
        if let Some(error) = ctx.current_match.error.clone() {
            self.yerror(format!("Invalid match: {}", error));
            return false;
        }

        if match_is_empty(&ctx.current_match) {
            // C: ow->con = focused; (commands.c:70) — no NULL check, a missing
            // focused con crashes C as well (see maybe_back_and_forth).
            let focused = self.t.focused.expect("HANDLE_EMPTY_MATCH: no focused con");
            ctx.owindows.clear();
            ctx.owindows.push(Owindow { con: focused });
        }
        true
    }

    /// C: #define CHECK_MOVE_CON_TO_WORKSPACE (commands.c:231). Returns
    /// false when an error reply was emitted and the handler must return.
    fn check_move_con_to_workspace(&mut self, ctx: &mut Ctx) -> bool {
        if !self.handle_empty_match(ctx) {
            return false;
        }
        if ctx.owindows.is_empty() {
            self.yerror("Nothing to move: specified criteria don't match any window".to_string());
            return false;
        }
        let mut found = false;
        // C removes empty workspaces from the TAILQ while iterating.
        let mut keep: Vec<Owindow<ConId>> = Vec::new();
        for ow in ctx.owindows.drain(..) {
            // C: if (current->con->type == CT_WORKSPACE &&
            //         !con_has_children(current->con)) — empty workspaces are
            // dropped from the owindows list (commands.c:241).
            if self.t.cons[ow.con].r#type == ConType::CT_WORKSPACE
                && !crate::con::con_has_children(self.t, ow.con)
            {
                continue;
            }
            found = true;
            keep.push(ow);
        }
        ctx.owindows = keep;
        if !found {
            self.yerror("Nothing to move: workspace empty".to_string());
            return false;
        }
        true
    }
}

/// The Rust port snapshots ctx.owindows (a Vec<Owindow>) before mutating
/// the tree; Owindow has no Clone in i3-spec, so the snapshot is rebuilt
/// element-wise (C: the TAILQ stays untouched while the handler mutates the
/// tree, which is the same effect).
fn owindows_snapshot(ctx: &Ctx) -> Vec<Owindow<ConId>> {
    ctx.owindows.iter().map(|o| Owindow { con: o.con }).collect()
}

/// C: src/commands.c:223 move_matches_to_workspace
fn move_matches_to_workspace(
    runner: &mut CommandRunner,
    owindows: &[Owindow<ConId>],
    ws: ConId,
) {
    for current in owindows {
        con_move_to_workspace(runner.t, runner.b, current.con, ws, true, false, false);
    }
}

/// C: src/commands.c:375 parse_direction — assumes a valid direction string.
fn parse_direction(str: &str) -> Direction {
    if str == "left" {
        return Direction::D_LEFT;
    }
    if str == "right" {
        return Direction::D_RIGHT;
    }
    if str == "up" {
        return Direction::D_UP;
    }
    if str == "down" {
        return Direction::D_DOWN;
    }
    panic!("Invalid direction. This is a parser bug.");
}

/// C: src/util.c:488 direction_from_orientation_position
fn direction_from_orientation_position(orientation: Orientation, position: Position) -> Direction {
    if orientation == Orientation::HORIZ {
        if position == Position::BEFORE {
            Direction::D_LEFT
        } else {
            Direction::D_RIGHT
        }
    } else if position == Position::BEFORE {
        Direction::D_UP
    } else {
        Direction::D_DOWN
    }
}

/// C: src/randr.c:145 get_output_from_rect
///
/// The implementation lives in output.rs (C: randr.c) because the floating
/// drag/resize path of src/floating.c needs it too.
fn get_output_from_rect(t: &Tree, rect: Rect) -> Option<OutputId> {
    crate::output::get_output_from_rect(t, rect)
}

/// C: src/randr.c:183 output_containing_rect
fn output_containing_rect(t: &Tree, rect: Rect) -> Option<OutputId> {
    crate::output::output_containing_rect(t, rect)
}

/// C: src/floating.c:22 total_outputs_dimensions
fn total_outputs_dimensions(t: &Tree) -> Rect {
    crate::floating::total_outputs_dimensions(t)
}

/// C: src/floating.c:76 floating_check_size
fn floating_check_size(runner: &mut CommandRunner, floating_con: ConId, prefer_height: bool) {
    crate::floating::floating_check_size(runner.t, floating_con, prefer_height);
}

/// C: src/floating.c:489 floating_maybe_reassign_ws
fn floating_maybe_reassign_ws(runner: &mut CommandRunner, con: ConId) -> bool {
    crate::floating::floating_maybe_reassign_ws(runner.t, runner.b, con)
}

/// C: src/floating.c:754 floating_reposition
fn floating_reposition(runner: &mut CommandRunner, con: ConId, newrect: Rect) -> bool {
    crate::floating::floating_reposition(runner.t, runner.b, con, newrect)
}

/// C: src/floating.c:782 floating_resize
fn floating_resize(runner: &mut CommandRunner, floating_con: ConId, x: u32, y: u32) {
    let focused_con = con_descend_focused(runner.t, floating_con);
    if runner.t.cons[focused_con].window.is_none() {
        /* No window is focused. Not resizing. */
        return;
    }
    let (wi, hi) = {
        let window = runner.t.cons[focused_con].window.unwrap();
        (
            runner.t.windows[window].width_increment,
            runner.t.windows[window].height_increment,
        )
    };
    let prefer_height = runner.t.cons[floating_con].rect.width == x;
    runner.t.cons[floating_con].rect.width = x;
    runner.t.cons[floating_con].rect.height = y;
    if wi != 0 {
        let rect = &mut runner.t.cons[floating_con].rect;
        rect.width = rect
            .width
            .wrapping_add((wi as i64 - 1 - rect.width as i64).rem_euclid(wi as i64) as u32);
    }
    if hi != 0 {
        let rect = &mut runner.t.cons[floating_con].rect;
        rect.height = rect
            .height
            .wrapping_add((hi as i64 - 1 - rect.height as i64).rem_euclid(hi as i64) as u32);
    }

    floating_check_size(runner, floating_con, prefer_height);

    /* If this is a scratchpad window, don't auto center it from now on. */
    if runner.t.cons[floating_con].scratchpad_state == ScratchpadState::SCRATCHPAD_FRESH {
        runner.t.cons[floating_con].scratchpad_state = ScratchpadState::SCRATCHPAD_CHANGED;
    }
}

/// C: src/commands.c:392 cmd_resize_floating
fn cmd_resize_floating(runner: &mut CommandRunner, direction_str: &str, floating_con: ConId, px: &mut i32) {
    let old_rect = runner.t.cons[floating_con].rect;
    let focused_con = con_descend_focused(runner.t, floating_con);

    let direction = if direction_str == "height" {
        Direction::D_DOWN
    } else if direction_str == "width" {
        Direction::D_RIGHT
    } else {
        parse_direction(direction_str)
    };
    let orientation = orientation_from_direction(direction);

    /* ensure that resize will take place even if pixel increment is smaller
     * than height increment or width increment. fixes #1011 */
    if let Some(window) = runner.t.cons[focused_con].window {
        let (w_incr, h_incr) = {
            let w = &runner.t.windows[window];
            (w.width_increment, w.height_increment)
        };
        if orientation == Orientation::VERT {
            if *px < 0 {
                *px = if -*px < h_incr { -h_incr } else { *px };
            } else {
                *px = if *px < h_incr { h_incr } else { *px };
            }
        } else if *px < 0 {
            *px = if -*px < w_incr { -w_incr } else { *px };
        } else {
            *px = if *px < w_incr { w_incr } else { *px };
        }
    }

    if orientation == Orientation::VERT {
        runner.t.cons[floating_con].rect.height =
            (runner.t.cons[floating_con].rect.height as i64 + *px as i64) as u32;
    } else {
        runner.t.cons[floating_con].rect.width =
            (runner.t.cons[floating_con].rect.width as i64 + *px as i64) as u32;
    }
    floating_check_size(runner, floating_con, orientation == Orientation::VERT);

    /* Did we actually resize anything or did the size constraints prevent
     * us? If we could not resize, exit now to not move the window. */
    if rect_equals(old_rect, runner.t.cons[floating_con].rect) {
        return;
    }

    if direction == Direction::D_UP {
        runner.t.cons[floating_con].rect.y = runner.t.cons[floating_con]
            .rect
            .y
            .wrapping_sub(runner.t.cons[floating_con].rect.height.wrapping_sub(old_rect.height));
    } else if direction == Direction::D_LEFT {
        runner.t.cons[floating_con].rect.x = runner.t.cons[floating_con]
            .rect
            .x
            .wrapping_sub(runner.t.cons[floating_con].rect.width.wrapping_sub(old_rect.width));
    }

    /* If this is a scratchpad window, don't auto center it from now on. */
    if runner.t.cons[floating_con].scratchpad_state == ScratchpadState::SCRATCHPAD_FRESH {
        runner.t.cons[floating_con].scratchpad_state = ScratchpadState::SCRATCHPAD_CHANGED;
    }
}

/// C: src/resize.c:131 percent_for_1px
fn percent_for_1px(runner: &CommandRunner, con: ConId) -> f64 {
    let parent = runner.t.cons[con].parent.expect("percent_for_1px: no parent");
    let parent_size = con_rect_size_in_orientation(runner.t, parent);
    /* deco_rect.height is subtracted from each child in render_con_split */
    let min_size = if con_orientation(runner.t, parent) == Orientation::HORIZ {
        1
    } else {
        1 + runner.t.cons[con].deco_rect.height as i64
    };
    (min_size as f64) / (parent_size as f64)
}

/// C: src/resize.c:146 resize_neighboring_cons
fn resize_neighboring_cons(
    runner: &mut CommandRunner,
    first: ConId,
    second: ConId,
    px: i32,
    ppt: i32,
) -> bool {
    debug_assert!(px as i64 * ppt as i64 == 0);

    let parent = runner.t.cons[first].parent.expect("resize_neighboring_cons: no parent");
    let (new_first_percent, new_second_percent) = if ppt != 0 {
        (
            runner.t.cons[first].percent + (ppt as f64 / 100.0),
            runner.t.cons[second].percent - (ppt as f64 / 100.0),
        )
    } else {
        /* Convert px change to change in percentages */
        let pct = px as f64 / con_rect_size_in_orientation(runner.t, parent) as f64;
        (
            runner.t.cons[first].percent + pct,
            runner.t.cons[second].percent - pct,
        )
    };
    /* Ensure that no container will be less than 1 pixel in the resizing
     * direction. */
    if new_first_percent < percent_for_1px(runner, first)
        || new_second_percent < percent_for_1px(runner, second)
    {
        return false;
    }

    runner.t.cons[first].percent = new_first_percent;
    runner.t.cons[second].percent = new_second_percent;
    con_fix_percent(runner.t, parent);
    true
}

/// C: src/commands.c:451 cmd_resize_tiling_direction
fn cmd_resize_tiling_direction(
    runner: &mut CommandRunner,
    current: ConId,
    direction: &str,
    px: &mut i32,
    ppt: i32,
) -> bool {
    let mut second: Option<ConId> = None;
    let mut first = current;
    let search_direction = parse_direction(direction);

    let res =
        resize_find_tiling_participants(runner.t, &mut first, &mut second, search_direction, false);
    if !res {
        runner.yerror("No second container found in this direction.".to_string());
        return false;
    }

    let mut px = *px;
    if ppt != 0 {
        /* For backwards compatibility, 'X px or Y ppt' means that ppt is
         * preferred. */
        px = 0;
    }
    resize_neighboring_cons(
        runner,
        first,
        second.expect("cmd_resize_tiling_direction: no second"),
        px,
        ppt,
    )
}

/// C: src/commands.c:470 cmd_resize_tiling_width_height
fn cmd_resize_tiling_width_height(
    runner: &mut CommandRunner,
    current: ConId,
    direction: &str,
    px: i32,
    mut ppt: f64,
) -> bool {
    /* get the appropriate current container (skip stacked/tabbed cons) */
    let mut dummy: Option<ConId> = None;
    let mut current = current;
    let search_direction = if direction == "width" {
        Direction::D_LEFT
    } else {
        Direction::D_DOWN
    };
    let search_result =
        resize_find_tiling_participants(runner.t, &mut current, &mut dummy, search_direction, true);
    if !search_result {
        runner
            .yerror("Failed to find appropriate tiling containers for resize operation".to_string());
        return false;
    }

    /* get the default percentage */
    let parent = runner.t.cons[current].parent.expect("cmd_resize_tiling_width_height: no parent");
    let children = con_num_children(runner.t, parent) as i64;
    let percentage = 1.0 / children as f64;

    /* Ensure all the other children have a percentage set. */
    let parent_children: Vec<ConId> = runner.t.cons[parent].nodes_head.iter().copied().collect();
    for child in &parent_children {
        if runner.t.cons[*child].percent == 0.0 {
            runner.t.cons[*child].percent = percentage;
        }
    }

    let new_current_percent;
    if ppt != 0.0 {
        new_current_percent = runner.t.cons[current].percent + ppt;
    } else {
        /* Convert px change to change in percentages */
        ppt = px as f64 / con_rect_size_in_orientation(runner.t, parent) as f64;
        new_current_percent = runner.t.cons[current].percent + ppt;
    }
    let subtract_percent = ppt / (children - 1) as f64;
    if ppt < 0.0 && new_current_percent < percent_for_1px(runner, current) {
        runner.yerror("Not resizing, container would end with less than 1px".to_string());
        return false;
    }

    /* Ensure that the new percentages are positive. */
    if subtract_percent >= 0.0 {
        for child in &parent_children {
            if *child == current {
                continue;
            }
            if runner.t.cons[*child].percent - subtract_percent < percent_for_1px(runner, *child) {
                runner.yerror(format!(
                    "Not resizing, already at minimum size (child {} would end up with a size of {:.0}",
                    child.data().as_ffi(),
                    runner.t.cons[*child].percent - subtract_percent
                ));
                return false;
            }
        }
    }

    runner.t.cons[current].percent = new_current_percent;

    for child in &parent_children {
        if *child == current {
            continue;
        }
        runner.t.cons[*child].percent -= subtract_percent;
    }

    true
}

/// C: src/commands.c:590 resize_set_tiling
fn resize_set_tiling(
    runner: &mut CommandRunner,
    target: ConId,
    resize_orientation: Orientation,
    is_ppt: bool,
    target_size: i64,
) -> bool {
    let search_direction;
    let mode;
    if resize_orientation == Orientation::HORIZ {
        search_direction = Direction::D_LEFT;
        mode = "width";
    } else {
        search_direction = Direction::D_DOWN;
        mode = "height";
    }

    /* Get the appropriate current container (skip stacked/tabbed cons) */
    let mut dummy: Option<ConId> = None;
    let mut target = target;
    resize_find_tiling_participants(runner.t, &mut target, &mut dummy, search_direction, true);

    /* Calculate new size for the target container */
    let ppt;
    let px;
    if is_ppt {
        ppt = target_size as f64 / 100.0 - runner.t.cons[target].percent;
        px = 0;
    } else {
        ppt = 0.0;
        px = (target_size
            - if resize_orientation == Orientation::HORIZ {
                runner.t.cons[target].rect.width
            } else {
                runner.t.cons[target].rect.height
            } as i64) as i32;
    }

    /* Perform resizing and report failure if not possible */
    cmd_resize_tiling_width_height(runner, target, mode, px, ppt)
}

/// C: src/commands.c:672 border_width_from_style
fn border_width_from_style(
    runner: &CommandRunner,
    border_style: BorderStyle,
    border_width: i64,
    con: ConId,
) -> i32 {
    if border_style == BorderStyle::BS_NONE {
        return 0;
    }
    if border_width >= 0 {
        return logical_px(border_width as i32);
    }

    let is_floating = con_inside_floating(runner.t, con).is_some();
    /* Load the configured defaults. */
    if is_floating && border_style == runner.t.config.default_floating_border {
        return runner.t.config.default_floating_border_width;
    } else if !is_floating && border_style == runner.t.config.default_border {
        return runner.t.config.default_border_width;
    } else {
        /* Use some hardcoded values. */
        return logical_px(if border_style == BorderStyle::BS_NORMAL { 2 } else { 1 });
    }
}

/// C: src/commands.c:994-1065 — the user_output_names list helpers.
/// The static TAILQs become fields of CommandRunner.
fn user_output_names_add(t: &mut Tree, list: &mut VecDeque<String>, name: &str) {
    let get_non_primary = name.eq_ignore_ascii_case("nonprimary");
    if get_non_primary || name == "next" {
        /* "next" (or "nonprimary") here work like a wildcard: It "expands" to
         * all available (or non-primary) outputs. */
        for idx in 0..t.outputs.len() {
            if get_non_primary && t.outputs[idx].primary {
                continue;
            }

            let primary_name = output_primary_name(t, idx);
            list.push_back(primary_name);
        }
        return;
    }

    list.push_back(name.to_string());
}

/// C: src/commands.c:1023 user_output_names_find_next
fn user_output_names_find_next(
    runner: &mut CommandRunner,
    names: &[String],
    current_output: Option<OutputId>,
) -> Option<OutputId> {
    let mut target_output: Option<OutputId> = None;
    for (i, uo) in names.iter().enumerate() {
        if target_output.is_none() {
            /* The first available output from the list is used in 2 cases:
             * 1. When we must wrap around the user list. For example, if
             * user specifies outputs A B C and C is current_output.
             * 2. When the current output is not in the user list. For
             * example, user specifies A B C and D is current_output. */
            target_output = get_output_from_string(runner.t, current_output, uo);
        }
        if let Some(current_output) = current_output {
            if output_primary_name(runner.t, current_output).eq_ignore_ascii_case(uo) {
                /* The current output is in the user list */
                for uo in &names[i + 1..] {
                    if let Some(out) = get_output_from_string(runner.t, Some(current_output), uo) {
                        return Some(out);
                    }
                }
                /* We reached the end of the list. We should use the first
                 * available output that, if it exists, is already saved in
                 * target_output. */
                break;
            }
        }
    }
    target_output
}

// // ---------------------------------------------------------------------------
// JSON parsing for 'append_layout' and the in-place restart lives in
// load_layout.rs (C: src/load_layout.c:600-766). These re-exports keep the
// previous call sites in this file unchanged.
// ---------------------------------------------------------------------------

pub use crate::load_layout::{json_content_t, json_determine_content, json_validate};

impl<'t, 'b> CommandHooks for CommandRunner<'t, 'b> {
    type Match = Match;
    type Con = ConId;

    // C: #define y(x, ...) (commands.c:18) — no-op when json_gen is NULL.
    fn y_array_open(&mut self) {
        if let Some(gen) = self.gen.as_mut() {
            gen.array_open();
        }
    }
    fn y_array_close(&mut self) {
        if let Some(gen) = self.gen.as_mut() {
            gen.array_close();
        }
    }
    fn y_map_open(&mut self) {
        if let Some(gen) = self.gen.as_mut() {
            gen.map_open();
        }
    }
    fn y_map_close(&mut self) {
        if let Some(gen) = self.gen.as_mut() {
            gen.map_close();
        }
    }
    fn y_bool(&mut self, b: bool) {
        if let Some(gen) = self.gen.as_mut() {
            gen.boolean(b);
        }
    }
    fn y_string(&mut self, s: &str) {
        if let Some(gen) = self.gen.as_mut() {
            gen.string(s);
        }
    }

    /// C: src/commands.c:132 cmd_criteria_init
    fn cmd_criteria_init(&mut self, ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_criteria_init", line!(), "handling command cmd_criteria_init\n");
        // C: match_free(current_match); match_init(current_match);
        match_init(&mut ctx.current_match);
    }

    /// C: src/commands.c:219 cmd_criteria_add
    fn cmd_criteria_add(&mut self, ctx: &mut Ctx, ctype: Option<&str>, cvalue: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_criteria_add", line!(), "handling command cmd_criteria_add\n");
        match_parse_property(self.t, &mut ctx.current_match, ctype.unwrap_or(""), cvalue);
    }

    /// C: src/commands.c:143 cmd_criteria_match_windows
    fn cmd_criteria_match_windows(&mut self, ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_criteria_match_windows", line!(), "handling command cmd_criteria_match_windows\n");
        /* Clear old queue */
        ctx.owindows.clear();

        /* Go through all cons and find matches */
        let all_cons: Vec<ConId> = self.t.all_cons.iter().copied().collect();
        for con in all_cons {
            /* We use this flag to prevent matching on window-less containers
             * if only window-specific criteria were specified. */
            let mut accept_match = false;

            if let Some(match_con_id) = ctx.current_match.con_id {
                accept_match = true;

                if match_con_id == con {
                    /* con_id matched. */
                } else {
                    continue;
                }
            }

            if ctx.current_match.mark.is_some() && !self.t.cons[con].marks_head.is_empty() {
                accept_match = true;
                let mut matched_by_mark = false;

                for mark in &self.t.cons[con].marks_head {
                    let mark_re = ctx.current_match.mark.as_ref().unwrap();
                    if !mark_re.matches(&mark.name) {
                        continue;
                    }

                    /* match by mark */
                    matched_by_mark = true;
                    break;
                }

                if !matched_by_mark {
                    continue;
                }
            }

            if self.t.cons[con].window.is_some() {
                if crate::match_::match_matches_window(
                    self.t,
                    &ctx.current_match,
                    self.t.cons[con].window,
                ) {
                    accept_match = true;
                } else {
                    continue;
                }
            }

            if accept_match {
                ctx.owindows.push(Owindow { con });
            }
        }
    }

    /// C: src/commands.c:263 cmd_move_con_to_workspace
    fn cmd_move_con_to_workspace(&mut self, ctx: &mut Ctx, which: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_move_con_to_workspace", line!(), "handling command cmd_move_con_to_workspace\n");
        let which = which.unwrap_or("");
        if !self.check_move_con_to_workspace(ctx) {
            return;
        }

        /* get the workspace */
        let ws = if which == "next" {
            workspace_next(self.t)
        } else if which == "prev" {
            workspace_prev(self.t)
        } else if which == "next_on_output" {
            workspace_next_on_output(self.t)
        } else if which == "prev_on_output" {
            workspace_prev_on_output(self.t)
        } else if which == "current" {
            con_get_workspace(
                self.t,
                self.t.focused.expect("cmd_move_con_to_workspace: no focused con"),
            )
        } else {
            self.yerror(format!("BUG: called with which={}", which));
            return;
        };
        // C passes a possibly NULL ws to move_matches_to_workspace
        // (con_move_to_workspace would segfault); keep the bug loud.
        let ws = ws.expect("cmd_move_con_to_workspace: workspace_next() returned NULL");

        move_matches_to_workspace(self, &ctx.owindows, ws);

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:296 cmd_move_con_to_workspace_back_and_forth
    fn cmd_move_con_to_workspace_back_and_forth(&mut self, ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_move_con_to_workspace_back_and_forth", line!(), "handling command cmd_move_con_to_workspace_back_and_forth\n");
        let ws = workspace_back_and_forth_get(self.t, self.b);
        if ws.is_none() {
            self.yerror("No workspace was previously active.".to_string());
            return;
        }

        if !self.handle_empty_match(ctx) {
            return;
        }

        move_matches_to_workspace(self, &ctx.owindows, ws.unwrap());

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:316 cmd_move_con_to_workspace_name
    fn cmd_move_con_to_workspace_name(&mut self, ctx: &mut Ctx, name: Option<&str>, no_auto_back_and_forth: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_move_con_to_workspace_name", line!(), "handling command cmd_move_con_to_workspace_name\n");
        let name = name.unwrap_or("");
        if name.len() >= 2 && name.as_bytes()[..2].eq_ignore_ascii_case(b"__") {
            self.yerror(format!(
                "You cannot move containers to i3-internal workspaces (\"{}\").",
                name
            ));
            return;
        }

        if !self.check_move_con_to_workspace(ctx) {
            return;
        }

        /* get the workspace */
        let ws = workspace_get(self.t, self.b, name);

        let ws = if no_auto_back_and_forth.is_none() {
            maybe_auto_back_and_forth_workspace(self, ws)
        } else {
            ws
        };

        move_matches_to_workspace(self, &ctx.owindows, ws);

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:343 cmd_move_con_to_workspace_number
    fn cmd_move_con_to_workspace_number(&mut self, ctx: &mut Ctx, number: Option<&str>, no_auto_back_and_forth: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_move_con_to_workspace_number", line!(), "handling command cmd_move_con_to_workspace_number\n");
        let which = number.unwrap_or("");
        if !self.check_move_con_to_workspace(ctx) {
            return;
        }

        let parsed_num = ws_name_to_number(which);
        if parsed_num == -1 {
            self.yerror(format!("Could not parse number \"{}\"", which));
            return;
        }

        let ws = match get_existing_workspace_by_num(self.t, parsed_num) {
            Some(ws) => ws,
            None => workspace_get(self.t, self.b, which),
        };

        let ws = if no_auto_back_and_forth.is_none() {
            maybe_auto_back_and_forth_workspace(self, ws)
        } else {
            ws
        };

        move_matches_to_workspace(self, &ctx.owindows, ws);

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:544 cmd_resize
    fn cmd_resize(&mut self, ctx: &mut Ctx, way: Option<&str>, direction: Option<&str>, resize_px: i64, resize_ppt: i64) {
        crate::log::dlog_at(file!(), "cmd_resize", line!(), "handling command cmd_resize\n");
        let way = way.unwrap_or("");
        let direction = direction.unwrap_or("");
        let mut resize_px = resize_px as i32;
        let mut resize_ppt = resize_ppt as i32;
        if way == "shrink" {
            resize_px *= -1;
            resize_ppt *= -1;
        }

        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            /* Don't handle dock windows (issue #1201) */
            if let Some(window) = self.t.cons[current.con].window {
                if self.t.windows[window].dock != crate::data::WindowDock::W_NODOCK {
                    continue;
                }
            }

            if let Some(floating_con) = con_inside_floating(self.t, current.con) {
                cmd_resize_floating(self, direction, floating_con, &mut resize_px);
            } else {
                if direction == "width" || direction == "height" {
                    let ppt = resize_ppt as f64 / 100.0;
                    if !cmd_resize_tiling_width_height(self, current.con, direction, resize_px, ppt) {
                        self.yerror("Cannot resize.".to_string());
                        return;
                    }
                } else {
                    if !cmd_resize_tiling_direction(
                        self,
                        current.con,
                        direction,
                        &mut resize_px,
                        resize_ppt,
                    ) {
                        self.yerror("Cannot resize.".to_string());
                        return;
                    }
                }
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:623 cmd_resize_set
    fn cmd_resize_set(&mut self, ctx: &mut Ctx, cwidth: i64, mode_width: Option<&str>, cheight: i64, mode_height: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_resize_set", line!(), "handling command cmd_resize_set\n");
        if cwidth < 0 || cheight < 0 {
            self.yerror("Dimensions cannot be negative.".to_string());
            return;
        }

        if !self.handle_empty_match(ctx) {
            return;
        }

        let mut cwidth = cwidth;
        let mut cheight = cheight;
        let mut success = true;
        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if let Some(floating_con) = con_inside_floating(self.t, current.con) {
                let output = con_get_output(self.t, floating_con)
                    .expect("cmd_resize_set: no output");
                if cwidth == 0 {
                    cwidth = self.t.cons[floating_con].rect.width as i64;
                } else if mode_width == Some("ppt") {
                    cwidth =
                        (self.t.cons[output].rect.width as f64 * (cwidth as f64 / 100.0)) as i64;
                }
                if cheight == 0 {
                    cheight = self.t.cons[floating_con].rect.height as i64;
                } else if mode_height == Some("ppt") {
                    cheight = (self.t.cons[output].rect.height as f64
                        * (cheight as f64 / 100.0)) as i64;
                }
                floating_resize(self, floating_con, cwidth as u32, cheight as u32);
            } else {
                if self
                    .t
                    .cons[current.con]
                    .window
                    .map_or(false, |w| {
                        self.t.windows[w].dock != crate::data::WindowDock::W_NODOCK
                    })
                {
                    continue;
                }

                if cwidth > 0 {
                    let is_ppt = mode_width == Some("ppt");
                    success &=
                        resize_set_tiling(self, current.con, Orientation::HORIZ, is_ppt, cwidth);
                }
                if cheight > 0 {
                    let is_ppt = mode_height == Some("ppt");
                    success &=
                        resize_set_tiling(self, current.con, Orientation::VERT, is_ppt, cheight);
                }
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(success);
    }

    /// C: src/commands.c:696 cmd_border
    fn cmd_border(&mut self, ctx: &mut Ctx, border_style_str: Option<&str>, border_width: i64) {
        crate::log::dlog_at(file!(), "cmd_border", line!(), "handling command cmd_border\n");
        let border_style_str = border_style_str.unwrap_or("");

        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            let border_style = if border_style_str == "toggle" {
                let bs = self.t.cons[current.con].border_style as i32;
                match (bs + 1) % 3 {
                    0 => BorderStyle::BS_NONE,
                    1 => BorderStyle::BS_PIXEL,
                    _ => BorderStyle::BS_NORMAL,
                }
            } else if border_style_str == "normal" {
                BorderStyle::BS_NORMAL
            } else if border_style_str == "pixel" {
                BorderStyle::BS_PIXEL
            } else if border_style_str == "none" {
                BorderStyle::BS_NONE
            } else {
                self.yerror(format!("BUG: called with border_style={}", border_style_str));
                return;
            };

            /* User changed the border */
            self.t.cons[current.con].max_user_border_style = border_style;
            let con_border_width =
                border_width_from_style(self, border_style, border_width, current.con);
            con_set_border_style(self.t, current.con, border_style, con_border_width);
        }

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(true);
    }

    /// C: src/commands.c:733 cmd_nop
    fn cmd_nop(&mut self, _ctx: &mut Ctx, comment: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_nop", line!(), "handling command cmd_nop\n");
        // C: LOG only reaches stdout when verbose, but it always reaches the
        // SHM ring buffer while shm logging is on — which is what
        // 207-shmlog.t reads back through i3-dump-log.
        crate::log::log_("-------------------------------------------------\n");
        crate::log::log_(&format!(
            "  NOP: {}\n",
            comment.unwrap_or("").chars().take(4000).collect::<String>()
        ));
        crate::log::log_("-------------------------------------------------\n");
        self.ysuccess(true);
    }

    /// C: src/commands.c:744 cmd_append_layout
    fn cmd_append_layout(&mut self, ctx: &mut Ctx, cpath: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_append_layout", line!(), "handling command cmd_append_layout\n");
        let cpath = cpath.unwrap_or("");

        /* Make sure we allow paths like '~/.i3/layout.json' */
        let path = resolve_tilde(cpath);

        let buf = match crate::load_layout::slurp(&path) {
            None => {
                self.yerror(format!("Could not slurp \"{}\".", path));
                /* slurp already logged an error. */
                return;
            }
            Some(buf) => buf,
        };

        if !crate::load_layout::json_validate(&buf, buf.len()) {
            self.yerror(format!("Could not parse \"{}\" as JSON.", path));
            return;
        }

        let content = crate::load_layout::json_determine_content(&buf, buf.len());
        // C: LOG("JSON content = %d\n", content);
        crate::log::log_(&format!("JSON content = {}\n", content as i32));
        if content == json_content_t::JSON_CONTENT_UNKNOWN {
            self.yerror(format!("Could not determine the contents of \"{}\".", path));
            return;
        }

        let focused = self.t.focused.expect("cmd_append_layout: no focused con");
        let mut parent = focused;
        if content == json_content_t::JSON_CONTENT_WORKSPACE {
            parent = output_get_content(
                self.t,
                con_get_output(self.t, parent).expect("cmd_append_layout: no output"),
            )
            .expect("cmd_append_layout: no content");
        } else {
            /* We need to append the layout to a split container, since a leaf
             * container must not have any children (by definition).
             * Note that we explicitly check for workspaces, since they are
             * okay for this purpose, but con_accepts_window() returns false
             * for workspaces. */
            while self.t.cons[parent].r#type != ConType::CT_WORKSPACE
                && !crate::con::con_accepts_window(self.t, parent)
            {
                parent = self.t.cons[parent]
                    .parent
                    .expect("cmd_append_layout: reached the root without a parent to attach to");
            }
        }
        // C: DLOG("Appending to parent=%p instead of focused=%p\n", parent, focused);
        crate::log::dlog_at(
            file!(),
            "cmd_append_layout",
            line!(),
            &format!(
                "Appending to parent={} instead of focused={}\n",
                crate::manage::con_ptr(parent),
                crate::manage::con_ptr(focused)
            ),
        );
        // C: tree_append_json(parent, buf, len, &errormsg)
        let errormsg = crate::load_layout::tree_append_json(self.t, self.b, parent, &buf);
        if let Some(errormsg) = errormsg {
            self.yerror(errormsg.trim_end().to_string());
            /* Note that we continue executing since tree_append_json() has
             * side-effects — user-provided layouts can be partly valid, partly
             * invalid. */
        } else {
            self.ysuccess(true);
        }

        // XXX: This is a bit of a kludge. Theoretically, render_con(parent,
        // false); should be enough, but when sending 'workspace 4;
        // append_layout /tmp/foo.json', the needs_tree_render == true of the
        // workspace command is not executed yet and will be batched with
        // append_layout's needs_tree_render after the parser finished.
        crate::tree::tree_render(self.t, self.b);

        // C: restore_open_placeholder_windows(parent) — the placeholder
        // windows belong to the X layer, which the ConBackend hook does not
        // carry yet (restore_layout.rs owns the implementation).
        crate::restore_layout::restore_open_placeholder_windows(self.t, parent);

        if content == json_content_t::JSON_CONTENT_WORKSPACE {
            self.b.ipc_send_workspace_event(self.t, "restored", parent, None);
        }

        ctx.subcommand_output.needs_tree_render = true;
    }

    /// C: src/commands.c:829 cmd_workspace
    fn cmd_workspace(&mut self, ctx: &mut Ctx, which: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_workspace", line!(), "handling command cmd_workspace\n");
        let which = which.unwrap_or("");

        disable_global_fullscreen(self);

        let ws = if which == "next" {
            workspace_next(self.t)
        } else if which == "prev" {
            workspace_prev(self.t)
        } else if which == "next_on_output" {
            workspace_next_on_output(self.t)
        } else if which == "prev_on_output" {
            workspace_prev_on_output(self.t)
        } else {
            self.yerror(format!("BUG: called with which={}", which));
            return;
        };
        // C would segfault when workspace_next() returns NULL.
        let ws = ws.expect("cmd_workspace: workspace_next() returned NULL");

        workspace_show(self.t, self.b, ws);

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:860 cmd_workspace_number
    fn cmd_workspace_number(&mut self, ctx: &mut Ctx, workspace: Option<&str>, no_auto_back_and_forth: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_workspace_number", line!(), "handling command cmd_workspace_number\n");
        let which = workspace.unwrap_or("");
        let no_auto_back_and_forth = no_auto_back_and_forth.is_some();

        disable_global_fullscreen(self);

        let parsed_num = ws_name_to_number(which);
        if parsed_num == -1 {
            self.yerror(format!(
                "Could not parse initial part of \"{}\" as a number.",
                which
            ));
            return;
        }

        match get_existing_workspace_by_num(self.t, parsed_num) {
            None => {
                self.ysuccess(true);
                workspace_show_by_name(self.t, self.b, which);
                ctx.subcommand_output.needs_tree_render = true;
                return;
            }
            Some(workspace) => {
                let ws_name = self.t.cons[workspace].name.clone();
                if !no_auto_back_and_forth
                    && maybe_back_and_forth(
                        self,
                        &mut ctx.subcommand_output.needs_tree_render,
                        ws_name.as_deref().unwrap_or(""),
                    )
                {
                    self.ysuccess(true);
                    return;
                }
                workspace_show(self.t, self.b, workspace);
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:894 cmd_workspace_back_and_forth
    fn cmd_workspace_back_and_forth(&mut self, ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_workspace_back_and_forth", line!(), "handling command cmd_workspace_back_and_forth\n");
        disable_global_fullscreen(self);

        workspace_back_and_forth(self.t, self.b);

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:908 cmd_workspace_name
    fn cmd_workspace_name(&mut self, ctx: &mut Ctx, workspace: Option<&str>, no_auto_back_and_forth: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_workspace_name", line!(), "handling command cmd_workspace_name\n");
        let name = workspace.unwrap_or("");
        let no_auto_back_and_forth = no_auto_back_and_forth.is_some();

        if name.len() >= 2 && name.as_bytes()[..2].eq_ignore_ascii_case(b"__") {
            self.yerror(format!(
                "You cannot switch to the i3-internal workspaces (\"{}\").",
                name
            ));
            return;
        }

        disable_global_fullscreen(self);

        if !no_auto_back_and_forth
            && maybe_back_and_forth(self, &mut ctx.subcommand_output.needs_tree_render, name)
        {
            self.ysuccess(true);
            return;
        }
        workspace_show_by_name(self.t, self.b, name);

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:934 cmd_mark
    fn cmd_mark(&mut self, ctx: &mut Ctx, mark: Option<&str>, mode: Option<&str>, toggle: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_mark", line!(), "handling command cmd_mark\n");
        if !self.handle_empty_match(ctx) {
            return;
        }

        if ctx.owindows.is_empty() {
            self.yerror("Given criteria don't match a window".to_string());
            return;
        }

        /* Marks must be unique, i.e., no two windows must have the same
         * mark. */
        if ctx.owindows.len() > 1 {
            self.yerror("A mark must not be put onto more than one window".to_string());
            return;
        }

        let mark = mark.unwrap_or("");
        let mark_mode = if mode.is_none() || mode == Some("--replace") {
            MarkMode::MM_REPLACE
        } else {
            MarkMode::MM_ADD
        };
        if toggle.is_some() {
            con_mark_toggle(self.t, self.b, ctx.owindows[0].con, mark, mark_mode);
        } else {
            con_mark(self.t, self.b, ctx.owindows[0].con, mark, mark_mode);
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:967 cmd_unmark
    fn cmd_unmark(&mut self, ctx: &mut Ctx, mark: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_unmark", line!(), "handling command cmd_unmark\n");
        if match_is_empty(&ctx.current_match) {
            con_unmark(self.t, self.b, None, mark);
        } else {
            let owindows = owindows_snapshot(ctx);
            for current in &owindows {
                con_unmark(self.t, self.b, Some(current.con), mark);
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:986 cmd_mode
    fn cmd_mode(&mut self, _ctx: &mut Ctx, mode: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_mode", line!(), "handling command cmd_mode\n");
        let mode = mode.unwrap_or("");
        // C: DLOG("mode=%s\n", mode); switch_mode(mode);
        //
        // The backend hook is ConBackend::switch_mode: the mode list, the key
        // grabs and the IPC mode event belong to bindings.c, which the command
        // layer reaches through the backend (C reaches the global bindings).
        self.b.switch_mode(self.t, mode);
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1071 cmd_move_con_to_output
    fn cmd_move_con_to_output(&mut self, ctx: &mut Ctx, output: Option<&str>, move_workspace: i64) {
        crate::log::dlog_at(file!(), "cmd_move_con_to_output", line!(), "handling command cmd_move_con_to_output\n");
        /* Initialize a data structure that is used to save multiple
         * user-specified output names since this function is called multiple
         * times for each command call. (C: static TAILQ) */
        if let Some(name) = output {
            user_output_names_add(self.t, &mut self.move_output_names, name);
            return;
        }

        if !self.handle_empty_match(ctx) {
            return;
        }

        if self.move_output_names.is_empty() {
            self.yerror("At least one output must be specified".to_string());
            return;
        }

        let mut success = false;
        let owindows = owindows_snapshot(ctx);
        let names: Vec<String> = self.move_output_names.iter().cloned().collect();
        for current in &owindows {
            let ws = con_get_workspace(self.t, current.con)
                .expect("cmd_move_con_to_output: no workspace");
            if con_is_internal(self.t, ws) {
                continue;
            }

            let current_output = get_output_for_con(self.t, ws);
            let target_output = user_output_names_find_next(self, &names, current_output);
            if let Some(target_output) = target_output {
                if move_workspace != 0 {
                    workspace_move_to_output(self.t, self.b, ws, target_output);
                } else {
                    con_move_to_output(self.t, self.b, current.con, target_output, true);
                }
                success = true;
            }
        }
        self.move_output_names.clear();

        ctx.subcommand_output.needs_tree_render = success;
        if success {
            self.ysuccess(true);
        } else {
            self.yerror("No output matched".to_string());
        }
    }

    /// C: src/commands.c:1122 cmd_move_con_to_mark
    fn cmd_move_con_to_mark(&mut self, ctx: &mut Ctx, mark: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_move_con_to_mark", line!(), "handling command cmd_move_con_to_mark\n");
        let mark = mark.unwrap_or("");

        if !self.handle_empty_match(ctx) {
            return;
        }

        let mut result = true;
        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            result &= con_move_to_mark(self.t, self.b, current.con, mark);
        }

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(result);
    }

    /// C: src/commands.c:1142 cmd_floating
    fn cmd_floating(&mut self, ctx: &mut Ctx, floating: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_floating", line!(), "handling command cmd_floating\n");
        let floating_mode = floating.unwrap_or("");

        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if floating_mode == "toggle" {
                crate::floating::toggle_floating_mode(self.t, self.b, current.con, false);
            } else {
                if floating_mode == "enable" {
                    crate::floating::floating_enable(self.t, self.b, current.con, false);
                } else {
                    crate::floating::floating_disable(self.t, self.b, current.con);
                }
            }
            run_assignments_for(self, self.t.cons[current.con].window);
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1174 cmd_split
    fn cmd_split(&mut self, ctx: &mut Ctx, direction: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_split", line!(), "handling command cmd_split\n");
        let direction = direction.unwrap_or("");

        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if con_is_docked(self.t, current.con) {
                continue;
            }

            if direction.as_bytes().first() == Some(&b't') {
                let current_layout = if self.t.cons[current.con].r#type == ConType::CT_WORKSPACE {
                    self.t.cons[current.con].layout
                } else {
                    let parent = self.t.cons[current.con].parent.expect("cmd_split: no parent");
                    self.t.cons[parent].layout
                };
                /* toggling split orientation */
                if current_layout == Layout::L_SPLITH {
                    tree_split(self.t, self.b, current.con, Orientation::VERT);
                } else {
                    tree_split(self.t, self.b, current.con, Orientation::HORIZ);
                }
            } else {
                tree_split(
                    self.t,
                    self.b,
                    current.con,
                    if direction.as_bytes().first() == Some(&b'v') {
                        Orientation::VERT
                    } else {
                        Orientation::HORIZ
                    },
                );
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1213 cmd_kill
    fn cmd_kill(&mut self, ctx: &mut Ctx, kill_mode: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_kill", line!(), "handling command cmd_kill\n");
        let kill_mode_str = kill_mode.unwrap_or("window");

        let kill_mode = if kill_mode_str == "window" {
            crate::data::KillWindow::KILL_WINDOW
        } else if kill_mode_str == "client" {
            crate::data::KillWindow::KILL_CLIENT
        } else {
            self.yerror(format!("BUG: called with kill_mode={}", kill_mode_str));
            return;
        };

        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            con_close(self.t, self.b, current.con, kill_mode);
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1246 cmd_exec
    fn cmd_exec(&mut self, ctx: &mut Ctx, nosn: Option<&str>, command: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_exec", line!(), "handling command cmd_exec\n");
        let no_startup_id = nosn.is_some();

        if !self.handle_empty_match(ctx) {
            return;
        }

        let count = ctx.owindows.len();

        if count > 1 {
            // C: LOG("WARNING: Your criteria for the exec command match %d
            //         containers, so the command will execute this many
            //         times.\n", count);
            crate::log::log_(&format!(
                "WARNING: Your criteria for the exec command match {} containers, so the command will execute this many times.\n",
                count
            ));
        }

        let command = command.unwrap_or("");
        for current in &ctx.owindows {
            // C: start_application(command, no_startup_id,
            //    current->con->window == NULL ? XCB_NONE :
            //    current->con->window->id);
            let window_id = match self.t.cons[current.con].window {
                None => crate::startup::XCB_NONE,
                // C: current->con->window->id
                Some(window) => self.t.windows[window].id,
            };
            self.b
                .start_application(self.t, command, no_startup_id, window_id);
        }

        self.ysuccess(true);
    }

    /// C: src/commands.c:1292 cmd_focus_direction
    fn cmd_focus_direction(&mut self, ctx: &mut Ctx, direction: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_focus_direction", line!(), "handling command cmd_focus_direction\n");
        let direction_str = direction.unwrap_or("");
        if !self.handle_empty_match(ctx) {
            return;
        }

        let mut direction = Direction::D_LEFT;
        let position;
        let mut auto_direction = true;
        if direction_str == "prev" {
            position = Position::BEFORE;
        } else if direction_str == "next" {
            position = Position::AFTER;
        } else {
            auto_direction = false;
            direction = parse_direction(direction_str);
            position = Position::AFTER; // unused in this branch
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            let ws = con_get_workspace(self.t, current.con);
            match ws {
                None => continue,
                Some(ws) => {
                    if con_is_internal(self.t, ws) {
                        continue;
                    }
                }
            }
            let direction = if auto_direction {
                let parent = self.t.cons[current.con]
                    .parent
                    .expect("cmd_focus_direction: no parent");
                let o = con_orientation(self.t, parent);
                direction_from_orientation_position(o, position)
            } else {
                direction
            };
            tree_next(self.t, self.b, current.con, direction);
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1330 cmd_focus_sibling
    fn cmd_focus_sibling(&mut self, ctx: &mut Ctx, direction: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_focus_sibling", line!(), "handling command cmd_focus_sibling\n");
        let direction_str = direction.unwrap_or("");
        if !self.handle_empty_match(ctx) {
            return;
        }

        let direction = if direction_str.starts_with("prev") {
            Position::BEFORE
        } else {
            Position::AFTER
        };
        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            let ws = con_get_workspace(self.t, current.con);
            match ws {
                None => continue,
                Some(ws) => {
                    if con_is_internal(self.t, ws) {
                        continue;
                    }
                }
            }
            let next = get_tree_next_sibling(self.t, current.con, direction);
            if let Some(next) = next {
                if self.t.cons[next].r#type == ConType::CT_WORKSPACE {
                    /* On the workspace level, we need to make sure that the
                     * workspace change happens properly. However,
                     * workspace_show descends focus so we also have to put
                     * focus on the workspace itself to maintain
                     * consistency. See #3997. */
                    workspace_show(self.t, self.b, next);
                    con_focus(self.t, self.b, next);
                } else {
                    con_activate(self.t, self.b, next);
                }
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1365 cmd_focus_window_mode
    fn cmd_focus_window_mode(&mut self, ctx: &mut Ctx, window_mode: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_focus_window_mode", line!(), "handling command cmd_focus_window_mode\n");
        let window_mode = window_mode.unwrap_or("");

        let focused = self.t.focused.expect("cmd_focus_window_mode: no focused con");
        let to_floating = if window_mode == "mode_toggle" {
            !con_inside_floating(self.t, focused).is_some()
        } else if window_mode == "floating" {
            true
        } else {
            // C leaves to_floating false for "tiling" and unknown modes.
            false
        };

        let ws = con_get_workspace(self.t, focused)
            .expect("cmd_focus_window_mode: focused has no workspace");
        let mut success = false;
        let focus_head: Vec<ConId> = self.t.cons[ws].focus_head.iter().copied().collect();
        for current in focus_head {
            if (to_floating && self.t.cons[current].r#type != ConType::CT_FLOATING_CON)
                || (!to_floating && self.t.cons[current].r#type == ConType::CT_FLOATING_CON)
            {
                continue;
            }

            con_activate_unblock(self.t, self.b, con_descend_focused(self.t, current));
            success = true;
            break;
        }

        if success {
            ctx.subcommand_output.needs_tree_render = true;
            self.ysuccess(true);
        } else {
            self.yerror(format!(
                "Failed to find a {} container in workspace.",
                if to_floating { "floating" } else { "tiling" }
            ));
        }
    }

    /// C: src/commands.c:1403 cmd_focus_level
    fn cmd_focus_level(&mut self, ctx: &mut Ctx, level: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_focus_level", line!(), "handling command cmd_focus_level\n");
        let level = level.unwrap_or("");
        let mut success = false;

        /* Focusing the parent can only be allowed if the newly focused
         * container won't escape the fullscreen container. */
        if level == "parent" {
            let focused = self.t.focused;
            if let Some(focused) = focused {
                if let Some(parent) = self.t.cons[focused].parent {
                    if con_fullscreen_permits_focusing(self.t, Some(parent)) {
                        success = level_up(self.t, self.b);
                    }
                    // C: ELOG("'focus parent': Currently in fullscreen, not going up\n")
                }
            }
        }
        /* Focusing a child should always be allowed. */
        else {
            success = level_down(self.t, self.b);
        }

        ctx.subcommand_output.needs_tree_render = success;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(success);
    }

    /// C: src/commands.c:1433 cmd_focus
    fn cmd_focus(&mut self, ctx: &mut Ctx, focus_workspace: i64) {
        crate::log::dlog_at(file!(), "cmd_focus", line!(), "handling command cmd_focus\n");
        let focus_workspace = focus_workspace != 0;

        if match_is_empty(&ctx.current_match) {
            self.yerror(
                "You have to specify which window/container should be focused".to_string(),
            );
            return;
        } else if ctx.owindows.is_empty() {
            self.yerror("No window matches given criteria".to_string());
            return;
        }

        let __i3_scratch = workspace_get(self.t, self.b, "__i3_scratch");
        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            let ws = con_get_workspace(self.t, current.con);
            /* If no workspace could be found, this was a dock window.
             * Just skip it, you cannot focus dock windows. */
            let ws = match ws {
                None => continue,
                Some(ws) => ws,
            };

            /* In case this is a scratchpad window, call scratchpad_show(). */
            if ws == __i3_scratch && !focus_workspace {
                scratchpad_show(self.t, self.b, Some(current.con));
                /* While for the normal focus case we can change focus
                 * multiple times and only a single window ends up focused,
                 * we could show multiple scratchpad windows. So, rather
                 * break here. */
                break;
            }

            if focus_workspace {
                /* Show the workspace of the matched container, without
                 * necessarily focusing it. */
                let ws = maybe_auto_back_and_forth_workspace(self, ws);
                workspace_show(self.t, self.b, ws);
            } else {
                con_activate_unblock(self.t, self.b, current.con);
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(true);
    }

    /// C: src/commands.c:1488 cmd_fullscreen
    fn cmd_fullscreen(&mut self, ctx: &mut Ctx, action: Option<&str>, mode: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_fullscreen", line!(), "handling command cmd_fullscreen\n");
        let action = action.unwrap_or("");
        let fullscreen_mode_str = mode.unwrap_or("");
        let mode = if fullscreen_mode_str == "global" {
            FullscreenMode::CF_GLOBAL
        } else {
            FullscreenMode::CF_OUTPUT
        };

        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if action == "toggle" {
                crate::con::con_toggle_fullscreen(self.t, self.b, current.con, mode as i32);
            } else if action == "enable" {
                crate::con::con_enable_fullscreen(self.t, self.b, current.con, mode);
            } else if action == "disable" {
                crate::con::con_disable_fullscreen(self.t, self.b, current.con);
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1515 cmd_sticky
    fn cmd_sticky(&mut self, ctx: &mut Ctx, action: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_sticky", line!(), "handling command cmd_sticky\n");
        let action = action.unwrap_or("");
        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if self.t.cons[current.con].window.is_none() {
                continue;
            }

            let sticky = if action == "enable" {
                true
            } else if action == "disable" {
                false
            } else if action == "toggle" {
                !self.t.cons[current.con].sticky
            } else {
                false
            };

            self.t.cons[current.con].sticky = sticky;
            // C: ewmh_update_sticky(current->con->window->id, sticky);
            if let Some(window) = self.t.cons[current.con].window {
                let window_id = self.t.windows[window].id;
                self.b.ewmh_update_sticky(window_id, sticky);
            }
        }

        /* A window we made sticky might not be on a visible workspace right
         * now, so we need to make sure it gets pushed to the front now. */
        let focused = self.t.focused;
        crate::output::output_push_sticky_windows(self.t, self.b, focused);

        self.b.ewmh_update_wm_desktop(self.t);

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(true);
    }

    /// C: src/commands.c:1554 cmd_move_direction
    fn cmd_move_direction(&mut self, ctx: &mut Ctx, direction: Option<&str>, amount: i64, mode: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_move_direction", line!(), "handling command cmd_move_direction\n");
        let direction_str = direction.unwrap_or("");
        if !self.handle_empty_match(ctx) {
            return;
        }

        let direction = parse_direction(direction_str);

        let is_ppt = mode == Some("ppt");

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if con_is_floating(self.t, current.con) {
                let newrect = {
                    let parent = self.t.cons[current.con]
                        .parent
                        .expect("cmd_move_direction: floating con has no parent");
                    self.t.cons[parent].rect
                };
                let output = con_get_output(self.t, current.con)
                    .expect("cmd_move_direction: no output");
                let offset = if is_ppt {
                    let dim = if direction == Direction::D_LEFT
                        || direction == Direction::D_RIGHT
                    {
                        self.t.cons[output].rect.width
                    } else {
                        self.t.cons[output].rect.height
                    } as f64;
                    (dim * (amount as f64 / 100.0)) as i64
                } else {
                    amount
                };
                let mut newrect = newrect;
                match direction {
                    Direction::D_LEFT => newrect.x = newrect.x.wrapping_sub(offset as u32),
                    Direction::D_RIGHT => newrect.x = newrect.x.wrapping_add(offset as u32),
                    Direction::D_UP => newrect.y = newrect.y.wrapping_sub(offset as u32),
                    Direction::D_DOWN => newrect.y = newrect.y.wrapping_add(offset as u32),
                }

                let parent = self.t.cons[current.con]
                    .parent
                    .expect("cmd_move_direction: floating con has no parent");
                ctx.subcommand_output.needs_tree_render =
                    floating_reposition(self, parent, newrect);
            } else {
                tree_move(self.t, self.b, current.con, direction);
                ctx.subcommand_output.needs_tree_render = true;
            }
        }

        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1599 cmd_layout
    fn cmd_layout(&mut self, ctx: &mut Ctx, layout_mode: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_layout", line!(), "handling command cmd_layout\n");
        let layout_str = layout_mode.unwrap_or("");
        if !self.handle_empty_match(ctx) {
            return;
        }

        let layout = match layout_from_name(layout_str) {
            None => {
                self.yerror(format!(
                    "Unknown layout \"{}\", this is a mismatch between code and parser spec.",
                    layout_str
                ));
                return;
            }
            Some(l) => l,
        };

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if con_is_docked(self.t, current.con) {
                continue;
            }

            crate::con::con_set_layout(self.t, self.b, current.con, layout);
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1630 cmd_layout_toggle
    fn cmd_layout_toggle(&mut self, ctx: &mut Ctx, toggle_mode: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_layout_toggle", line!(), "handling command cmd_layout_toggle\n");
        let toggle_mode = toggle_mode.unwrap_or("default");

        /* check if the match is empty, not if the result is empty */
        if match_is_empty(&ctx.current_match) {
            let focused = self.t.focused.expect("cmd_layout_toggle: no focused con");
            con_toggle_layout(self.t, self.b, focused, toggle_mode);
        } else {
            let owindows = owindows_snapshot(ctx);
            for current in &owindows {
                con_toggle_layout(self.t, self.b, current.con, toggle_mode);
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1657 cmd_exit
    fn cmd_exit(&mut self, _ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_exit", line!(), "handling command cmd_exit\n");
        // C: exit(EXIT_SUCCESS). The core layer cannot terminate the
        // process; the caller (event loop) observes a successful reply and
        // is responsible for exiting.
        crate::reload::request_exit();
        self.ysuccess(true);
    }

    /// C: src/commands.c:1668 cmd_reload
    ///
    /// C runs the whole reload inline (kill_nagbar, load_configuration(NULL,
    /// C_RELOAD) with its translate_keysyms()/grab_all_keys()/
    /// regrab_all_buttons()/gaps_reapply_workspace_assignments()/tree_render()
    /// tail, x_set_i3_atoms(), the workspace and barconfig_update IPC events)
    /// because every part of it is a process global. The command layer here
    /// owns the Tree and the backend only, so — like cmd_restart — it records
    /// the request and the event loop performs those steps in the same
    /// iteration (see crate::reload::reload).
    fn cmd_reload(&mut self, _ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_reload", line!(), "handling command cmd_reload\n");
        crate::reload::request_reload();
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1698 cmd_restart
    fn cmd_restart(&mut self, _ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_restart", line!(), "handling command cmd_restart\n");
        crate::log::log_("restarting i3\n");

        /* C: exempt_fd = cmd_output->client->fd; fcntl(fd, F_SETFD, flags &
         * ~FD_CLOEXEC); setenv("_I3_RESTART_FD", fdstr, 1).
         *
         * The IPC shutdown and the exec() happen at the end of the current
         * event loop iteration (crates/bins/i3/src/server.rs): unlike C, the
         * command layer does not own the IPC server, the X connection or the
         * argument vector. Everything up to that point is identical. */
        crate::restore_layout::cmd_restart_prepare(self.client);

        // C never returns from this handler (i3_restart() execs), so no reply
        // is sent; the success reply would be pointless here as well.
        self.ysuccess(true);
    }

    /// C: src/commands.c:1730 cmd_open
    fn cmd_open(&mut self, ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_open", line!(), "handling command cmd_open\n");
        let con = tree_open_con(self.t, self.b, None, None);
        self.t.cons[con].layout = Layout::L_SPLITH;
        con_activate(self.t, self.b, con);

        let con_ffi = con.data().as_ffi() as i64;
        if let Some(gen) = self.gen.as_mut() {
            gen.map_open();
            gen.string("success");
            gen.boolean(true);
            gen.string("id");
            gen.integer(con_ffi);
            gen.map_close();
        }

        ctx.subcommand_output.needs_tree_render = true;
    }

    /// C: src/commands.c:1750 cmd_focus_output
    fn cmd_focus_output(&mut self, ctx: &mut Ctx, output: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_focus_output", line!(), "handling command cmd_focus_output\n");
        if let Some(name) = output {
            user_output_names_add(self.t, &mut self.focus_output_names, name);
            return;
        }

        if self.focus_output_names.is_empty() {
            self.yerror("At least one output must be specified".to_string());
            return;
        }

        if !self.handle_empty_match(ctx) {
            return;
        }

        if ctx.owindows.is_empty() {
            self.ysuccess(true);
            return;
        }

        /* Command criteria need to work for focus output left|right|up|down.
         * We need to avoid using internal workspaces with get_output_for_con,
         * so we go through all matched windows until we find a non-internal
         * one. If there is no match, fall back to the focused one. */
        let mut con = self.t.focused.expect("cmd_focus_output: no focused con");
        for current in &ctx.owindows {
            if let Some(ws) = con_get_workspace(self.t, current.con) {
                if !con_is_internal(self.t, ws) {
                    con = current.con;
                    break;
                }
            }
        }

        let current_output = get_output_for_con(self.t, con);
        let names: Vec<String> = self.focus_output_names.iter().cloned().collect();
        let target_output = user_output_names_find_next(self, &names, current_output);
        self.focus_output_names.clear();
        let mut success = false;
        if let Some(target_output) = target_output {
            success = true;

            /* get visible workspace on output */
            let output_con = self.t.outputs[target_output]
                .con
                .expect("cmd_focus_output: output has no con");
            let content = output_get_content(self.t, output_con)
                .expect("cmd_focus_output: no content");
            let mut ws: Option<ConId> = None;
            let children: Vec<ConId> = self.t.cons[content].nodes_head.iter().copied().collect();
            for child in children {
                if workspace_is_visible(self.t, child) {
                    ws = Some(child);
                    break;
                }
            }
            let ws = match ws {
                None => {
                    self.yerror("BUG: No workspace found on output.".to_string());
                    return;
                }
                Some(ws) => ws,
            };

            workspace_show(self.t, self.b, ws);
        }

        ctx.subcommand_output.needs_tree_render = success;
        if success {
            self.ysuccess(true);
        } else {
            self.yerror("No output matched".to_string());
        }
    }

    /// C: src/commands.c:1812 cmd_move_window_to_position
    fn cmd_move_window_to_position(&mut self, ctx: &mut Ctx, coord_x: i64, mode_x: Option<&str>, coord_y: i64, mode_y: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_move_window_to_position", line!(), "handling command cmd_move_window_to_position\n");
        let mut has_error = false;

        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if !con_is_floating(self.t, current.con) {
                if !has_error {
                    self.yerror(
                        "Cannot change position of a window/container because it is not floating."
                            .to_string(),
                    );
                    has_error = true;
                }

                continue;
            }

            let parent = self.t.cons[current.con]
                .parent
                .expect("cmd_move_window_to_position: floating con has no parent");
            let mut newrect = self.t.cons[parent].rect;
            let output = con_get_output(self.t, current.con)
                .expect("cmd_move_window_to_position: no output");

            newrect.x = if mode_x == Some("ppt") {
                (self.t.cons[output].rect.width as f64 * (coord_x as f64 / 100.0)) as u32
            } else {
                coord_x as u32
            };
            newrect.y = if mode_y == Some("ppt") {
                (self.t.cons[output].rect.height as f64 * (coord_y as f64 / 100.0)) as u32
            } else {
                coord_y as u32
            };

            if !floating_reposition(self, parent, newrect) {
                self.yerror("Cannot move window/container out of bounds.".to_string());
                has_error = true;
            }
        }

        if !has_error {
            self.ysuccess(true);
        }
    }

    /// C: src/commands.c:1852 cmd_move_window_to_center
    fn cmd_move_window_to_center(&mut self, ctx: &mut Ctx, method: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_move_window_to_center", line!(), "handling command cmd_move_window_to_center\n");
        let method = method.unwrap_or("");
        let mut has_error = false;
        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            let floating_con = match con_inside_floating(self.t, current.con) {
                None => {
                    if !has_error {
                        self.yerror(
                            "Cannot change position of a window/container because it is not floating."
                                .to_string(),
                        );
                        has_error = true;
                    }

                    continue;
                }
                Some(f) => f,
            };

            if method == "absolute" {
                let croot = self.t.croot.expect("cmd_move_window_to_center: no croot");
                crate::floating::floating_center(self.t, floating_con, self.t.cons[croot].rect);

                floating_maybe_reassign_ws(self, floating_con);
                ctx.subcommand_output.needs_tree_render = true;
            }

            if method == "position" {
                let ws = con_get_workspace(self.t, floating_con)
                    .expect("cmd_move_window_to_center: no workspace");
                crate::floating::floating_center(self.t, floating_con, self.t.cons[ws].rect);

                ctx.subcommand_output.needs_tree_render = true;
            }
        }

        // XXX: default reply for now, make this a better reply
        if !has_error {
            self.ysuccess(true);
        }
    }

    /// C: src/commands.c:1897 cmd_move_window_to_mouse
    fn cmd_move_window_to_mouse(&mut self, ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_move_window_to_mouse", line!(), "handling command cmd_move_window_to_mouse\n");
        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            let floating_con = match con_inside_floating(self.t, current.con) {
                None => continue,
                Some(f) => f,
            };

            // C: src/floating.c:537 floating_move_to_pointer — the C code reads
            // the pointer through the global conn; the command layer has no
            // XConn, so the query goes through ConBackend::x_query_pointer_root
            // and the reposition through floating_move_to_position.
            match self.b.x_query_pointer_root() {
                None => crate::log::errorlog(
                    "could not query pointer position, not moving this container\n",
                ),
                Some((_child, root_x, root_y)) => {
                    crate::floating::floating_move_to_position(
                        self.t,
                        self.b,
                        floating_con,
                        root_x,
                        root_y,
                    );
                }
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(true);
    }

    /// C: src/commands.c:1921 cmd_move_scratchpad
    fn cmd_move_scratchpad(&mut self, ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_move_scratchpad", line!(), "handling command cmd_move_scratchpad\n");
        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            scratchpad_move(self.t, self.b, current.con);
        }

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(true);
    }

    /// C: src/commands.c:1941 cmd_scratchpad_show
    fn cmd_scratchpad_show(&mut self, ctx: &mut Ctx) {
        crate::log::dlog_at(file!(), "cmd_scratchpad_show", line!(), "handling command cmd_scratchpad_show\n");
        let mut result = false;

        if match_is_empty(&ctx.current_match) {
            result = scratchpad_show(self.t, self.b, None);
        } else {
            let owindows = owindows_snapshot(ctx);
            for current in &owindows {
                result |= scratchpad_show(self.t, self.b, Some(current.con));
            }
        }

        ctx.subcommand_output.needs_tree_render = true;

        self.ysuccess(result);
    }

    /// C: src/commands.c:1964 cmd_swap
    fn cmd_swap(&mut self, ctx: &mut Ctx, mode: Option<&str>, arg: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_swap", line!(), "handling command cmd_swap\n");
        let mode = mode.unwrap_or("");
        let arg = arg.unwrap_or("");
        if !self.handle_empty_match(ctx) {
            return;
        }

        if ctx.owindows.is_empty() {
            self.yerror("No match found for swapping.".to_string());
            return;
        }

        let con = if mode == "id" {
            let target = match parse_long(arg, 0) {
                None => {
                    self.yerror(format!("Failed to parse {} into a window id.", arg));
                    return;
                }
                Some(t) => t,
            };

            crate::con::con_by_window_id(self.t, target as u32)
        } else if mode == "con_id" {
            let target = match parse_long(arg, 0) {
                None => {
                    self.yerror(format!("Failed to parse {} into a container id.", arg));
                    return;
                }
                Some(t) => t,
            };

            let target = lookup_con_by_ffi(target as u64);
            crate::con::con_by_con_id(self.t, target)
        } else if mode == "mark" {
            crate::con::con_by_mark(self.t, arg)
        } else {
            self.yerror(format!("Unhandled swap mode \"{}\". This is a bug.", mode));
            return;
        };

        let con = match con {
            None => {
                self.yerror(format!("Could not find container for {} = {}", mode, arg));
                return;
            }
            Some(con) => con,
        };

        if ctx.owindows.len() > 1 {
            // C: LOG("More than one container matched the swap command, only
            //         using the first one.");
        }

        let result = crate::con::con_swap(self.t, self.b, ctx.owindows[0].con, con);

        ctx.subcommand_output.needs_tree_render = true;
        // XXX: default reply for now, make this a better reply
        self.ysuccess(result);
    }

    /// C: src/commands.c:2022 cmd_title_format
    fn cmd_title_format(&mut self, ctx: &mut Ctx, format: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_title_format", line!(), "handling command cmd_title_format\n");
        let format = format.unwrap_or("");
        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            /* If we only display the title without anything else, we can skip
             * the parsing step, so we remove the title format altogether. */
            if format.eq_ignore_ascii_case("%title") {
                self.t.cons[current.con].title_format = None;

                if let Some(window) = self.t.cons[current.con].window {
                    /* We can remove _NET_WM_VISIBLE_NAME since we don't
                     * display a custom title. */
                    // C: ewmh_update_visible_name(window->id, NULL);
                    let window_id = self.t.windows[window].id;
                    self.b.ewmh_update_visible_name(window_id, None);
                    self.t.windows[window].name_x_changed = true;
                }
            } else {
                self.t.cons[current.con].title_format = Some(format.to_string());

                if let Some(window) = self.t.cons[current.con].window {
                    // C: ewmh_update_visible_name(window->id,
                    //    i3string_as_utf8(con_parse_title_format(con)));
                    let formatted = crate::con::con_parse_title_format(self.t, current.con);
                    let window_id = self.t.windows[window].id;
                    self.b.ewmh_update_visible_name(window_id, Some(&formatted));
                    self.t.windows[window].name_x_changed = true;
                }
            }

            if self.t.cons[current.con].window.is_none() {
                /* For windowless containers we also need to force the
                 * redrawing. */
                self.t.cons[current.con].deco_render_params = None;
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(true);
    }

    /// C: src/commands.c:2065 cmd_title_window_icon
    fn cmd_title_window_icon(&mut self, ctx: &mut Ctx, enable: Option<&str>, padding: i64) {
        crate::log::dlog_at(file!(), "cmd_title_window_icon", line!(), "handling command cmd_title_window_icon\n");
        let mut padding = padding;
        let mut is_toggle = false;
        if let Some(enable) = enable {
            if enable == "toggle" {
                is_toggle = true;
            } else if !boolstr(enable) {
                padding = -1;
            }
        }

        if !self.handle_empty_match(ctx) {
            return;
        }

        let owindows = owindows_snapshot(ctx);
        for current in &owindows {
            if is_toggle {
                let current_padding = self.t.cons[current.con].window_icon_padding;
                if padding > 0 {
                    if current_padding < 0 {
                        self.t.cons[current.con].window_icon_padding = padding as i32;
                    } else {
                        /* toggle off, but store padding given */
                        self.t.cons[current.con].window_icon_padding =
                            -(padding + 1) as i32;
                    }
                } else {
                    /* Set to negative of (current value+1) to keep old
                     * padding when toggling */
                    self.t.cons[current.con].window_icon_padding =
                        -(current_padding as i64 + 1) as i32;
                }
            } else {
                self.t.cons[current.con].window_icon_padding = padding as i32;
            }

            if let Some(window) = self.t.cons[current.con].window {
                /* Make sure the window title is redrawn immediately. */
                self.t.windows[window].name_x_changed = true;
            } else {
                /* For windowless containers we also need to force the
                 * redrawing. */
                self.t.cons[current.con].deco_render_params = None;
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(true);
    }

    /// C: src/commands.c:2114 cmd_rename_workspace
    fn cmd_rename_workspace(&mut self, ctx: &mut Ctx, old_name: Option<&str>, new_name: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_rename_workspace", line!(), "handling command cmd_rename_workspace\n");
        let new_name = new_name.unwrap_or("");
        if new_name.len() >= 2 && new_name.as_bytes()[..2].eq_ignore_ascii_case(b"__") {
            self.yerror(format!(
                "Cannot rename workspace to \"{}\": names starting with __ are i3-internal.",
                new_name
            ));
            return;
        }

        let workspace;
        let old_name_owned;
        match old_name {
            Some(old) => {
                workspace = get_existing_workspace_by_name(self.t, old);
                old_name_owned = old.to_string();
            }
            None => {
                let focused = self.t.focused.expect("cmd_rename_workspace: no focused con");
                workspace = con_get_workspace(self.t, focused);
                old_name_owned = workspace
                    .and_then(|w| self.t.cons[w].name.clone())
                    .unwrap_or_default();
            }
        }

        let workspace = match workspace {
            None => {
                self.yerror(format!("Old workspace \"{}\" not found", old_name_owned));
                return;
            }
            Some(w) => w,
        };

        let check_dest = get_existing_workspace_by_name(self.t, new_name);

        /* If check_dest == workspace, the user might be changing the case of
         * the workspace, or it might just be a no-op. */
        if check_dest.is_some() && check_dest != Some(workspace) {
            self.yerror(format!("New workspace \"{}\" already exists", new_name));
            return;
        }

        /* Change the name and try to parse it as a number. */
        self.t.cons[workspace].name = Some(new_name.to_string());

        self.t.cons[workspace].num = ws_name_to_number(new_name);

        /* By re-attaching, the sort order will be correct afterwards. */
        let previously_focused = self.t.focused;
        let previously_focused_content = previously_focused.and_then(|f| {
            if self.t.cons[f].r#type == ConType::CT_WORKSPACE {
                self.t.cons[f].parent
            } else {
                None
            }
        });
        let parent = self.t.cons[workspace]
            .parent
            .expect("cmd_rename_workspace: no parent");
        con_detach(self.t, workspace);
        con_attach(self.t, self.b, workspace, parent, false);
        self.b
            .ipc_send_workspace_event(self.t, "rename", workspace, None);

        let assigned =
            get_assigned_output(self.t, Some(new_name), self.t.cons[workspace].num as i64);
        if let Some(assigned) = assigned {
            let output = get_output_for_con(self.t, assigned)
                .expect("cmd_rename_workspace: assigned output not found");
            workspace_move_to_output(self.t, self.b, workspace, output);
        }

        let mut can_restore_focus = previously_focused.is_some();
        if let Some(previously_focused_content) = previously_focused_content {
            let ws = self.t.cons[previously_focused_content]
                .nodes_head
                .iter()
                .copied()
                .find(|&child| Some(child) == previously_focused);
            can_restore_focus &= ws.is_some();
        }

        if can_restore_focus {
            /* Restore the previous focus since con_attach messes with the
             * focus. */
            let pf = previously_focused.unwrap();
            let pf_ws = con_get_workspace(self.t, pf)
                .expect("cmd_rename_workspace: no ws of previous focus");
            workspace_show(self.t, self.b, pf_ws);
            con_focus(self.t, self.b, pf);
        }

        /* Let back-and-forth work after renaming the previous workspace.
         * See #3694. */
        if let Some(previous) = &self.t.previous_workspace_name {
            if *previous == old_name_owned {
                self.t.previous_workspace_name = Some(new_name.to_string());
            }
        }

        ctx.subcommand_output.needs_tree_render = true;
        self.ysuccess(true);

        self.b.ewmh_update_desktop_properties(self.t);

        // C: startup_sequence_rename_workspace(old_name_copy, new_name);
        // TODO(startup): needs the startup-notification layer.
    }

    /// C: src/commands.c:2207 cmd_bar_mode
    fn cmd_bar_mode(&mut self, _ctx: &mut Ctx, bar_value: Option<&str>, bar_id: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_bar_mode", line!(), "handling command cmd_bar_mode\n");
        let bar_mode = bar_value.unwrap_or("");
        let mut mode = crate::config::bar_mode_t::M_DOCK as i32;
        let mut toggle = false;
        match bar_mode {
            "dock" => mode = crate::config::bar_mode_t::M_DOCK as i32,
            "hide" => mode = crate::config::bar_mode_t::M_HIDE as i32,
            "invisible" => mode = crate::config::bar_mode_t::M_INVISIBLE as i32,
            "toggle" => toggle = true,
            _ => {
                // C: ELOG("Unknown bar mode ..."); assert(false);
                crate::log::errorlog(&format!(
                    "ERROR: Unknown bar mode \"{}\", this is a mismatch between code and parser spec.\n",
                    bar_mode
                ));
                panic!("Unknown bar mode \"{}\"", bar_mode);
            }
        }

        // C: if (TAILQ_EMPTY(&barconfigs)) { yerror("No bars found\n"); return; }
        // The list is the global the config layer filled (Tree::barconfigs).
        if self.t.barconfigs.is_empty() {
            self.yerror("No bars found\n".to_string());
            return;
        }

        let count = self.t.barconfigs.len();
        for idx in 0..count {
            if let Some(id) = bar_id {
                if self.t.barconfigs[idx].id.as_deref() != Some(id) {
                    continue;
                }
            }

            if toggle {
                // C: mode = (current->mode + 1) % 2;
                mode = (self.t.barconfigs[idx].mode as i32 + 1) % 2;
            }

            // C: if ((int)current->mode != mode) { current->mode = mode;
            //        ipc_send_barconfig_update_event(current); }
            if self.t.barconfigs[idx].mode as i32 != mode {
                self.t.barconfigs[idx].mode = bar_mode_from_i32(mode);
                let current = self.t.barconfigs[idx].clone();
                self.b.ipc_send_barconfig_update_event(self.t, &current);
            }

            if bar_id.is_some() {
                /* We are looking for a specific bar and we found it */
                self.ysuccess(true);
                return;
            }
        }

        if let Some(id) = bar_id {
            /* We are looking for a specific bar and we did not find it */
            self.yerror(format!(
                "Changing bar mode of bar_id {} failed, bar_id not found.\n",
                id
            ));
        } else {
            /* We have already handled the case of no bars, so we must have
             * updated all active bars now. */
            self.ysuccess(true);
        }
    }

    /// C: src/commands.c:2266 cmd_bar_hidden_state
    fn cmd_bar_hidden_state(&mut self, _ctx: &mut Ctx, bar_value: Option<&str>, bar_id: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_bar_hidden_state", line!(), "handling command cmd_bar_hidden_state\n");
        let bar_hidden_state = bar_value.unwrap_or("");
        let mut hidden_state = crate::config::bar_hidden_state_t::S_SHOW as i32;
        let mut toggle = false;
        match bar_hidden_state {
            "hide" => hidden_state = crate::config::bar_hidden_state_t::S_HIDE as i32,
            "show" => hidden_state = crate::config::bar_hidden_state_t::S_SHOW as i32,
            "toggle" => toggle = true,
            _ => {
                // C: ELOG("Unknown bar state ..."); assert(false);
                crate::log::errorlog(&format!(
                    "ERROR: Unknown bar state \"{}\", this is a mismatch between code and parser spec.\n",
                    bar_hidden_state
                ));
                panic!("Unknown bar state \"{}\"", bar_hidden_state);
            }
        }

        // C: if (TAILQ_EMPTY(&barconfigs)) { yerror("No bars found\n"); return; }
        if self.t.barconfigs.is_empty() {
            self.yerror("No bars found\n".to_string());
            return;
        }

        let count = self.t.barconfigs.len();
        for idx in 0..count {
            if let Some(id) = bar_id {
                if self.t.barconfigs[idx].id.as_deref() != Some(id) {
                    continue;
                }
            }

            if toggle {
                // C: hidden_state = (current->hidden_state + 1) % 2;
                hidden_state = (self.t.barconfigs[idx].hidden_state as i32 + 1) % 2;
            }

            // C: if ((int)current->hidden_state != hidden_state) {
            //        current->hidden_state = hidden_state;
            //        ipc_send_barconfig_update_event(current); }
            if self.t.barconfigs[idx].hidden_state as i32 != hidden_state {
                self.t.barconfigs[idx].hidden_state = bar_hidden_state_from_i32(hidden_state);
                let current = self.t.barconfigs[idx].clone();
                self.b.ipc_send_barconfig_update_event(self.t, &current);
            }

            if bar_id.is_some() {
                self.ysuccess(true);
                return;
            }
        }

        if let Some(id) = bar_id {
            self.yerror(format!(
                "Changing bar hidden_state of bar_id {} failed, bar_id not found.\n",
                id
            ));
        } else {
            self.ysuccess(true);
        }
    }

    /// C: src/commands.c:2323 cmd_shmlog
    fn cmd_shmlog(&mut self, _ctx: &mut Ctx, argument: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_shmlog", line!(), "handling command cmd_shmlog\n");
        // (C: src/commands.c:2323 compares the parser's string argument; the
        // spec parser hands back the remainder of the line, so trailing
        // whitespace is stripped here for the comparisons below.)
        let argument = argument.unwrap_or("").trim();

        if argument == "toggle" {
            /* Toggle shm log, if size is not 0. If it is 0, set it to default. */
            let size = crate::log::shmlog_size_signed();
            crate::log::set_shmlog_size_signed(if size != 0 {
                size.wrapping_neg()
            } else {
                crate::log::default_shmlog_size() as i64
            });
        } else if argument == "on" {
            crate::log::set_shmlog_size(crate::log::default_shmlog_size());
        } else if argument == "off" {
            crate::log::set_shmlog_size_signed(0);
        } else {
            let new_size = match parse_long(argument, 0) {
                Some(size) => size,
                None => {
                    self.yerror(format!("Failed to parse {} into a shmlog size.", argument));
                    return;
                }
            };
            /* If shm logging now, restart logging with the new size. */
            if crate::log::shmlog_size_signed() > 0 {
                crate::log::set_shmlog_size_signed(0);
                crate::log::log_("Restarting shm logging...\n");
                crate::log::init_logging();
            }
            crate::log::set_shmlog_size_signed(new_size);
        }
        crate::log::log_(&format!(
            "{} shm logging\n",
            if crate::log::shmlog_size_signed() > 0 {
                "Enabling"
            } else {
                "Disabling"
            }
        ));
        crate::log::init_logging();
        // C: src/commands.c:2347 update_shmlog_atom() — the command layer has
        // no X connection, so the atom export is requested from the loop.
        crate::log::request_shmlog_atom_update();
        self.ysuccess(true);
    }

    /// C: src/commands.c:2355 cmd_debuglog
    fn cmd_debuglog(&mut self, _ctx: &mut Ctx, argument: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_debuglog", line!(), "handling command cmd_debuglog\n");
        let argument = argument.unwrap_or("");
        let logging = crate::log::get_debug_logging();
        if argument == "toggle" {
            crate::log::log_(&format!(
                "{} debug logging\n",
                if logging { "Disabling" } else { "Enabling" }
            ));
            crate::log::set_debug_logging(!logging);
        } else if argument == "on" && !logging {
            crate::log::log_("Enabling debug logging\n");
            crate::log::set_debug_logging(true);
        } else if argument == "off" && logging {
            crate::log::log_("Disabling debug logging\n");
            crate::log::set_debug_logging(false);
        }
        self.ysuccess(true);
    }

    /// C: src/commands.c:2477 cmd_gaps
    fn cmd_gaps(&mut self, ctx: &mut Ctx, r#type: Option<&str>, scope: Option<&str>, mode: Option<&str>, value: Option<&str>) {
        crate::log::dlog_at(file!(), "cmd_gaps", line!(), "handling command cmd_gaps\n");
        let type_ = r#type.unwrap_or("");
        let scope = scope.unwrap_or("");
        let mode = mode.unwrap_or("");
        let value = value.unwrap_or("");
        // C: const int pixels = logical_px(atoi(value)); — the parser's
        // value token is a *word*, so "gaps inner 30px" arrives as "30px"
        // (commands.spec:137 GAPS_WITH_MODE value = word, GAPS_END 'px').
        // atoi() only reads the leading number, unlike Rust's parse().
        let pixels = logical_px(atoi(value));

        let mut ok = true;
        if type_ == "inner" {
            ok &= gaps_update(self, GapField::Inner, scope, mode, pixels);
            /* Update all workspaces with a no-op change (plus 0) so that the
             * minimum value is re-calculated and applied as a side effect. */
            ok &= gaps_update(self, GapField::Top, "all", "plus", 0);
            ok &= gaps_update(self, GapField::Bottom, "all", "plus", 0);
            ok &= gaps_update(self, GapField::Right, "all", "plus", 0);
            ok &= gaps_update(self, GapField::Left, "all", "plus", 0);
        } else if type_ == "outer" {
            ok &= gaps_update(self, GapField::Top, scope, mode, pixels);
            ok &= gaps_update(self, GapField::Bottom, scope, mode, pixels);
            ok &= gaps_update(self, GapField::Right, scope, mode, pixels);
            ok &= gaps_update(self, GapField::Left, scope, mode, pixels);
        } else if type_ == "vertical" {
            ok &= gaps_update(self, GapField::Top, scope, mode, pixels);
            ok &= gaps_update(self, GapField::Bottom, scope, mode, pixels);
        } else if type_ == "horizontal" {
            ok &= gaps_update(self, GapField::Right, scope, mode, pixels);
            ok &= gaps_update(self, GapField::Left, scope, mode, pixels);
        } else if type_ == "top" {
            ok &= gaps_update(self, GapField::Top, scope, mode, pixels);
        } else if type_ == "bottom" {
            ok &= gaps_update(self, GapField::Bottom, scope, mode, pixels);
        } else if type_ == "right" {
            ok &= gaps_update(self, GapField::Right, scope, mode, pixels);
        } else if type_ == "left" {
            ok &= gaps_update(self, GapField::Left, scope, mode, pixels);
        } else {
            ok = false;
        }

        if ok {
            ctx.subcommand_output.needs_tree_render = true;
            // XXX: default reply for now, make this a better reply
            self.ysuccess(true);
        } else {
            self.ysuccess(false);
        }
    }
}

/// C: src/assignments.c run_assignments is called with the con's window
/// after a floating mode change (commands.c:1162).
fn run_assignments_for(runner: &mut CommandRunner, window: Option<WindowId>) {
    crate::assignments::run_assignments(runner.t, runner.b, window);
}

/// C: src/commands.c:2393 gaps_update (the gap_accessor function pointers
/// become a GapField).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GapField {
    Inner,
    Top,
    Right,
    Bottom,
    Left,
}

impl GapField {
    fn get(self, g: &gaps_t) -> i32 {
        match self {
            GapField::Inner => g.inner,
            GapField::Top => g.top,
            GapField::Right => g.right,
            GapField::Bottom => g.bottom,
            GapField::Left => g.left,
        }
    }

    fn set(self, g: &mut gaps_t, value: i32) {
        match self {
            GapField::Inner => g.inner = value,
            GapField::Top => g.top = value,
            GapField::Right => g.right = value,
            GapField::Bottom => g.bottom = value,
            GapField::Left => g.left = value,
        }
    }

    /// C: get(&(config.gaps)) != gaps_inner(&(config.gaps))
    fn is_outer(self) -> bool {
        self != GapField::Inner
    }
}

fn gaps_update(runner: &mut CommandRunner, field: GapField, scope: &str, mode: &str, pixels: i32) -> bool {
    let focused = runner.t.focused.expect("gaps_update: no focused con");
    let workspace = con_get_workspace(runner.t, focused)
        .expect("gaps_update: focused has no workspace");

    let global_gap_size = field.get(&runner.t.config.gaps);
    let mut current_value = global_gap_size;
    if scope == "current" {
        current_value += field.get(&runner.t.cons[workspace].gaps);
    }

    let mut reset = false;
    if mode == "plus" {
        current_value += pixels;
    } else if mode == "minus" {
        current_value -= pixels;
    } else if mode == "set" {
        current_value = pixels;
        reset = true;
    } else if mode == "toggle" {
        current_value = (!current_value as i32) * pixels;
        reset = true;
    } else {
        // C: ELOG("Invalid mode %s when changing gaps", mode);
        return false;
    }

    /* See https://github.com/Airblader/i3/issues/262 */
    let mut min_value = 0;
    let is_outer = field.is_outer();
    if is_outer {
        /* Outer gaps can compensate inner gaps. */
        if scope == "all" {
            min_value = -runner.t.config.gaps.inner;
        } else {
            min_value = -runner.t.config.gaps.inner - runner.t.cons[workspace].gaps.inner;
        }
    }

    if current_value < min_value {
        current_value = min_value;
    }

    if scope == "all" {
        let croot = runner.t.croot.expect("gaps_update: no croot");
        let outputs: Vec<ConId> = runner.t.cons[croot].nodes_head.iter().copied().collect();
        for output in outputs {
            let content = match output_get_content(runner.t, output) {
                None => continue,
                Some(c) => c,
            };
            let workspaces: Vec<ConId> =
                runner.t.cons[content].nodes_head.iter().copied().collect();
            for cur_ws in workspaces {
                if reset {
                    field.set(&mut runner.t.cons[cur_ws].gaps, 0);
                } else {
                    let mut max_compensate = 0;
                    if is_outer {
                        max_compensate = runner.t.config.gaps.inner;
                    }
                    let gaps_value = field.get(&runner.t.cons[cur_ws].gaps);
                    if gaps_value + current_value + max_compensate < 0 {
                        /* Enforce new per-workspace gap size minimum value
                         * (in case current_value is smaller than before):
                         * the workspace can at most have a negative gap size
                         * of -current_value - max_compensate. */
                        field.set(
                            &mut runner.t.cons[cur_ws].gaps,
                            -current_value - max_compensate,
                        );
                    }
                }
            }
        }

        field.set(&mut runner.t.config.gaps, current_value);
    } else {
        let value = current_value - global_gap_size;
        field.set(&mut runner.t.cons[workspace].gaps, value);
    }

    true
}

impl<'t, 'b> CommandRunner<'t, 'b> {
    /// Runs a single command string through the parser (C: parse_command in
    /// src/commands_parser.c). The json reply (if any) is accumulated in
    /// gen; needs_tree_render comes back in the CommandResult.
    pub fn run(&mut self, input: &str) -> CommandResult {
        let result = parse_command(input, self);
        if result.needs_tree_render {
            // C: the caller of parse_command calls tree_render() right after
            // (src/ipc.c:218 IPC_HANDLER(run_command), src/bindings.c:879). The
            // request also covers the callers which ignore the flag
            // (src/assignments.c:79 in C); tree_render() clears it again.
            crate::tree::request_tree_render();
        }
        result
    }

    /// Takes the reply generator back (used by the IPC server after
    /// parse_command returned; C reads the buffer through
    /// yajl_gen_get_buf(gen, …)).
    pub fn take_gen(&mut self) -> Option<JsonGen> {
        self.gen.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::data::Window;
    use crate::tree::tree_init;
    use crate::workspace::create_workspace_on_output;

    /// C: src/floating.c:22 total_outputs_dimensions — the sum of the output
    /// sizes, or the root screen dimensions when no output exists. The root
    /// screen is the X screen the WM runs on (libi3/dpi.rs keeps its values).
    #[test]
    fn total_outputs_dimensions_sums_outputs_and_falls_back_to_root_screen() {
        let (t, _output, _ws) = test_tree();
        assert_eq!(total_outputs_dimensions(&t), Rect::new(0, 0, 1280, 800));

        // No outputs at all: C returns root_screen->width/height_in_pixels.
        let mut t = Tree::new();
        let saved = libi3::dpi::root_screen();
        libi3::dpi::set_root_screen(None);
        assert_eq!(total_outputs_dimensions(&t), Rect::new(0, 0, 0, 0));
        libi3::dpi::set_root_screen(Some(libi3::dpi::RootScreen {
            root: 0,
            width_in_pixels: 1024,
            height_in_pixels: 768,
            height_in_millimeters: 203,
        }));
        assert_eq!(total_outputs_dimensions(&t), Rect::new(0, 0, 1024, 768));

        // Two outputs: the dimensions are summed (x/y stay untouched).
        let mut b = NullBackend;
        tree_init(&mut t, &mut b, Rect::new(0, 0, 1024, 768));
        for (i, (x, y, w, h)) in [(0u32, 0u32, 800u32, 600u32), (800, 0, 1024, 768)].iter().enumerate() {
            let mut output = crate::data::Output::default();
            output.active = true;
            output.rect = Rect::new(*x, *y, *w, *h);
            output.names_head.push_back(crate::data::output_name {
                name: format!("out-{}", i),
            });
            t.outputs.push_back(output);
        }
        assert_eq!(total_outputs_dimensions(&t), Rect::new(0, 0, 1824, 1368));

        libi3::dpi::set_root_screen(saved);
    }

    fn test_tree() -> (Tree, crate::ConId, crate::ConId) {
        let mut t = Tree::new();
        let mut b = NullBackend;
        tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));

        let croot = t.croot;
        /* C: src/randr.c:371 — a real output con gets layout L_OUTPUT and is
         * attached to croot exactly once (con_new(parent) attaches it). */
        let output = crate::con::con_new(&mut t, &mut b, croot, None);
        t.cons[output].r#type = ConType::CT_OUTPUT;
        t.cons[output].layout = Layout::L_OUTPUT;
        t.cons[output].name = Some("fake-0".to_string());
        t.cons[output].rect = Rect::new(0, 0, 1280, 800);

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

        /* C: src/randr.c — the content con is attached to the output once. */
        let content = crate::con::con_new(&mut t, &mut b, Some(output), None);
        t.cons[content].r#type = ConType::CT_CON;
        t.cons[content].layout = Layout::L_SPLITH;
        t.cons[content].rect = Rect::new(0, 0, 1280, 800);

        let ws = create_workspace_on_output(&mut t, &mut b, output_id, content);
        t.focused = Some(t.cons[ws].nodes_head.front().copied().unwrap_or(ws));
        (t, output, ws)
    }

    /// Adds a window (with the given class) on the currently focused
    /// workspace and focuses it — like manage_window in C.
    fn open_window(t: &mut Tree, class: &str, id: u32) -> ConId {
        let mut b = NullBackend;
        let window = t.windows.insert(Window {
            id,
            class_class: Some(class.to_string()),
            ..Default::default()
        });
        t.window_index.insert(id, window);
        let con = tree_open_con(t, &mut b, None, Some(window));
        t.cons[con].window = Some(window);
        con_focus(t, &mut b, con);
        con
    }

    fn run(t: &mut Tree, input: &str) -> CommandResult {
        let mut b = NullBackend;
        let mut runner = CommandRunner::new(t, &mut b, None, None);
        runner.run(input)
    }

    fn run_with(t: &mut Tree, b: &mut dyn crate::backend::ConBackend, input: &str) -> CommandResult {
        let mut runner = CommandRunner::new(t, b, None, None);
        runner.run(input)
    }

    /// Records the WM_DELETE request of x_window_kill (C: src/x.c:849) so the
    /// asynchronous kill protocol of tree_close_internal can be observed.
    #[derive(Default)]
    struct KillRecorder {
        killed: Vec<(u32, crate::data::KillWindow)>,
    }

    impl crate::backend::ConBackend for KillRecorder {
        fn x_window_kill(&mut self, _t: &Tree, window_id: u32, kill_window: crate::data::KillWindow) {
            self.killed.push((window_id, kill_window));
        }
    }

    /// C: 'split v' creates a new split container with the focused con
    /// (src/tree.c:327 tree_split + src/commands.c:1174 cmd_split).
    #[test]
    fn split_creates_parent() {
        let (mut t, _o, _ws) = test_tree();
        let _con1 = open_window(&mut t, "XTerm", 1);
        /* src/tree.c:357-363: with only one child the parent's orientation is
         * just switched, so two windows are needed to exercise the split. */
        let con = open_window(&mut t, "XTerm", 2);
        let parent = t.cons[con].parent.unwrap();

        let result = run(&mut t, "split vertical");
        assert!(result.needs_tree_render);

        let new_parent = t.cons[con].parent.unwrap();
        assert!(new_parent != parent);
        assert_eq!(t.cons[new_parent].layout, Layout::L_SPLITV);
        assert_eq!(t.cons[new_parent].nodes_head.len(), 1);
        // The focused con is now inside the new split container.
        assert!(t.cons[t.cons[new_parent].nodes_head[0]].window.is_some());
    }

    /// C: 'layout stacked' changes the layout (src/commands.c:1599
    /// cmd_layout + src/con.c:1997 con_set_layout).
    #[test]
    fn layout_stacked() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 1);
        let ws = con_get_workspace(&t, con).unwrap();

        let result = run(&mut t, "layout stacked");
        assert!(result.needs_tree_render);
        /* src/con.c:2027-2049: a workspace with children gets a new split
         * container which carries the requested layout; the workspace itself
         * keeps its split layout (and only records last_split_layout). */
        assert_eq!(t.cons[ws].last_split_layout, Layout::L_SPLITH);
        assert_eq!(t.cons[ws].nodes_head.len(), 1);
        let new_split = t.cons[ws].nodes_head[0];
        assert_ne!(new_split, con);
        assert_eq!(t.cons[new_split].layout, Layout::L_STACKED);
        assert_eq!(t.cons[new_split].nodes_head.len(), 1);
        assert_eq!(t.cons[new_split].nodes_head[0], con);
    }

    /// C: 'border pixel 3' sets the border style and width (commands.c:696).
    #[test]
    fn border_pixel() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 1);

        let result = run(&mut t, "border pixel 3");
        assert!(result.needs_tree_render);
        assert_eq!(t.cons[con].border_style, BorderStyle::BS_PIXEL);
        /* src/con.c:1959-1969 (con_set_border_style) stores the requested
         * width in current_border_width; ->border_width is the X11 attribute
         * set while rendering (src/x.c). */
        assert_eq!(t.cons[con].current_border_width, 3);
        assert_eq!(t.cons[con].max_user_border_style, BorderStyle::BS_PIXEL);
    }

    /// C: 'workspace 2' creates and shows workspace "2" (commands.c:908).
    #[test]
    fn workspace_switch_by_name() {
        let (mut t, _o, _ws) = test_tree();
        open_window(&mut t, "XTerm", 1);

        let result = run(&mut t, "workspace 2");
        assert!(result.needs_tree_render);

        let focused_ws = con_get_workspace(&t, t.focused.unwrap()).unwrap();
        assert_eq!(t.cons[focused_ws].name.as_deref(), Some("2"));
        assert_eq!(t.cons[focused_ws].num, 2);
    }

    /// C: 'workspace number 2' picks the workspace by number
    /// (commands.c:860).
    #[test]
    fn workspace_switch_by_number() {
        let (mut t, _o, _ws) = test_tree();
        open_window(&mut t, "XTerm", 1);
        run(&mut t, "workspace 3");

        // Switching to an existing workspace by number shows it.
        let result = run(&mut t, "workspace number 1");
        assert!(result.needs_tree_render);
        let focused_ws = con_get_workspace(&t, t.focused.unwrap()).unwrap();
        assert_eq!(t.cons[focused_ws].num, 1);

        // C: src/commands.c:879-882 — selecting the already focused workspace
        // is handled by maybe_back_and_forth (commands.c:83), which returns
        // true and therefore does not schedule a render.
        let result = run(&mut t, "workspace number 1");
        assert!(!result.needs_tree_render);
        let focused_ws = con_get_workspace(&t, t.focused.unwrap()).unwrap();
        assert_eq!(t.cons[focused_ws].num, 1);

        // C: an unparsable number errors without switching.
        let result = run(&mut t, "workspace number x");
        assert!(!result.needs_tree_render);
        let focused_ws = con_get_workspace(&t, t.focused.unwrap()).unwrap();
        assert_eq!(t.cons[focused_ws].num, 1);
    }

    /// C: 'workspace back_and_forth' returns to the previous workspace
    /// (commands.c:894).
    #[test]
    fn workspace_back_and_forth() {
        let (mut t, _o, _ws) = test_tree();
        open_window(&mut t, "XTerm", 1);
        run(&mut t, "workspace 2");
        run(&mut t, "workspace 1");
        run(&mut t, "workspace back_and_forth");

        let focused_ws = con_get_workspace(&t, t.focused.unwrap()).unwrap();
        assert_eq!(t.cons[focused_ws].name.as_deref(), Some("2"));
    }

    /// C: criteria — '[class="XTerm"] focus' focuses the matching window
    /// (commands.c:143 cmd_criteria_match_windows + 1433 cmd_focus).
    #[test]
    fn criteria_focus_by_class() {
        let (mut t, _o, _ws) = test_tree();
        let con1 = open_window(&mut t, "XTerm", 1);
        let con2 = open_window(&mut t, "Chromium", 2);
        // con2 is focused now.
        assert_eq!(t.focused, Some(con2));

        let result = run(&mut t, r#"[class="XTerm"] focus"#);
        assert!(result.needs_tree_render);
        assert_eq!(t.focused, Some(con1));
    }

    /// C: '[con_mark="m"] kill' closes only the marked window.
    #[test]
    fn criteria_kill_by_mark() {
        let (mut t, _o, _ws) = test_tree();
        let con1 = open_window(&mut t, "XTerm", 1);
        let con2 = open_window(&mut t, "XTerm", 2);

        run(&mut t, "mark --replace m");
        assert_eq!(t.cons[t.focused.unwrap()].marks_head.len(), 1);

        let mut b = KillRecorder::default();
        let result = run_with(&mut t, &mut b, r#"[con_mark="m"] kill"#);
        assert!(result.needs_tree_render);
        assert!(b.killed.iter().all(|(window, mode)| {
            *window == 2 && *mode == crate::data::KillWindow::KILL_WINDOW
        }));
        assert_eq!(b.killed.len(), 1);

        /* src/tree.c:219-223: tree_close_internal only sends WM_DELETE and
         * returns; the con is closed when the UnmapNotify arrives, which the
         * handler does with DONT_KILL_WINDOW. */
        assert!(t.cons.contains_key(con2));
        crate::tree::tree_close_internal(
            &mut t,
            &mut b,
            con2,
            crate::data::KillWindow::DONT_KILL_WINDOW,
            false,
        );
        assert!(!t.cons.contains_key(con2));
        assert!(t.cons.contains_key(con1));
    }

    /// C: 'focus left' moves the focus to the left neighbour
    /// (commands.c:1292 cmd_focus_direction).
    #[test]
    fn focus_direction() {
        let (mut t, _o, _ws) = test_tree();
        let con1 = open_window(&mut t, "XTerm", 1);
        let con2 = open_window(&mut t, "XTerm", 2);
        assert_eq!(t.focused, Some(con2));

        run(&mut t, "focus left");
        assert_eq!(t.focused, Some(con1));

        run(&mut t, "focus right");
        assert_eq!(t.focused, Some(con2));
    }

    /// C: 'kill' closes the focused window (commands.c:1213).
    #[test]
    fn kill_closes_window() {
        let (mut t, _o, _ws) = test_tree();
        let con1 = open_window(&mut t, "XTerm", 1);
        let con2 = open_window(&mut t, "XTerm", 2);

        let mut b = KillRecorder::default();
        let result = run_with(&mut t, &mut b, "kill");
        assert!(result.needs_tree_render);
        assert_eq!(b.killed, vec![(2, crate::data::KillWindow::KILL_WINDOW)]);

        /* src/tree.c:219-223: the con stays until the client goes away; the
         * UnmapNotify handler then closes it with DONT_KILL_WINDOW. */
        assert!(t.cons.contains_key(con2));
        crate::tree::tree_close_internal(
            &mut t,
            &mut b,
            con2,
            crate::data::KillWindow::DONT_KILL_WINDOW,
            false,
        );
        assert!(!t.cons.contains_key(con2));
        assert_eq!(t.focused, Some(con1));
    }

    /// C: 'resize grow width 10 px' redistributes percentages between the
    /// two children of the parent (C: cmd_resize_tiling_direction +
    /// resize_neighboring_cons, src/resize.c:146).
    #[test]
    fn resize_grow_width_percent() {
        let (mut t, _o, _ws) = test_tree();
        open_window(&mut t, "XTerm", 1);
        open_window(&mut t, "XTerm", 2);
        let parent = t.cons[t.focused.unwrap()].parent.unwrap();

        // Freshly opened cons have percent 0 until the first render;
        // initialize them like con_fix_percent does.
        let children: Vec<ConId> = t.cons[parent].nodes_head.iter().copied().collect();
        for child in &children {
            t.cons[*child].percent = 0.5;
        }

        let result = run(&mut t, "resize grow width 10 px");
        assert!(result.needs_tree_render);

        /* src/commands.c:470 cmd_resize_tiling_width_height grows the focused
         * container ("current"); the siblings lose the same amount. */
        let focused = t.focused.unwrap();
        let other = t.cons[parent]
            .nodes_head
            .iter()
            .copied()
            .find(|c| *c != focused)
            .unwrap();
        assert!(t.cons[focused].percent > 0.5);
        assert!(t.cons[other].percent < 0.5);
        assert!((t.cons[focused].percent + t.cons[other].percent - 1.0).abs() < 1e-9);
    }

    /// C: 'resize set 800 px 400 px' sets an absolute size (commands.c:623).
    #[test]
    fn resize_set() {
        let (mut t, _o, _ws) = test_tree();
        open_window(&mut t, "XTerm", 1);
        open_window(&mut t, "XTerm", 2);
        let parent = t.cons[t.focused.unwrap()].parent.unwrap();
        // Simulate a rendered tree: children with equal percentages.
        let children: Vec<ConId> = t.cons[parent].nodes_head.iter().copied().collect();
        for child in &children {
            t.cons[*child].percent = 0.5;
            t.cons[*child].rect = Rect::new(0, 0, 640, 800);
        }
        t.cons[parent].rect = Rect::new(0, 0, 1280, 800);

        let result = run(&mut t, "resize set 800 px 400 px");
        assert!(result.needs_tree_render);

        /* src/commands.c:590 resize_set_tiling resizes the container the
         * command was run on, i.e. the focused one. 800px of 1280px total
         * width -> 62.5%. */
        let focused = t.focused.unwrap();
        assert!((t.cons[focused].percent - 800.0 / 1280.0).abs() < 0.02);
    }

    /// C: 'floating enable' makes the window float (commands.c:1142).
    #[test]
    fn floating_toggle() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 1);
        let ws = con_get_workspace(&t, con).unwrap();

        let result = run(&mut t, "floating enable");
        assert!(result.needs_tree_render);
        assert_eq!(t.cons[con].floating, FloatingState::FLOATING_USER_ON);
        let parent = t.cons[con].parent.unwrap();
        assert_eq!(t.cons[parent].r#type, ConType::CT_FLOATING_CON);
        assert!(t.cons[ws].floating_head.contains(&parent));

        run(&mut t, "floating disable");
        assert_eq!(t.cons[con].floating, FloatingState::FLOATING_USER_OFF);
        let parent = t.cons[con].parent.unwrap();
        assert_eq!(t.cons[parent].r#type, ConType::CT_WORKSPACE);
    }

    /// C: 'mark m' + 'unmark' (commands.c:934/967).
    #[test]
    fn mark_and_unmark() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 1);

        run(&mut t, "mark m1");
        assert_eq!(t.cons[con].marks_head.len(), 1);
        assert_eq!(t.cons[con].marks_head[0].name, "m1");

        // A second mark with --replace replaces the first one.
        run(&mut t, "mark --replace m2");
        assert_eq!(t.cons[con].marks_head.len(), 1);
        assert_eq!(t.cons[con].marks_head[0].name, "m2");

        run(&mut t, "unmark");
        assert_eq!(t.cons[con].marks_head.len(), 0);
    }

    /// C: 'move scratchpad' + 'scratchpad show' (commands.c:1921/1941).
    #[test]
    fn scratchpad_move_and_show() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 1);
        let ws1 = con_get_workspace(&t, con).unwrap();

        let result = run(&mut t, "move scratchpad");
        assert!(result.needs_tree_render);

        let scratch_ws = get_existing_workspace_by_name(&t, "__i3_scratch")
            .expect("scratchpad workspace exists");
        let parent = t.cons[con].parent.unwrap();
        assert_eq!(t.cons[parent].parent, Some(scratch_ws));
        /* src/scratchpad.c:63-72: the state is set on the floating container
         * (con was reassigned to con->parent by floating_enable), not on the
         * leaf window con. */
        assert_eq!(
            t.cons[parent].scratchpad_state,
            ScratchpadState::SCRATCHPAD_FRESH
        );

        // Show it again: it is moved to the then-current workspace.
        run(&mut t, "workspace 2");
        let result = run(&mut t, "scratchpad show");
        assert!(result.needs_tree_render);
        /* src/scratchpad.c:171-195: without criteria, scratchpad_show() takes
         * the first con of __i3_scratch and moves it to the *active*
         * workspace. */
        let ws2 = get_existing_workspace_by_name(&t, "2").expect("workspace 2 exists");
        let ws = con_get_workspace(&t, con).unwrap();
        assert_eq!(ws, ws2);
        assert_ne!(ws, ws1);
        assert_ne!(ws, scratch_ws);
    }

    /// C: 'move workspace 3' moves the focused window to workspace "3"
    /// (commands.c:316 cmd_move_con_to_workspace_name).
    #[test]
    fn move_to_workspace() {
        let (mut t, _o, _ws) = test_tree();
        let con1 = open_window(&mut t, "XTerm", 1);
        let con2 = open_window(&mut t, "XTerm", 2);

        let result = run(&mut t, "move workspace 3");
        assert!(result.needs_tree_render);

        let ws3 = get_existing_workspace_by_num(&t, 3).unwrap();
        assert_eq!(con_get_workspace(&t, con2), Some(ws3));
        assert_ne!(con_get_workspace(&t, con1), Some(ws3));
    }

    /// C: 'rename workspace to <new>' renames and renumbers (commands.c:2114).
    #[test]
    fn rename_workspace() {
        let (mut t, _o, _ws) = test_tree();
        open_window(&mut t, "XTerm", 1);

        let result = run(&mut t, "rename workspace to stuff");
        assert!(result.needs_tree_render);

        let focused_ws = con_get_workspace(&t, t.focused.unwrap()).unwrap();
        assert_eq!(t.cons[focused_ws].name.as_deref(), Some("stuff"));
        assert_eq!(t.cons[focused_ws].num, -1);

        // C: src/commands.c:2140-2146 — renaming to the current name is a
        // no-op that still succeeds (and renders).
        let result = run(&mut t, "rename workspace to stuff");
        assert!(result.needs_tree_render);

        // C: src/commands.c:2140-2146 — renaming to another existing
        // workspace fails without touching the tree.
        run(&mut t, "workspace other");
        /* Give "other" a window, otherwise it is cleaned up when switching
         * away and the rename would succeed. */
        open_window(&mut t, "XTerm", 2);
        run(&mut t, "workspace stuff");
        let result = run(&mut t, "rename workspace to other");
        assert!(!result.needs_tree_render);
        let focused_ws = con_get_workspace(&t, t.focused.unwrap()).unwrap();
        assert_eq!(t.cons[focused_ws].name.as_deref(), Some("stuff"));
    }

    /// C: 'fullscreen toggle' sets CF_OUTPUT on the focused window
    /// (commands.c:1488).
    #[test]
    fn fullscreen_toggle() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 1);

        run(&mut t, "fullscreen toggle");
        assert_eq!(t.cons[con].fullscreen_mode, FullscreenMode::CF_OUTPUT);

        run(&mut t, "fullscreen toggle");
        assert_eq!(t.cons[con].fullscreen_mode, FullscreenMode::CF_NONE);
    }

    /// C: 'move left' swaps the focused con with its left neighbour
    /// (commands.c:1554 cmd_move_direction + src/move.c tree_move).
    #[test]
    fn move_direction() {
        let (mut t, _o, _ws) = test_tree();
        let con1 = open_window(&mut t, "XTerm", 1);
        let con2 = open_window(&mut t, "XTerm", 2);
        let parent = t.cons[con1].parent.unwrap();

        run(&mut t, "move left");
        assert_eq!(t.cons[parent].nodes_head[0], con2);
        assert_eq!(t.cons[parent].nodes_head[1], con1);
    }

    /// C: 'swap with con_id <id>' swaps two containers (commands.c:1964).
    #[test]
    fn swap_by_con_id() {
        let (mut t, _o, _ws) = test_tree();
        let con1 = open_window(&mut t, "XTerm", 1);
        let con2 = open_window(&mut t, "XTerm", 2);
        let parent = t.cons[con1].parent.unwrap();

        /* The focused con (con2) is the "match"; swap it with con1
         * (src/commands.c:2011 con_swap). */
        let result = run(&mut t, &format!("swap with con_id {}", con1.data().as_ffi()));
        assert!(result.needs_tree_render);
        assert_eq!(t.cons[parent].nodes_head[0], con2);
        assert_eq!(t.cons[parent].nodes_head[1], con1);
    }

    /// C: 'gaps inner current plus 5' updates the workspace gaps
    /// (commands.c:2477 + gaps_update at 2393).
    #[test]
    fn gaps_inner_plus() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 1);
        let ws = con_get_workspace(&t, con).unwrap();

        let result = run(&mut t, "gaps inner current plus 5");
        assert!(result.needs_tree_render);
        assert_eq!(t.cons[ws].gaps.inner, 5);
        assert_eq!(t.config.gaps.inner, 0);

        run(&mut t, "gaps inner all set 10");
        assert_eq!(t.config.gaps.inner, 10);
        assert_eq!(t.cons[ws].gaps.inner, 0);

        /* C: src/commands.c:2465-2467 — with scope "current" the workspace
         * stores the *offset* from the global value: 3 - 10 = -7. */
        run(&mut t, "gaps inner current set 3");
        assert_eq!(t.cons[ws].gaps.inner, -7);
    }

    /// C: 'sticky enable' sets sticky on a floating window (commands.c:1515).
    #[test]
    fn sticky_enable() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 1);
        run(&mut t, "floating enable");

        run(&mut t, "sticky enable");
        assert!(t.cons[con].sticky);

        run(&mut t, "sticky disable");
        assert!(!t.cons[con].sticky);
    }

    /// C: compound commands run left to right (commands_parser.c).
    #[test]
    fn compound_command() {
        let (mut t, _o, _ws) = test_tree();
        open_window(&mut t, "XTerm", 1);

        let result = run(&mut t, "workspace 2, border none");
        assert!(result.needs_tree_render);
        let focused_ws = con_get_workspace(&t, t.focused.unwrap()).unwrap();
        assert_eq!(t.cons[focused_ws].name.as_deref(), Some("2"));
        let con = t.focused.unwrap();
        assert_eq!(t.cons[con].border_style, BorderStyle::BS_NONE);
    }

    /// C: a parse error is reported through CommandResult (commands_parser.c
    /// builds the JSON error reply through the y_* hooks).
    #[test]
    fn parse_error() {
        let (mut t, _o, _ws) = test_tree();
        let mut gen = JsonGen::new();
        {
            let mut b = NullBackend;
            let mut runner = CommandRunner::new(&mut t, &mut b, Some(gen), None);
            let result = runner.run("nonsensecommand");
            assert!(result.parse_error);
            assert!(result.error_message.is_some());
            gen = runner.take_gen().unwrap();
        }
        assert_eq!(
            gen.get_buf(),
            // The token order is the one generate-command-parser.pl emits for
            // tokens_INITIAL (i.e. the order in parser-specs/commands.spec).
            r#"[{"success":false,"parse_error":true,"error":"Expected one of these tokens: <end>, '[', 'move', 'exec', 'exit', 'restart', 'reload', 'shmlog', 'debuglog', 'border', 'layout', 'append_layout', 'workspace', 'focus', 'kill', 'open', 'fullscreen', 'sticky', 'split', 'floating', 'mark', 'unmark', 'resize', 'rename', 'nop', 'scratchpad', 'swap', 'title_format', 'title_window_icon', 'mode', 'bar', 'gaps'","input":"nonsensecommand","errorposition":"^^^^^^^^^^^^^^^"}]"#
        );
    }

    /// C: match_init/match_is_empty (src/match.c:26/39).
    #[test]
    fn match_helpers() {
        let mut m = Match::default();
        match_init(&mut m);
        assert!(match_is_empty(&m));

        let t = Tree::new();
        match_parse_property(&t, &mut m, "class", Some("XTerm"));
        assert!(!match_is_empty(&m));
        assert!(m.class.as_ref().unwrap().matches("XTerm"));
        assert!(!m.class.as_ref().unwrap().matches("Chromium"));
    }

    /// Records the two command-layer hooks: start_application (exec) and
    /// switch_mode (mode).
    #[derive(Default)]
    struct ExecModeRecorder {
        execs: Vec<(String, bool, u32)>,
        modes: Vec<String>,
    }

    impl crate::backend::ConBackend for ExecModeRecorder {
        fn start_application(
            &mut self,
            _t: &Tree,
            command: &str,
            no_startup_id: bool,
            window_id: u32,
        ) {
            self.execs
                .push((command.to_string(), no_startup_id, window_id));
        }

        fn switch_mode(&mut self, _t: &mut Tree, mode: &str) {
            self.modes.push(mode.to_string());
        }
    }

    /// C: src/commands.c:1246 cmd_exec — the command string is handed to
    /// start_application() verbatim; --no-startup-id only clears the startup
    /// notification flag, and the window id of the matched container is passed
    /// through (XCB_NONE without a window).
    #[test]
    fn exec_starts_application() {
        let (mut t, _o, _ws) = test_tree();
        let con = open_window(&mut t, "XTerm", 0x1234);

        let mut rec = ExecModeRecorder::default();
        let result = run_with(&mut t, &mut rec, "exec --no-startup-id xmessage hi");
        assert!(!result.parse_error);
        assert_eq!(
            rec.execs,
            vec![("xmessage hi".to_string(), true, 0x1234)],
            "the command keeps its arguments, the flag is stripped"
        );
        assert_eq!(t.windows[t.cons[con].window.unwrap()].id, 0x1234);

        let mut rec = ExecModeRecorder::default();
        run_with(&mut t, &mut rec, "exec i3-sensible-terminal");
        assert_eq!(
            rec.execs,
            vec![("i3-sensible-terminal".to_string(), false, 0x1234)]
        );

        // A container without a window passes XCB_NONE (C: window == NULL).
        t.cons[con].window = None;
        let mut rec = ExecModeRecorder::default();
        run_with(&mut t, &mut rec, "exec true");
        assert_eq!(rec.execs, vec![("true".to_string(), false, 0)]);
    }

    /// C: src/commands.c:986 cmd_mode — the mode name reaches switch_mode()
    /// (through the backend hook) unchanged.
    #[test]
    fn mode_requests_switch() {
        let (mut t, _o, _ws) = test_tree();
        let _ = open_window(&mut t, "XTerm", 1);

        let mut rec = ExecModeRecorder::default();
        let result = run_with(&mut t, &mut rec, "mode resize");
        assert!(!result.parse_error);
        assert_eq!(rec.modes, vec!["resize".to_string()]);

        // A quoted mode name is unquoted by the parser, like C.
        let mut rec = ExecModeRecorder::default();
        run_with(&mut t, &mut rec, "mode \"default\"");
        assert_eq!(rec.modes, vec!["default".to_string()]);
    }

    /// C: parse_long (src/util.c:425).
    #[test]
    fn parse_long_helpers() {
        assert_eq!(parse_long("10", 0), Some(10));
        assert_eq!(parse_long("0x1f", 0), Some(31));
        assert_eq!(parse_long("-5", 0), None); // C: result < 0 -> false
        assert_eq!(parse_long("10abc", 0), None);
        assert_eq!(parse_long("", 0), Some(0)); // strtol quirk, 1:1
        assert_eq!(parse_long("99999999999999999999", 0), None);
    }
}
