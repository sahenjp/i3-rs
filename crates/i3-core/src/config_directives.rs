//! C: src/config_directives.c — all config storing functions, ported 1:1 as
//! the ConfigHooks implementation (the cfg_* functions of the C config
//! parser). Each hook writes into the ConfigDirectives state, which carries
//! the C globals the config layer uses (config, modes, barconfigs,
//! included_files, assignments, ws_assignments, autostarts, …).
//!
//! Helpers from src/bindings.c (configure_binding, mode_from_name,
//! check_for_duplicate_bindings) and src/match.c (match_init and friends)
//! are ported here as well because the config directives are their only
//! users so far — TODO: move them into bindings.rs / match_.rs when those
//! files are ported, reconciling with their authors.

// C identifiers are kept verbatim (enum values, struct names).
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

use std::collections::VecDeque;

use i3_spec::config_parser::{ConfigHooks, ConfigParserCtx, ConfigResultIr, ParserContext};
use libi3::boolstr;
use libi3::strcasecmp;

use crate::config::{
    draw_util_hex_to_color, load_font, logical_px, wordexp, Barconfig, Barbinding, Colortriple,
    Configuration, IncludedFile, Mode, bar_hidden_state_t, bar_mode_t, bar_position_t,
    tray_output_t,
    focus_on_window_activation_t, popup_during_fullscreen_t, title_align_t, tiling_drag_t,
};
use crate::con::ConId;
use crate::data::{
    regex, Assignment, Autostart, Binding, BindingRelease, GapsMask, HideEdgeBordersMode,
    InputType, Layout, Match, MatchDock, MatchUrgency, MatchWindowMode, Orientation, SmartGaps,
    Warping, Workspace_Assignment, A_COMMAND, A_NO_FOCUS, A_TO_OUTPUT, A_TO_WORKSPACE,
    A_TO_WORKSPACE_NUMBER, BorderStyle, FocusWrapping, GAPS_BOTTOM, GAPS_HORIZONTAL, GAPS_INNER,
    GAPS_LEFT, GAPS_OUTER, GAPS_RIGHT, GAPS_TOP, GAPS_VERTICAL, I3_XKB_GROUP_MASK_1,
    I3_XKB_GROUP_MASK_2, I3_XKB_GROUP_MASK_3, I3_XKB_GROUP_MASK_4, XCB_WINDOW_NONE,
};
use i3_spec::config_parser::ParseFileResult;

/// C: src/bindings.c:25 DEFAULT_BINDING_MODE
pub const DEFAULT_BINDING_MODE: &str = "default";

/// C: xcb/xproto.h — the key/button mask bits (the subset i3 uses; C:
/// XCB_KEY_BUT_MASK_*). TODO(x11 port): move into a shared xcb constants
/// module when the x layer lands.
pub const XCB_NONE: u32 = 0;
/// C: XCB_KEY_BUT_MASK_SHIFT
pub const XCB_KEY_BUT_MASK_SHIFT: u32 = 1 << 0;
/// C: XCB_KEY_BUT_MASK_LOCK
pub const XCB_KEY_BUT_MASK_LOCK: u32 = 1 << 1;
/// C: XCB_KEY_BUT_MASK_CONTROL
pub const XCB_KEY_BUT_MASK_CONTROL: u32 = 1 << 2;
/// C: XCB_KEY_BUT_MASK_MOD_1
pub const XCB_KEY_BUT_MASK_MOD_1: u32 = 1 << 3;
/// C: XCB_KEY_BUT_MASK_MOD_2
pub const XCB_KEY_BUT_MASK_MOD_2: u32 = 1 << 4;
/// C: XCB_KEY_BUT_MASK_MOD_3
pub const XCB_KEY_BUT_MASK_MOD_3: u32 = 1 << 5;
/// C: XCB_KEY_BUT_MASK_MOD_4
pub const XCB_KEY_BUT_MASK_MOD_4: u32 = 1 << 6;
/// C: XCB_KEY_BUT_MASK_MOD_5
pub const XCB_KEY_BUT_MASK_MOD_5: u32 = 1 << 7;

/// C: src/match.c:346-373 — the window_type criterion values are the X11
/// atoms (A__NET_WM_WINDOW_TYPE_*) which i3 interns at startup. The port
/// keeps stable placeholder ids for them; the table lives next to the IPC
/// dump in [crate::dump] (which turns the same ids back into names), and
/// the Window side maps the runtime atom onto them in
/// [crate::manage::window_type_from_atom]. The config parser and the
/// command parser must share this table, otherwise `for_window
/// [window_type=...]` and `[window_type=...] command` compare different
/// numbers and the criterion never matches.
pub use crate::dump::{
    A__NET_WM_WINDOW_TYPE_DIALOG, A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
    A__NET_WM_WINDOW_TYPE_MENU, A__NET_WM_WINDOW_TYPE_NORMAL,
    A__NET_WM_WINDOW_TYPE_NOTIFICATION, A__NET_WM_WINDOW_TYPE_POPUP_MENU,
    A__NET_WM_WINDOW_TYPE_SPLASH, A__NET_WM_WINDOW_TYPE_TOOLBAR,
    A__NET_WM_WINDOW_TYPE_TOOLTIP, A__NET_WM_WINDOW_TYPE_UTILITY,
};

/// The state of src/config_directives.c plus the C globals it uses.
pub struct ConfigDirectives {
    /// C: src/config.c `Config config`
    pub config: Configuration,
    /// C: src/config.c `struct modes_head modes` (SLIST, insert-head — a
    /// VecDeque with push_front).
    pub modes: VecDeque<Mode>,
    /// C: src/config.c `struct barconfig_head barconfigs` (TAILQ).
    pub barconfigs: VecDeque<Barconfig>,
    /// C: src/config.c `struct includedfiles_head included_files`.
    pub included_files: VecDeque<IncludedFile>,
    /// C: src/config.c `char *current_configpath`.
    pub current_configpath: Option<String>,
    /// C: src/config_directives.c:186 `static char *current_mode`.
    current_mode: Option<String>,
    /// C: src/config_directives.c:187 `static bool current_mode_pango_markup`.
    current_mode_pango_markup: bool,
    /// C: src/config_directives.c:538 `static char *current_workspace`.
    current_workspace: Option<String>,
    /// C: src/config_directives.c:729 `static Barconfig *current_bar`.
    current_bar: Option<Barconfig>,
    /// C: src/config_directives.c:91 `static int criteria_next_state`.
    criteria_next_state: i32,
    /// C: src/assignments.c `struct assignments_head assignments` (TAILQ).
    pub assignments: VecDeque<Assignment>,
    /// C: src/config.c `struct ws_assignments_head ws_assignments` (TAILQ).
    pub ws_assignments: VecDeque<Workspace_Assignment>,
    /// C: src/startup.c `struct autostarts_head autostarts` (TAILQ).
    pub autostarts: VecDeque<Autostart>,
    /// C: src/startup.c `struct autostarts_head autostarts_always` (TAILQ).
    pub autostarts_always: VecDeque<Autostart>,
    /// C: src/bindings.c `char *current_binding_mode`.
    pub current_binding_mode: Option<String>,
    /// C: src/config.c:52 `pid_t config_error_nagbar_pid = -1` — the pid of
    /// the i3-nagbar the config parser started (config_parser.c:820). A
    /// reload kills it before parsing again (src/commands.c:1671).
    pub config_error_nagbar_pid: i32,
    /// C: src/ipc.c `static double kill_timeout` as set by
    /// CFGFUN(ipc_kill_timeout) (config_directives.c:707
    /// `ipc_set_kill_timeout(timeout_ms / 1000.0)`). C writes the ipc layer's
    /// global directly; the port stores the value here and the event loop
    /// hands it to the IpcServer after load_configuration()/reload.
    pub ipc_kill_timeout: f64,
}

impl Default for ConfigDirectives {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigDirectives {
    /// A fresh state, like the C zero-initialized globals.
    pub fn new() -> ConfigDirectives {
        ConfigDirectives {
            config: Configuration::default(),
            modes: VecDeque::new(),
            barconfigs: VecDeque::new(),
            included_files: VecDeque::new(),
            current_configpath: None,
            current_mode: None,
            current_mode_pango_markup: false,
            current_workspace: None,
            current_bar: None,
            criteria_next_state: 0,
            assignments: VecDeque::new(),
            ws_assignments: VecDeque::new(),
            autostarts: VecDeque::new(),
            autostarts_always: VecDeque::new(),
            current_binding_mode: None,
            // C: src/config.c:52 pid_t config_error_nagbar_pid = -1
            config_error_nagbar_pid: -1,
            // C: src/ipc.c kill_timeout (ipc.c:35 ipc_set_kill_timeout) — the
            // initial value is the IPC layer's DEFAULT_KILL_TIMEOUT (5s).
            ipc_kill_timeout: crate::ipc::DEFAULT_KILL_TIMEOUT,
        }
    }

    /// C: src/bindings.c:32 mode_from_name — returns the mode specified by
    /// name or creates a new one and adds it to the list of modes (C: an
    /// SLIST with insert-head).
    pub fn mode_from_name(&mut self, name: &str, pango_markup: bool) -> &mut Mode {
        if let Some(pos) = self.modes.iter().position(|m| m.name == name) {
            return &mut self.modes[pos];
        }
        self.modes.push_front(Mode {
            name: name.to_string(),
            pango_markup,
            bindings: VecDeque::new(),
        });
        self.modes.front_mut().unwrap()
    }

    /// C: src/bindings.c:58 configure_binding — adds a binding from config
    /// parameters given as strings and returns the binding structure (C
    /// returns NULL when the input code could not be parsed). The binding is
    /// inserted into the given mode's binding list; pushing it into the
    /// Tree (src/main.c `bindings`) is up to the caller.
    #[allow(clippy::too_many_arguments)]
    pub fn configure_binding(
        &mut self,
        bindtype: &str,
        modifiers: Option<&str>,
        input_code: &str,
        release: Option<&str>,
        border: Option<&str>,
        whole_window: Option<&str>,
        exclude_titlebar: Option<&str>,
        command: &str,
        modename: &str,
        pango_markup: bool,
    ) -> Option<Binding> {
        let mut new_binding = Binding::default();
        // C: DLOG("Binding %p bindtype %s, modifiers %s, input code %s, release %s\n", ...);
        new_binding.release = if release.is_some() {
            BindingRelease::B_UPON_KEYRELEASE
        } else {
            BindingRelease::B_UPON_KEYPRESS
        };
        new_binding.border = border.is_some();
        new_binding.whole_window = whole_window.is_some();
        new_binding.exclude_titlebar = exclude_titlebar.is_some();
        if bindtype == "bindsym" {
            // C: strncasecmp(input_code, "button", sizeof("button") - 1)
            new_binding.input_type = if strncasecmp_prefix(input_code, "button") {
                InputType::B_MOUSE
            } else {
                InputType::B_KEYBOARD
            };
            new_binding.symbol = Some(input_code.to_string());
        } else {
            // C: if (!parse_long(input_code, &keycode, 10)) { ELOG; FREE; return NULL; }
            let keycode = match parse_long(input_code, 10) {
                Some(keycode) => keycode,
                None => {
                    crate::log::errorlog(&format!(
                        "Could not parse \"{}\" as an input code, ignoring this binding.\n",
                        input_code
                    ));
                    return None;
                }
            };
            new_binding.keycode = keycode as u32;
            new_binding.input_type = InputType::B_KEYBOARD;
        }
        new_binding.command = command.to_string();
        new_binding.event_state_mask = event_state_from_str(modifiers);
        let mut group_bits_set = 0;
        for mask in [
            I3_XKB_GROUP_MASK_1.0,
            I3_XKB_GROUP_MASK_2.0,
            I3_XKB_GROUP_MASK_3.0,
            I3_XKB_GROUP_MASK_4.0,
        ] {
            if (new_binding.event_state_mask >> 16) & mask != 0 {
                group_bits_set += 1;
            }
        }
        if group_bits_set > 1 {
            crate::log::errorlog(
                "Keybinding has more than one Group specified, but your X server is always in \
                 precisely one group. The keybinding can never trigger.\n",
            );
        }

        // C: const struct Mode *mode = mode_from_name(modename, pango_markup);
        //    TAILQ_INSERT_TAIL(mode->bindings, new_binding, bindings);
        let mode = self.mode_from_name(modename, pango_markup);
        mode.bindings.push_back(new_binding.clone());

        // C: TAILQ_INIT(&(new_binding->keycodes_head)); — empty by default.
        Some(new_binding)
    }

    /// C: src/config_directives.c:729 current_bar — C dereferences the
    /// pointer unconditionally (unreachable outside a bar block per the
    /// parser spec); panic loudly instead of undefined behaviour.
    fn bar(&mut self) -> &mut Barconfig {
        self.current_bar.as_mut().expect("BUG: bar directive outside a bar block")
    }

    /// C: src/config_directives.c:768 dlog_padding — DLOG only; skipped (the
    /// i3 debug log lives in i3-core's logging).
    fn dlog_padding(&self) {}
}

/// C: C handlers receive the stack value directly; the generated port wraps
/// it in Option<&str>. NULL is unreachable for these arguments (the parser
/// only leaves the stack empty after error recovery, which does not call
/// the handlers), so the unwrap mirrors the C dereference.
fn req(value: Option<&str>) -> &str {
    value.unwrap_or("")
}

/// C: strncasecmp(s, prefix, strlen(prefix)) == 0.
fn strncasecmp_prefix(s: &str, prefix: &str) -> bool {
    s.as_bytes().len() >= prefix.len()
        && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// C: src/util.c:425 parse_long — a strtol wrapper checking the parsing
/// result. Returns None when C would return false (LONG_MIN/LONG_MAX, a
/// negative value, or trailing garbage).
pub fn parse_long(s: &str, base: u32) -> Option<i64> {
    let (value, end) = strtol_full(s.as_bytes(), base)?;
    if value == i64::MIN || value == i64::MAX || value < 0 || end != s.as_bytes().len() {
        return None;
    }
    Some(value)
}

/// C: strtol(s, &end, base) — returns the value and the byte offset of the
/// first character that is not part of the number, or None when no
/// conversion could be performed (C: end == walk).
fn strtol_full(s: &[u8], base: u32) -> Option<(i64, usize)> {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c') {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let radix = if base == 0 {
        if i + 1 < s.len() && s[i] == b'0' && (s[i + 1] | 0x20) == b'x' {
            i += 2;
            16
        } else if i < s.len() && s[i] == b'0' {
            8
        } else {
            10
        }
    } else if base == 16 && i + 1 < s.len() && s[i] == b'0' && (s[i + 1] | 0x20) == b'x' {
        i += 2;
        16
    } else {
        base as usize
    };
    let digits_start = i;
    let mut acc: i128 = 0;
    let mut overflow = false;
    while i < s.len() {
        let digit = match s[i] {
            b'0'..=b'9' => (s[i] - b'0') as usize,
            b'a'..=b'z' => (s[i] - b'a' + 10) as usize,
            b'A'..=b'Z' => (s[i] - b'A' + 10) as usize,
            _ => break,
        };
        if digit >= radix {
            break;
        }
        acc = acc * radix as i128 + digit as i128;
        if acc > i64::MAX as i128 {
            // C: ERANGE → LONG_MAX (or LONG_MIN for negative values).
            overflow = true;
        }
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    let mut value = acc.min(i64::MAX as i128) as i64;
    if neg {
        value = -value;
        if overflow {
            value = i64::MIN;
        }
    } else if overflow {
        value = i64::MAX;
    }
    Some((value, i))
}

/// C: atoi(3) — strtol base 10 without error checking.
fn atoi(s: &str) -> i32 {
    strtol_full(s.as_bytes(), 10)
        .map(|(value, _)| value as i32)
        .unwrap_or(0)
}

/// C: src/config_directives.c:127 event_state_from_str — converts a string
/// containing the group and modifiers to the corresponding bit mask. The
/// simpler strstr()-based approach of the C code is kept.
fn event_state_from_str(s: Option<&str>) -> u32 {
    // C: if (str == NULL) { return result; }
    let s = match s {
        Some(s) => s,
        None => return 0,
    };
    let mut result = 0u32;
    if s.contains("Mod1") {
        result |= XCB_KEY_BUT_MASK_MOD_1;
    }
    if s.contains("Mod2") {
        result |= XCB_KEY_BUT_MASK_MOD_2;
    }
    if s.contains("Mod3") {
        result |= XCB_KEY_BUT_MASK_MOD_3;
    }
    if s.contains("Mod4") {
        result |= XCB_KEY_BUT_MASK_MOD_4;
    }
    if s.contains("Mod5") {
        result |= XCB_KEY_BUT_MASK_MOD_5;
    }
    if s.contains("Control") || s.contains("Ctrl") {
        result |= XCB_KEY_BUT_MASK_CONTROL;
    }
    if s.contains("Shift") {
        result |= XCB_KEY_BUT_MASK_SHIFT;
    }

    if s.contains("Group1") {
        result |= I3_XKB_GROUP_MASK_1.0 << 16;
    }
    if s.contains("Group2") || s.contains("Mode_switch") {
        result |= I3_XKB_GROUP_MASK_2.0 << 16;
    }
    if s.contains("Group3") {
        result |= I3_XKB_GROUP_MASK_3.0 << 16;
    }
    if s.contains("Group4") {
        result |= I3_XKB_GROUP_MASK_4.0 << 16;
    }
    result
}

/// C: src/match.c:26 match_init — initializes the Match data structure. The
/// members representing boolean values (like dock) need to be initialized
/// with -1 instead of 0 in C (memset 0 then explicit fixes).
fn match_init(match_: &mut Match) {
    *match_ = Match::default();
    match_.urgent = MatchUrgency::U_DONTCHECK;
    match_.window_mode = MatchWindowMode::WM_ANY;
    // we use this as the placeholder value for "not set". (C comment)
    match_.window_type = u32::MAX;
    // C: memset 0 leaves dock at 0 == M_NODOCK; data.rs defaults to
    // M_DONTCHECK, so it is set explicitly.
    match_.dock = MatchDock::M_NODOCK;
}

/// C: src/match.c:39 match_is_empty — necessary while parsing commands to
/// see whether the user specified a match at all.
fn match_is_empty(match_: &Match) -> bool {
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

/// C: src/match.c:64 match_copy — copies the data of a match from src to
/// dest. The DUPLICATE_REGEX macro creates a new regular expression from
/// the ->pattern of the old one (a little more memory than a refcounting
/// system, but easier) (C comment).
fn match_copy(dest: &mut Match, src: &Match) {
    *dest = src.clone();
    let duplicate = |field: &mut Option<Box<regex>>| {
        if let Some(r) = field {
            let pattern = r.pattern.clone();
            *r = Box::new(regex::new(&pattern));
        }
    };
    duplicate(&mut dest.title);
    duplicate(&mut dest.mark);
    duplicate(&mut dest.application);
    duplicate(&mut dest.class);
    duplicate(&mut dest.instance);
    duplicate(&mut dest.window_role);
    duplicate(&mut dest.workspace);
    duplicate(&mut dest.machine);
}

/// C: src/match.c:279 match_free — frees the given match. Only the error
/// string and the regexes are freed in C; the remaining members keep their
/// values (Rust drops the owned Strings).
fn match_free(match_: &mut Match) {
    match_.error = None;
    match_.title = None;
    match_.application = None;
    match_.class = None;
    match_.instance = None;
    match_.mark = None;
    match_.window_role = None;
    match_.workspace = None;
    match_.machine = None;
}

/// C: src/match.c:295 match_parse_property — interprets a ctype=cvalue pair
/// and adds it to the given match specification. focused corresponds to the
/// C `focused` global (NULL during config parsing, which has no tree yet).
fn match_parse_property(
    match_: &mut Match,
    ctype: &str,
    cvalue: Option<&str>,
    focused: Option<ConId>,
) {
    // C: DLOG("ctype=*%s*, cvalue=*%s*\n", ctype, cvalue);
    if ctype == "class" {
        match_.class = Some(Box::new(regex::new(req(cvalue))));
        return;
    }

    if ctype == "instance" {
        match_.instance = Some(Box::new(regex::new(req(cvalue))));
        return;
    }

    if ctype == "window_role" {
        match_.window_role = Some(Box::new(regex::new(req(cvalue))));
        return;
    }

    if ctype == "con_id" {
        let cvalue = req(cvalue);
        if cvalue == "__focused__" {
            // C: match->con_id = focused;
            match_.con_id = focused;
            return;
        }
        match match_parse_long(cvalue, 0) {
            None => {
                crate::log::errorlog(&format!("Could not parse con id \"{}\"\n", cvalue));
                match_.error = Some("invalid con_id".to_string());
            }
            Some(_parsed) => {
                // C: match->con_id = (Con *)parsed; — a Con pointer; the
                // Rust port stores ConIds, and a raw pointer cannot be
                // reconstructed from the config text (TODO: the criterion is
                // effectively unmatchable during config parsing, like in C
                // without a running tree).
            }
        }
        return;
    }

    if ctype == "id" {
        let cvalue = req(cvalue);
        match match_parse_long(cvalue, 0) {
            None => {
                crate::log::errorlog(&format!("Could not parse window id \"{}\"\n", cvalue));
                match_.error = Some("invalid id".to_string());
            }
            Some(parsed) => {
                match_.id = parsed as u32;
            }
        }
        return;
    }

    if ctype == "window_type" {
        let cvalue = req(cvalue);
        match window_type_atom(&cvalue) {
            Some(atom) => match_.window_type = atom,
            None => {
                // C: ELOG("unknown window_type value \"%s\"\n", cvalue);
                crate::log::errorlog(&format!("unknown window_type value \"{}\"\n", cvalue));
                match_.error = Some("unknown window_type value".to_string());
            }
        }
        return;
    }

    if ctype == "con_mark" {
        match_.mark = Some(Box::new(regex::new(req(cvalue))));
        return;
    }

    if ctype == "title" {
        match_.title = Some(Box::new(regex::new(req(cvalue))));
        return;
    }

    if ctype == "urgent" {
        let cvalue = req(cvalue);
        if strcasecmp(&cvalue, "latest") == 0
            || strcasecmp(&cvalue, "newest") == 0
            || strcasecmp(&cvalue, "recent") == 0
            || strcasecmp(&cvalue, "last") == 0
        {
            match_.urgent = MatchUrgency::U_LATEST;
        } else if strcasecmp(&cvalue, "oldest") == 0 || strcasecmp(&cvalue, "first") == 0 {
            match_.urgent = MatchUrgency::U_OLDEST;
        }
        return;
    }

    if ctype == "workspace" {
        match_.workspace = Some(Box::new(regex::new(req(cvalue))));
        return;
    }

    if ctype == "machine" {
        match_.machine = Some(Box::new(regex::new(req(cvalue))));
        return;
    }

    if ctype == "tiling" {
        match_.window_mode = MatchWindowMode::WM_TILING;
        return;
    }

    if ctype == "tiling_from" && cvalue.is_some() && req(cvalue) == "auto" {
        match_.window_mode = MatchWindowMode::WM_TILING_AUTO;
        return;
    }

    if ctype == "tiling_from" && cvalue.is_some() && req(cvalue) == "user" {
        match_.window_mode = MatchWindowMode::WM_TILING_USER;
        return;
    }

    if ctype == "floating" {
        match_.window_mode = MatchWindowMode::WM_FLOATING;
        return;
    }

    if ctype == "floating_from" && cvalue.is_some() && req(cvalue) == "auto" {
        match_.window_mode = MatchWindowMode::WM_FLOATING_AUTO;
        return;
    }

    if ctype == "floating_from" && cvalue.is_some() && req(cvalue) == "user" {
        match_.window_mode = MatchWindowMode::WM_FLOATING_USER;
        return;
    }

    // match_matches_window() only checks negatively, so match_all_windows
    // won't actually be used there, but that's OK because if no negative
    // match is found (e.g. because of a more restrictive criterion) the
    // return value of match_matches_window() is true. Setting it here only
    // serves to cause match_is_empty() to return false, otherwise empty
    // criteria rules apply, and that's not what we want. (C comment)
    if ctype == "all" {
        match_.match_all_windows = true;
        return;
    }

    // C: ELOG("Unknown criterion: %s\n", ctype);
    crate::log::errorlog(&format!("Unknown criterion: {}\n", ctype));
}

/// C: strcasecmp-based window_type lookup (match.c:346-373) — see the
/// A__NET_WM_WINDOW_TYPE_* placeholder note above.
fn window_type_atom(cvalue: &str) -> Option<u32> {
    if strcasecmp(cvalue, "normal") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_NORMAL)
    } else if strcasecmp(cvalue, "dialog") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_DIALOG)
    } else if strcasecmp(cvalue, "utility") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_UTILITY)
    } else if strcasecmp(cvalue, "toolbar") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_TOOLBAR)
    } else if strcasecmp(cvalue, "splash") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_SPLASH)
    } else if strcasecmp(cvalue, "menu") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_MENU)
    } else if strcasecmp(cvalue, "dropdown_menu") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU)
    } else if strcasecmp(cvalue, "popup_menu") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_POPUP_MENU)
    } else if strcasecmp(cvalue, "tooltip") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_TOOLTIP)
    } else if strcasecmp(cvalue, "notification") == 0 {
        Some(A__NET_WM_WINDOW_TYPE_NOTIFICATION)
    } else {
        None
    }
}

/// C: parse_long with the match.c error message — local wrapper.
fn match_parse_long(cvalue: &str, base: u32) -> Option<i64> {
    parse_long(cvalue, base)
}

/// C: src/config_directives.c:245 apply_gaps — gaps is NULL in C for
/// callers without a gaps struct; the port models that with Option.
fn apply_gaps(gaps: Option<&mut crate::data::gaps_t>, mask: GapsMask, value: i32) {
    let gaps = match gaps {
        Some(gaps) => gaps,
        None => return,
    };
    if (mask & GAPS_INNER).0 != 0 {
        gaps.inner = value;
    }
    if (mask & GAPS_TOP).0 != 0 {
        gaps.top = value;
    }
    if (mask & GAPS_RIGHT).0 != 0 {
        gaps.right = value;
    }
    if (mask & GAPS_BOTTOM).0 != 0 {
        gaps.bottom = value;
    }
    if (mask & GAPS_LEFT).0 != 0 {
        gaps.left = value;
    }
}

/// C: src/config_directives.c:266 create_gaps_assignment
fn create_gaps_assignment(d: &mut ConfigDirectives, workspace: &str, mask: GapsMask, pixels: i32) {
    if mask.0 == 0 {
        return;
    }

    // C: DLOG("Setting gaps for workspace %s", workspace);
    let mut found = false;
    for assignment in &d.ws_assignments {
        if strcasecmp(&assignment.name, workspace) == 0 {
            found = true;
            break;
        }
    }

    // Assignment does not yet exist, let's create it. (C comment)
    if !found {
        d.ws_assignments.push_back(Workspace_Assignment {
            name: workspace.to_string(),
            output: None,
            ..Default::default()
        });
    }

    let pos = d
        .ws_assignments
        .iter()
        .position(|a| strcasecmp(&a.name, workspace) == 0)
        .expect("create_gaps_assignment: assignment vanished");
    let assignment = &mut d.ws_assignments[pos];
    assignment.gaps_mask = assignment.gaps_mask | mask;
    apply_gaps(Some(&mut assignment.gaps), mask, pixels);
}

/// C: src/config_directives.c:294 gaps_scope_to_mask
fn gaps_scope_to_mask(scope: &str) -> GapsMask {
    if scope == "inner" {
        return GAPS_INNER;
    }
    if scope == "outer" {
        return GAPS_OUTER;
    }
    if scope == "vertical" {
        return GAPS_VERTICAL;
    }
    if scope == "horizontal" {
        return GAPS_HORIZONTAL;
    }
    if scope == "top" {
        return GAPS_TOP;
    }
    if scope == "right" {
        return GAPS_RIGHT;
    }
    if scope == "bottom" {
        return GAPS_BOTTOM;
    }
    if scope == "left" {
        return GAPS_LEFT;
    }
    // C: ELOG("Invalid command, cannot process scope %s", scope);
    crate::log::errorlog(&format!("Invalid command, cannot process scope {}", scope));
    GapsMask(0)
}

/// C: src/config_directives.c:812 bar_configure_binding
fn bar_configure_binding(d: &mut ConfigDirectives, button: &str, release: Option<&str>, command: &str) {
    if !strncasecmp_prefix(button, "button") {
        // C: ELOG("Bindings for a bar can only be mouse bindings, not \"%s\", ignoring.\n", button);
        crate::log::errorlog(&format!(
            "Bindings for a bar can only be mouse bindings, not \"{}\", ignoring.\n",
            button
        ));
        return;
    }

    // C: const int input_code = atoi(button + strlen("button"));
    let input_code = atoi(&button["button".len()..]);
    if input_code < 1 {
        // C: ELOG("Button \"%s\" does not seem to be in format 'buttonX'.\n", button);
        crate::log::errorlog(&format!(
            "Button \"{}\" does not seem to be in format 'buttonX'.\n",
            button
        ));
        return;
    }
    let release_bool = release.is_some();

    let bar = d.bar();
    for current in &bar.bar_bindings {
        if current.input_code == input_code && current.release == release_bool {
            // C: ELOG("command for button %s was already specified, ignoring.\n", button);
            crate::log::errorlog(&format!(
                "command for button {} was already specified, ignoring.\n",
                button
            ));
            return;
        }
    }

    bar.bar_bindings.push_back(Barbinding {
        input_code,
        command: command.to_string(),
        release: release_bool,
    });
}

/// C: src/config_directives.c:71-77 (inside CFGFUN(include)) — on
/// PARSE_FILE_FAILED / PARSE_FILE_CONFIG_ERRORS the freshly added entry is
/// removed from the list and freed again.
fn remove_file_from_included_list(d: &mut ConfigDirectives, path: &str) {
    if let Some(pos) = d.included_files.iter().position(|file| file.path == path) {
        // C: TAILQ_REMOVE(&included_files, file, files); FREE(file->path);
        //    FREE(file->raw_contents); FREE(file->variable_replaced_contents); FREE(file);
        d.included_files.remove(pos);
    }
}

/// C: src/bindings.c:747 binding_same_key — returns true if a is a key
/// binding for the same key as b.
fn binding_same_key(a: &Binding, b: &Binding) -> bool {
    // Check if the input types are different
    if a.input_type != b.input_type {
        return false;
    }

    // Check if one is using keysym while the other is using bindsym.
    if (a.symbol.is_none() && b.symbol.is_some()) || (a.symbol.is_some() && b.symbol.is_none()) {
        return false;
    }

    // If a is NULL, b has to be NULL, too (see previous conditional). If the
    // keycodes differ, it can't be a duplicate. (C comment)
    if let (Some(sa), Some(sb)) = (&a.symbol, &b.symbol) {
        if strcasecmp(sa, sb) != 0 {
            return false;
        }
    }

    // Check if the keycodes or modifiers are different. If so, they can't
    // be duplicate (C comment)
    if a.keycode != b.keycode || a.event_state_mask != b.event_state_mask || a.release != b.release {
        return false;
    }

    true
}

/// C: src/bindings.c:784 check_for_duplicate_bindings — checks for
/// duplicate key bindings (the same keycode or keysym is configured more
/// than once). If a duplicate binding is found, a message is printed and
/// has_errors is set, which will start i3-nagbar. The C function iterates
/// the global `bindings` list — the currently active binding set, which is
/// the default mode's list during config parsing.
pub fn check_for_duplicate_bindings(d: &mut ConfigDirectives, context: &mut ParserContext) {
    let default_mode = match d.modes.iter().position(|m| m.name == DEFAULT_BINDING_MODE) {
        Some(pos) => pos,
        None => return,
    };
    let bindings = &d.modes[default_mode].bindings;
    for (current_idx, current) in bindings.iter().enumerate() {
        for bind in bindings.iter().take(current_idx) {
            // Abort when we reach the current keybinding, only check the
            // bindings before (C comment)
            if !binding_same_key(bind, current) {
                continue;
            }

            context.has_errors = true;
            if current.keycode != 0 {
                // C: ELOG("Duplicate keybinding in config file:\n  state mask 0x%x with keycode %d, command \"%s\"\n", ...);
                // ELOG adds the "ERROR: " prefix (C: include/log.h).
                crate::log::elog(&format!(
                    "Duplicate keybinding in config file:\n  state mask 0x{:x} with keycode {}, command \"{}\"\n",
                    current.event_state_mask, current.keycode, current.command
                ));
            } else {
                // C: ELOG("Duplicate keybinding in config file:\n  state mask 0x%x with keysym %s, command \"%s\"\n", ...);
                crate::log::elog(&format!(
                    "Duplicate keybinding in config file:\n  state mask 0x{:x} with keysym {}, command \"{}\"\n",
                    current.event_state_mask,
                    current.symbol.as_deref().unwrap_or("(null)"),
                    current.command
                ));
            }
        }
    }
}

impl ConfigHooks for ConfigDirectives {
    type Match = Match;

    /// C: src/config_directives.c:18-85 CFGFUN(include)
    fn cfg_include(
        &mut self,
        ctx: &mut ConfigParserCtx<Match>,
        result: &mut ConfigResultIr,
        pattern: Option<&str>,
    ) {
        // C: DLOG("include %s\n", pattern);
        // C: const int ret = wordexp(pattern, &p, 0);
        //    if (ret != 0) { ELOG("wordexp(%s): error %d\n", pattern, ret);
        //                    result->has_errors = true; return; }
        let words = match wordexp(req(pattern)) {
            Ok(words) => words,
            Err(ret) => {
                crate::log::errorlog(&format!(
                    "wordexp({}): error {}\n",
                    req(pattern),
                    ret
                ));
                result.has_errors = true;
                return;
            }
        };
        for w in &words {
            // C: if (realpath(w[i], resolved_path) == NULL) { LOG skip }
            let resolved = match std::fs::canonicalize(w) {
                Ok(path) => path.to_string_lossy().into_owned(),
                Err(err) => {
                    // C: LOG("Skipping %s: %s\n", w[i], strerror(errno));
                    crate::log::log_(&format!("Skipping {}: {}\n", w, err));
                    continue;
                }
            };

            let mut skip = false;
            for file in &self.included_files {
                if file.path == resolved {
                    skip = true;
                    break;
                }
            }
            if skip {
                // C: LOG("Skipping file %s (already included)\n", resolved_path);
                crate::log::log_(&format!("Skipping file {} (already included)\n", resolved));
                continue;
            }

            // C: LOG("Including config file %s\n", resolved_path);
            crate::log::log_(&format!("Including config file {}\n", resolved));

            // C: file = scalloc(1, sizeof(IncludedFile)); file->path = ...;
            //    TAILQ_INSERT_TAIL(&included_files, file, files);
            self.included_files.push_back(IncludedFile {
                path: resolved.clone(),
                ..Default::default()
            });

            // C: struct parser_ctx ctx = { .use_nagbar = ..., .variables = result->ctx->variables };
            // — the inner parser shares the outer variable list; the Rust
            // port moves the list into the inner ctx and restores it
            // afterwards (C copies the list head and writes it back at the
            // end of the switch).
            let variables = std::mem::take(&mut ctx.variables);
            let mut inner_ctx = ConfigParserCtx::<Match> {
                use_nagbar: ctx.use_nagbar,
                variables,
                ..Default::default()
            };
            let mut file = IncludedFile {
                path: resolved.clone(),
                ..Default::default()
            };
            let parse_result = crate::config::parse_file(&mut inner_ctx, self, &resolved, &mut file);
            // C: result->ctx->variables = ctx.variables; — the (possibly
            // extended) variable list is shared with the outer parser.
            ctx.variables = std::mem::take(&mut inner_ctx.variables);
            // C: the list entry and the parse_file argument are the same
            // object; the port keeps them separate and copies the contents.
            if let Some(pos) = self.included_files.iter().rposition(|f| f.path == resolved) {
                self.included_files[pos].raw_contents = file.raw_contents.clone();
                self.included_files[pos].variable_replaced_contents =
                    file.variable_replaced_contents.clone();
            }

            match parse_result {
                ParseFileResult::PARSE_FILE_SUCCESS => {}
                ParseFileResult::PARSE_FILE_FAILED => {
                    // C: ELOG("including config file %s: %s\n", resolved_path, strerror(errno));
                    crate::log::errorlog(&format!(
                        "including config file {}: {}\n",
                        resolved,
                        std::io::Error::last_os_error()
                    ));
                    // fallthrough
                    result.has_errors = true;
                    remove_file_from_included_list(self, &resolved);
                }
                ParseFileResult::PARSE_FILE_CONFIG_ERRORS => {
                    result.has_errors = true;
                    remove_file_from_included_list(self, &resolved);
                }
            }
        }
        // C: wordfree(&p); — automatic (the Vec drops).
    }

    /// C: src/config_directives.c:98 CFGFUN(criteria_init)
    fn cfg_criteria_init(
        &mut self,
        ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        _state: i32,
    ) {
        self.criteria_next_state = _state;

        // C: DLOG("Initializing criteria, current_match = %p, state = %d\n", ...);
        // C: match_free(current_match); match_init(current_match);
        match_free(&mut ctx.current_match);
        match_init(&mut ctx.current_match);
    }

    /// C: src/config_directives.c:106 CFGFUN(criteria_pop_state)
    fn cfg_criteria_pop_state(&mut self, _ctx: &mut ConfigParserCtx<Match>, result: &mut ConfigResultIr) {
        result.next_state = self.criteria_next_state;
    }

    /// C: src/config_directives.c:115 CFGFUN(criteria_add)
    fn cfg_criteria_add(
        &mut self,
        ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        ctype: Option<&str>,
        kind: Option<&str>,
    ) {
        // C: match_parse_property(current_match, ctype, cvalue);
        match_parse_property(&mut ctx.current_match, req(ctype), kind, None);
    }

    /// C: src/config_directives.c:173 CFGFUN(font)
    fn cfg_font(&mut self, _ctx: &mut ConfigParserCtx<Match>, _result: &mut ConfigResultIr, font: Option<&str>) {
        // C: config.font = load_font(font, true); set_font(&config.font);
        // String-keeping port: the pattern and the pango/xcb classification
        // are stored (TODO(X11 port): real font metrics via libi3/font.c and
        // set_font; 実描画は後).
        self.config.font = load_font(req(font), true);
    }

    /// C: src/config_directives.c:178 CFGFUN(binding)
    fn cfg_binding(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        bindtype: Option<&str>,
        modifiers: Option<&str>,
        key: Option<&str>,
        release: Option<&str>,
        border: Option<&str>,
        whole_window: Option<&str>,
        exclude_titlebar: Option<&str>,
        command: Option<&str>,
    ) {
        // C: configure_binding(bindtype, modifiers, key, release, border,
        //    whole_window, exclude_titlebar, command, DEFAULT_BINDING_MODE, false);
        self.configure_binding(
            req(bindtype),
            modifiers,
            req(key),
            release,
            border,
            whole_window,
            exclude_titlebar,
            req(command),
            DEFAULT_BINDING_MODE,
            false,
        );
    }

    /// C: src/config_directives.c:189 CFGFUN(mode_binding)
    fn cfg_mode_binding(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        bindtype: Option<&str>,
        modifiers: Option<&str>,
        key: Option<&str>,
        release: Option<&str>,
        border: Option<&str>,
        whole_window: Option<&str>,
        exclude_titlebar: Option<&str>,
        command: Option<&str>,
    ) {
        let current_mode = match self.current_mode.clone() {
            // C: if (current_mode == NULL) { return; } — when using an
            // invalid mode name, e.g. "default" (C comment)
            Some(mode) => mode,
            None => return,
        };
        self.configure_binding(
            req(bindtype),
            modifiers,
            req(key),
            release,
            border,
            whole_window,
            exclude_titlebar,
            req(command),
            &current_mode,
            self.current_mode_pango_markup,
        );
    }

    /// C: src/config_directives.c:198 CFGFUN(enter_mode)
    fn cfg_enter_mode(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        pango_markup: Option<&str>,
        modename: Option<&str>,
    ) {
        let modename = req(modename);
        if modename == DEFAULT_BINDING_MODE {
            // C: ELOG("You cannot use the name %s for your mode\n", DEFAULT_BINDING_MODE);
            crate::log::errorlog(&format!(
                "You cannot use the name {} for your mode\n",
                DEFAULT_BINDING_MODE
            ));
            return;
        }

        for mode in &self.modes {
            if mode.name == modename {
                // C: ELOG("The binding mode with name \"%s\" is defined at least twice.\n", modename);
                crate::log::errorlog(&format!(
                    "The binding mode with name \"{}\" is defined at least twice.\n",
                    modename
                ));
            }
        }

        // C: DLOG("\t now in mode %s\n", modename);
        self.current_mode = Some(modename.to_string());
        self.current_mode_pango_markup = pango_markup.is_some();
    }

    /// C: src/config_directives.c:217 CFGFUN(exec)
    fn cfg_exec(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        exectype: Option<&str>,
        no_startup_id: Option<&str>,
        command: Option<&str>,
    ) {
        let new = Autostart {
            command: req(command).to_string(),
            no_startup_id: no_startup_id.is_some(),
        };
        if req(exectype) == "exec" {
            self.autostarts.push_back(new);
        } else {
            self.autostarts_always.push_back(new);
        }
    }

    /// C: src/config_directives.c:228 CFGFUN(for_window)
    fn cfg_for_window(
        &mut self,
        ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        command: Option<&str>,
    ) {
        if let Some(error) = &ctx.current_match.error {
            // C: ELOG("match has error: %s\n", current_match->error);
            crate::log::errorlog(&format!("match has error: {}\n", error));
            return;
        }
        if match_is_empty(&ctx.current_match) {
            // C: ELOG("Match is empty, ignoring this for_window statement\n");
            crate::log::errorlog("Match is empty, ignoring this for_window statement\n");
            return;
        }
        // C: DLOG("\t should execute command %s for the criteria mentioned above\n", command);
        let mut assignment = Assignment::default();
        assignment.type_ = A_COMMAND;
        match_copy(&mut assignment.match_, &ctx.current_match);
        assignment.dest = Some(req(command).to_string());
        self.assignments.push_back(assignment);
    }

    /// C: src/config_directives.c:323 CFGFUN(gaps)
    fn cfg_gaps(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        workspace: Option<&str>,
        scope: Option<&str>,
        value: i64,
    ) {
        // C: const int pixels = logical_px(value); (long → int conversion)
        let pixels = logical_px(value as i32);
        let mask = gaps_scope_to_mask(req(scope));

        match workspace {
            None => apply_gaps(Some(&mut self.config.gaps), mask, pixels),
            Some(workspace) => create_gaps_assignment(self, workspace, mask, pixels),
        }
    }

    /// C: src/config_directives.c:334 CFGFUN(smart_borders)
    fn cfg_smart_borders(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        enable: Option<&str>,
    ) {
        let enable = req(enable);
        if enable == "no_gaps" {
            self.config.hide_edge_borders = HideEdgeBordersMode(crate::data::HEBM_SMART_NO_GAPS.0);
        } else if boolstr(enable) {
            if self.config.hide_edge_borders.0 == crate::data::HEBM_NONE.0 {
                // Only enable this if hide_edge_borders is at the default
                // value as it otherwise takes precedence (C comment)
                self.config.hide_edge_borders = HideEdgeBordersMode(crate::data::HEBM_SMART.0);
            } else {
                // C: ELOG("Both hide_edge_borders and smart_borders was used. Ignoring smart_borders as it is deprecated.\n");
                crate::log::errorlog(
                    "Both hide_edge_borders and smart_borders was used. Ignoring smart_borders as it is deprecated.\n",
                );
            }
        }
    }

    /// C: src/config_directives.c:348 CFGFUN(smart_gaps)
    fn cfg_smart_gaps(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        enabled: Option<&str>,
    ) {
        let enabled = req(enabled);
        if enabled == "inverse_outer" {
            self.config.smart_gaps = SmartGaps::SMART_GAPS_INVERSE_OUTER;
        } else {
            self.config.smart_gaps = if boolstr(enabled) {
                SmartGaps::SMART_GAPS_ON
            } else {
                SmartGaps::SMART_GAPS_OFF
            };
        }
    }

    /// C: src/config_directives.c:356 CFGFUN(floating_minimum_size)
    fn cfg_floating_minimum_size(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        width: i64,
        height: i64,
    ) {
        self.config.floating_minimum_width = width as i32;
        self.config.floating_minimum_height = height as i32;
    }

    /// C: src/config_directives.c:361 CFGFUN(floating_maximum_size)
    fn cfg_floating_maximum_size(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        width: i64,
        height: i64,
    ) {
        self.config.floating_maximum_width = width as i32;
        self.config.floating_maximum_height = height as i32;
    }

    /// C: src/config_directives.c:366 CFGFUN(floating_modifier)
    fn cfg_floating_modifier(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        modifiers: Option<&str>,
    ) {
        self.config.floating_modifier = event_state_from_str(modifiers);
    }

    /// C: src/config_directives.c:370 CFGFUN(tiling_drag_swap_modifier)
    fn cfg_tiling_drag_swap_modifier(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        modifiers: Option<&str>,
    ) {
        self.config.swap_modifier = event_state_from_str(modifiers);
    }

    /// C: src/config_directives.c:374 CFGFUN(default_orientation)
    fn cfg_default_orientation(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        orientation: Option<&str>,
    ) {
        let orientation = req(orientation);
        self.config.default_orientation = if orientation == "horizontal" {
            Orientation::HORIZ
        } else if orientation == "vertical" {
            Orientation::VERT
        } else {
            Orientation::NO_ORIENTATION
        };
    }

    /// C: src/config_directives.c:384 CFGFUN(workspace_layout)
    fn cfg_workspace_layout(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        layout: Option<&str>,
    ) {
        let layout = req(layout);
        self.config.default_layout = if layout == "default" {
            Layout::L_DEFAULT
        } else if layout == "stacking" || layout == "stacked" {
            Layout::L_STACKED
        } else {
            Layout::L_TABBED
        };
    }

    /// C: src/config_directives.c:395 CFGFUN(default_border)
    fn cfg_default_border(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        windowtype: Option<&str>,
        border: Option<&str>,
        width: i64,
    ) {
        let border = req(border);
        let (border_style, border_width) = if border == "1pixel" {
            (BorderStyle::BS_PIXEL, 1)
        } else if border == "none" {
            (BorderStyle::BS_NONE, 0)
        } else if border == "pixel" {
            (BorderStyle::BS_PIXEL, width as i32)
        } else {
            (BorderStyle::BS_NORMAL, width as i32)
        };

        let windowtype = req(windowtype);
        if windowtype == "default_border" || windowtype == "new_window" {
            // C: DLOG("default tiled border style = %d and border width = %d ...");
            self.config.default_border = border_style;
            self.config.default_border_width = logical_px(border_width);
        } else {
            // C: DLOG("default floating border style = %d and border width = %d ...");
            self.config.default_floating_border = border_style;
            self.config.default_floating_border_width = logical_px(border_width);
        }
    }

    /// C: src/config_directives.c:427 CFGFUN(hide_edge_borders)
    fn cfg_hide_edge_borders(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        borders: Option<&str>,
    ) {
        let borders = req(borders);
        self.config.hide_edge_borders = if borders == "smart_no_gaps" {
            HideEdgeBordersMode(crate::data::HEBM_SMART_NO_GAPS.0)
        } else if borders == "smart" {
            HideEdgeBordersMode(crate::data::HEBM_SMART.0)
        } else if borders == "vertical" {
            HideEdgeBordersMode(crate::data::HEBM_VERTICAL.0)
        } else if borders == "horizontal" {
            HideEdgeBordersMode(crate::data::HEBM_HORIZONTAL.0)
        } else if borders == "both" {
            HideEdgeBordersMode(crate::data::HEBM_BOTH.0)
        } else if borders == "none" {
            HideEdgeBordersMode(crate::data::HEBM_NONE.0)
        } else if boolstr(borders) {
            HideEdgeBordersMode(crate::data::HEBM_VERTICAL.0)
        } else {
            HideEdgeBordersMode(crate::data::HEBM_NONE.0)
        };
    }

    /// C: src/config_directives.c:447 CFGFUN(focus_follows_mouse)
    fn cfg_focus_follows_mouse(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.config.disable_focus_follows_mouse = !boolstr(req(value));
    }

    /// C: src/config_directives.c:451 CFGFUN(mouse_warping)
    fn cfg_mouse_warping(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        let value = req(value);
        if value == "none" {
            self.config.mouse_warping = Warping::POINTER_WARPING_NONE;
        } else if value == "output" {
            self.config.mouse_warping = Warping::POINTER_WARPING_OUTPUT;
        }
    }

    /// C: src/config_directives.c:459 CFGFUN(force_xinerama)
    fn cfg_force_xinerama(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.config.force_xinerama = boolstr(req(value));
    }

    /// C: src/config_directives.c:463 CFGFUN(disable_randr15)
    fn cfg_disable_randr15(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.config.disable_randr15 = boolstr(req(value));
    }

    /// C: src/config_directives.c:467 CFGFUN(focus_wrapping)
    fn cfg_focus_wrapping(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        let value = req(value);
        self.config.focus_wrapping = if value == "force" {
            FocusWrapping::FOCUS_WRAPPING_FORCE
        } else if value == "workspace" {
            FocusWrapping::FOCUS_WRAPPING_WORKSPACE
        } else if boolstr(value) {
            FocusWrapping::FOCUS_WRAPPING_ON
        } else {
            FocusWrapping::FOCUS_WRAPPING_OFF
        };
    }

    /// C: src/config_directives.c:479 CFGFUN(force_focus_wrapping)
    fn cfg_force_focus_wrapping(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        // C: Legacy syntax.
        if boolstr(req(value)) {
            self.config.focus_wrapping = FocusWrapping::FOCUS_WRAPPING_FORCE;
        } else {
            // C: For "force_focus_wrapping off", don't enable or disable
            // focus wrapping, just ensure it's not forced.
            if self.config.focus_wrapping == FocusWrapping::FOCUS_WRAPPING_FORCE {
                self.config.focus_wrapping = FocusWrapping::FOCUS_WRAPPING_ON;
            }
        }
    }

    /// C: src/config_directives.c:492 CFGFUN(workspace_back_and_forth)
    fn cfg_workspace_back_and_forth(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.config.workspace_auto_back_and_forth = boolstr(req(value));
    }

    /// C: src/config_directives.c:496 CFGFUN(fake_outputs)
    fn cfg_fake_outputs(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        outputs: Option<&str>,
    ) {
        // C: free(config.fake_outputs); config.fake_outputs = sstrdup(outputs);
        self.config.fake_outputs = Some(req(outputs).to_string());
    }

    /// C: src/config_directives.c:501 CFGFUN(force_display_urgency_hint)
    fn cfg_force_display_urgency_hint(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        duration_ms: i64,
    ) {
        self.config.workspace_urgency_timer = duration_ms as f32 / 1000.0;
    }

    /// C: src/config_directives.c:505 CFGFUN(focus_on_window_activation)
    fn cfg_focus_on_window_activation(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        mode: Option<&str>,
    ) {
        let mode = req(mode);
        if mode == "smart" {
            self.config.focus_on_window_activation = focus_on_window_activation_t::FOWA_SMART;
        } else if mode == "urgent" {
            self.config.focus_on_window_activation = focus_on_window_activation_t::FOWA_URGENT;
        } else if mode == "focus" {
            self.config.focus_on_window_activation = focus_on_window_activation_t::FOWA_FOCUS;
        } else if mode == "none" {
            self.config.focus_on_window_activation = focus_on_window_activation_t::FOWA_NONE;
        } else {
            // C: ELOG("Unknown focus_on_window_activation mode \"%s\", ignoring it.\n", mode);
            crate::log::errorlog(&format!(
                "Unknown focus_on_window_activation mode \"{}\", ignoring it.\n",
                mode
            ));
            return;
        }

        // C: DLOG("Set new focus_on_window_activation mode = %i.\n", ...);
        crate::log::dlog_at(
            file!(),
            "cfg_focus_on_window_activation",
            line!(),
            &format!(
                "Set new focus_on_window_activation mode = {}.\n",
                self.config.focus_on_window_activation as i32
            ),
        );
    }

    /// C: src/config_directives.c:522 CFGFUN(title_align)
    fn cfg_title_align(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        alignment: Option<&str>,
    ) {
        let alignment = req(alignment);
        if alignment == "left" {
            self.config.title_align = title_align_t::ALIGN_LEFT;
        } else if alignment == "center" {
            self.config.title_align = title_align_t::ALIGN_CENTER;
        } else if alignment == "right" {
            self.config.title_align = title_align_t::ALIGN_RIGHT;
        } else {
            // C: assert(false);
            panic!("title_align: invalid alignment \"{}\"", alignment);
        }
    }

    /// C: src/config_directives.c:534 CFGFUN(show_marks)
    fn cfg_show_marks(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.config.show_marks = boolstr(req(value));
    }

    /// C: src/config_directives.c:540 CFGFUN(workspace)
    fn cfg_workspace(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        workspace: Option<&str>,
        output: Option<&str>,
    ) {
        // C: When a new workspace line is encountered, for the first output
        // word, $workspace from the config.spec is non-NULL. Afterward, the
        // parser calls parser_clear_stack() because of the call line. Thus, we
        // have to preserve the workspace string.
        let workspace = match workspace {
            Some(workspace) => {
                // C: FREE(current_workspace); — the C code clears it *before*
                // the duplicate check, so a duplicate line also forgets the
                // workspace and the following words are silently dropped.
                self.current_workspace = None;
                for assignment in &self.ws_assignments {
                    if strcasecmp(&assignment.name, workspace) == 0 && assignment.output.is_some() {
                        // C: ELOG("You have a duplicate workspace assignment for workspace \"%s\"\n", workspace);
                        crate::log::errorlog(&format!(
                            "You have a duplicate workspace assignment for workspace \"{}\"\n",
                            workspace
                        ));
                        return;
                    }
                }

                self.current_workspace = Some(workspace.to_string());
                workspace.to_string()
            }
            None => match self.current_workspace.clone() {
                Some(current_workspace) => current_workspace,
                None => {
                    // C: DLOG("Both workspace and current_workspace are NULL, assuming we had an error before\n");
                    return;
                }
            },
        };

        // C: DLOG("Assigning workspace \"%s\" to output \"%s\"\n", workspace, output);
        self.ws_assignments.push_back(Workspace_Assignment {
            name: workspace,
            output: Some(req(output).to_string()),
            ..Default::default()
        });
    }

    /// C: src/config_directives.c:577 CFGFUN(ipc_socket)
    fn cfg_ipc_socket(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        path: Option<&str>,
    ) {
        // C: free(config.ipc_socket_path); config.ipc_socket_path = sstrdup(path);
        self.config.ipc_socket_path = Some(req(path).to_string());
    }

    /// C: src/config_directives.c:582 CFGFUN(restart_state)
    fn cfg_restart_state(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        path: Option<&str>,
    ) {
        // C: free(config.restart_state_path); config.restart_state_path = sstrdup(path);
        self.config.restart_state_path = Some(req(path).to_string());
    }

    /// C: src/config_directives.c:587 CFGFUN(popup_during_fullscreen)
    fn cfg_popup_during_fullscreen(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        let value = req(value);
        self.config.popup_during_fullscreen = if value == "ignore" {
            popup_during_fullscreen_t::PDF_IGNORE
        } else if value == "leave_fullscreen" {
            popup_during_fullscreen_t::PDF_LEAVE_FULLSCREEN
        } else if value == "all" {
            popup_during_fullscreen_t::PDF_ALL
        } else {
            popup_during_fullscreen_t::PDF_SMART
        };
    }

    /// C: src/config_directives.c:599 CFGFUN(color_single)
    fn cfg_color_single(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        _colorclass: Option<&str>,
        color: Option<&str>,
    ) {
        // C: used for client.background only currently
        self.config.client.background = draw_util_hex_to_color(req(color));
    }

    /// C: src/config_directives.c:604 CFGFUN(color)
    fn cfg_color(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        colorclass: Option<&str>,
        border: Option<&str>,
        background: Option<&str>,
        text: Option<&str>,
        indicator: Option<&str>,
        child_border: Option<&str>,
    ) {
        let colorclass = req(colorclass);
        let border = draw_util_hex_to_color(req(border));
        let background = draw_util_hex_to_color(req(background));
        let text = draw_util_hex_to_color(req(text));
        let indicator = indicator.map(draw_util_hex_to_color);
        let child_border = child_border.map(draw_util_hex_to_color);

        // C: #define APPLY_COLORS(classname) — border/background/text always,
        // indicator and child_border only when given (child_border falls back
        // to background).
        let apply = |triple: &mut Colortriple| {
            triple.border = border;
            triple.background = background;
            triple.text = text;
            if let Some(indicator) = indicator {
                triple.indicator = indicator;
            }
            triple.child_border = match child_border {
                Some(child_border) => child_border,
                None => background,
            };
        };

        let client = &mut self.config.client;

        // C: APPLY_COLORS(focused_inactive);
        if colorclass == "client.focused_inactive" {
            apply(&mut client.focused_inactive);
            return;
        }
        // C: APPLY_COLORS(focused_tab_title); — this class also records that it
        // was configured and warns about the two colors it ignores.
        if colorclass == "client.focused_tab_title" {
            client.got_focused_tab_title = true;
            if indicator.is_some() || child_border.is_some() {
                // C: ELOG("indicator and child_border colors have no effect for client.focused_tab_title\n");
                crate::log::errorlog(
                    "indicator and child_border colors have no effect for client.focused_tab_title\n",
                );
            }
            apply(&mut client.focused_tab_title);
            return;
        }
        // C: APPLY_COLORS(focused);
        if colorclass == "client.focused" {
            apply(&mut client.focused);
            return;
        }
        // C: APPLY_COLORS(unfocused);
        if colorclass == "client.unfocused" {
            apply(&mut client.unfocused);
            return;
        }
        // C: APPLY_COLORS(urgent);
        if colorclass == "client.urgent" {
            apply(&mut client.urgent);
            return;
        }
        // C: APPLY_COLORS(placeholder);
        if colorclass == "client.placeholder" {
            apply(&mut client.placeholder);
        }
    }

    /// C: src/config_directives.c:639 CFGFUN(assign_output)
    fn cfg_assign_output(
        &mut self,
        ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        output: Option<&str>,
    ) {
        if let Some(error) = ctx.current_match.error.as_deref() {
            // C: ELOG("match has error: %s\n", current_match->error);
            crate::log::errorlog(&format!("match has error: {}\n", error));
            return;
        }
        if match_is_empty(&ctx.current_match) {
            // C: ELOG("Match is empty, ignoring this assignment\n");
            crate::log::errorlog("Match is empty, ignoring this assignment\n");
            return;
        }

        if ctx.current_match.window_mode != MatchWindowMode::WM_ANY {
            // C: ELOG("Assignments using window mode (floating/tiling) is not supported\n");
            crate::log::errorlog("Assignments using window mode (floating/tiling) is not supported\n");
            return;
        }

        // C: DLOG("New assignment, using above criteria, to output \"%s\".\n", output);
        let mut assignment = Assignment::default();
        match_copy(&mut assignment.match_, &ctx.current_match);
        assignment.type_ = A_TO_OUTPUT;
        assignment.dest = Some(req(output).to_string());
        self.assignments.push_back(assignment);
    }

    /// C: src/config_directives.c:662 CFGFUN(assign)
    fn cfg_assign(
        &mut self,
        ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        workspace: Option<&str>,
        is_number: i64,
    ) {
        if let Some(error) = ctx.current_match.error.as_deref() {
            // C: ELOG("match has error: %s\n", current_match->error);
            crate::log::errorlog(&format!("match has error: {}\n", error));
            return;
        }
        if match_is_empty(&ctx.current_match) {
            // C: ELOG("Match is empty, ignoring this assignment\n");
            crate::log::errorlog("Match is empty, ignoring this assignment\n");
            return;
        }

        if ctx.current_match.window_mode != MatchWindowMode::WM_ANY {
            // C: ELOG("Assignments using window mode (floating/tiling) is not supported\n");
            crate::log::errorlog("Assignments using window mode (floating/tiling) is not supported\n");
            return;
        }

        let is_number = is_number != 0;
        let workspace = req(workspace);
        if is_number && crate::util::ws_name_to_number(workspace) == -1 {
            // C: ELOG("Could not parse initial part of \"%s\" as a number.\n", workspace);
            crate::log::errorlog(&format!(
                "Could not parse initial part of \"{}\" as a number.\n",
                workspace
            ));
            return;
        }

        // C: DLOG("New assignment, using above criteria, to workspace \"%s\".\n", workspace);
        let mut assignment = Assignment::default();
        match_copy(&mut assignment.match_, &ctx.current_match);
        assignment.type_ = if is_number {
            A_TO_WORKSPACE_NUMBER
        } else {
            A_TO_WORKSPACE
        };
        assignment.dest = Some(workspace.to_string());
        self.assignments.push_back(assignment);
    }

    /// C: src/config_directives.c:690 CFGFUN(no_focus)
    fn cfg_no_focus(
        &mut self,
        ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
    ) {
        if let Some(error) = ctx.current_match.error.as_deref() {
            // C: ELOG("match has error: %s\n", current_match->error);
            crate::log::errorlog(&format!("match has error: {}\n", error));
            return;
        }
        if match_is_empty(&ctx.current_match) {
            // C: ELOG("Match is empty, ignoring this assignment\n");
            crate::log::errorlog("Match is empty, ignoring this assignment\n");
            return;
        }

        // C: DLOG("New assignment, using above criteria, to ignore focus on manage.\n");
        let mut assignment = Assignment::default();
        match_copy(&mut assignment.match_, &ctx.current_match);
        assignment.type_ = A_NO_FOCUS;
        self.assignments.push_back(assignment);
    }

    /// C: src/config_directives.c:707 CFGFUN(ipc_kill_timeout)
    fn cfg_ipc_kill_timeout(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        timeout: i64,
    ) {
        // C: ipc_set_kill_timeout(timeout_ms / 1000.0);
        self.ipc_kill_timeout = timeout as f64 / 1000.0;
    }

    /// C: src/config_directives.c:711 CFGFUN(tiling_drag)
    fn cfg_tiling_drag(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        let value = req(value);
        self.config.tiling_drag = if value == "modifier" {
            tiling_drag_t::TILING_DRAG_MODIFIER
        } else if value == "titlebar" {
            tiling_drag_t::TILING_DRAG_TITLEBAR
        } else if value == "modifier,titlebar" || value == "titlebar,modifier" {
            // C: Switch the above to strtok() or similar if we ever grow more options
            tiling_drag_t::TILING_DRAG_MODIFIER_OR_TITLEBAR
        } else {
            tiling_drag_t::TILING_DRAG_OFF
        };
    }

    /// C: src/config_directives.c:731 CFGFUN(bar_font)
    fn cfg_bar_font(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        font: Option<&str>,
    ) {
        // C: FREE(current_bar->font); current_bar->font = sstrdup(font);
        self.bar().font = Some(req(font).to_string());
    }

    /// C: src/config_directives.c:736 CFGFUN(bar_separator_symbol)
    fn cfg_bar_separator_symbol(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        separator: Option<&str>,
    ) {
        self.bar().separator_symbol = Some(req(separator).to_string());
    }

    /// C: src/config_directives.c:741 CFGFUN(bar_mode)
    fn cfg_bar_mode(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        mode: Option<&str>,
    ) {
        let mode = req(mode);
        self.bar().mode = if mode == "dock" {
            bar_mode_t::M_DOCK
        } else if mode == "hide" {
            bar_mode_t::M_HIDE
        } else {
            bar_mode_t::M_INVISIBLE
        };
    }

    /// C: src/config_directives.c:745 CFGFUN(bar_hidden_state)
    fn cfg_bar_hidden_state(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        hidden_state: Option<&str>,
    ) {
        self.bar().hidden_state = if req(hidden_state) == "hide" {
            bar_hidden_state_t::S_HIDE
        } else {
            bar_hidden_state_t::S_SHOW
        };
    }

    /// C: src/config_directives.c:749 CFGFUN(bar_id)
    fn cfg_bar_id(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        bar_id: Option<&str>,
    ) {
        self.bar().id = Some(req(bar_id).to_string());
    }

    /// C: src/config_directives.c:753 CFGFUN(bar_output)
    fn cfg_bar_output(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        output: Option<&str>,
    ) {
        let bar = self.bar();
        // C: current_bar->outputs = srealloc(...); outputs[num_outputs] = sstrdup(output); num_outputs = new_outputs;
        bar.outputs.push(req(output).to_string());
        bar.num_outputs = bar.outputs.len() as i32;
    }

    /// C: src/config_directives.c:760 CFGFUN(bar_verbose)
    fn cfg_bar_verbose(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.bar().verbose = boolstr(req(value));
    }

    /// C: src/config_directives.c:764 CFGFUN(bar_height)
    fn cfg_bar_height(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: i64,
    ) {
        self.bar().bar_height = value as u32;
    }

    /// C: src/config_directives.c:776 CFGFUN(bar_padding_one)
    fn cfg_bar_padding_one(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        top_or_all: i64,
    ) {
        let bar = self.bar();
        bar.padding.x = top_or_all as u32;
        bar.padding.y = top_or_all as u32;
        bar.padding.width = top_or_all as u32;
        bar.padding.height = top_or_all as u32;
        // C: dlog_padding();
        self.dlog_padding();
    }

    /// C: src/config_directives.c:784 CFGFUN(bar_padding_two)
    fn cfg_bar_padding_two(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        top_or_all: i64,
        right_or_right_and_left: i64,
    ) {
        let bar = self.bar();
        bar.padding.x = right_or_right_and_left as u32;
        bar.padding.y = top_or_all as u32;
        bar.padding.width = right_or_right_and_left as u32;
        bar.padding.height = top_or_all as u32;
        // C: dlog_padding();
        self.dlog_padding();
    }

    /// C: src/config_directives.c:792 CFGFUN(bar_padding_three)
    fn cfg_bar_padding_three(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        top_or_all: i64,
        right_or_right_and_left: i64,
        bottom: i64,
    ) {
        let bar = self.bar();
        bar.padding.x = right_or_right_and_left as u32;
        bar.padding.y = top_or_all as u32;
        bar.padding.width = right_or_right_and_left as u32;
        bar.padding.height = bottom as u32;
        // C: dlog_padding();
        self.dlog_padding();
    }

    /// C: src/config_directives.c:800 CFGFUN(bar_padding_four)
    fn cfg_bar_padding_four(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        top_or_all: i64,
        right_or_right_and_left: i64,
        bottom: i64,
        left: i64,
    ) {
        let bar = self.bar();
        bar.padding.x = left as u32;
        bar.padding.y = top_or_all as u32;
        bar.padding.width = right_or_right_and_left as u32;
        bar.padding.height = bottom as u32;
        // C: dlog_padding();
        self.dlog_padding();
    }

    /// C: src/config_directives.c:808 CFGFUN(bar_modifier)
    fn cfg_bar_modifier(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        modifiers: Option<&str>,
    ) {
        // C: current_bar->modifier = modifiers ? event_state_from_str(modifiers) : XCB_NONE;
        self.bar().modifier = if modifiers.is_some() {
            event_state_from_str(modifiers)
        } else {
            XCB_NONE
        };
    }

    /// C: src/config_directives.c:840 CFGFUN(bar_wheel_up_cmd)
    fn cfg_bar_wheel_up_cmd(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        command: Option<&str>,
    ) {
        let command = req(command);
        // C: ELOG("'wheel_up_cmd' is deprecated. Please use 'bindsym button4 %s' instead.\n", command);
        crate::log::errorlog(&format!(
            "'wheel_up_cmd' is deprecated. Please use 'bindsym button4 {}' instead.\n",
            command
        ));
        // C: bar_configure_binding("button4", NULL, command);
        bar_configure_binding(self, "button4", None, command);
    }

    /// C: src/config_directives.c:845 CFGFUN(bar_wheel_down_cmd)
    fn cfg_bar_wheel_down_cmd(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        command: Option<&str>,
    ) {
        let command = req(command);
        // C: ELOG("'wheel_down_cmd' is deprecated. Please use 'bindsym button5 %s' instead.\n", command);
        crate::log::errorlog(&format!(
            "'wheel_down_cmd' is deprecated. Please use 'bindsym button5 {}' instead.\n",
            command
        ));
        // C: bar_configure_binding("button5", NULL, command);
        bar_configure_binding(self, "button5", None, command);
    }

    /// C: src/config_directives.c:850 CFGFUN(bar_bindsym)
    fn cfg_bar_bindsym(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        button: Option<&str>,
        release: Option<&str>,
        command: Option<&str>,
    ) {
        bar_configure_binding(self, req(button), release, req(command));
    }

    /// C: src/config_directives.c:854 CFGFUN(bar_position)
    fn cfg_bar_position(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        position: Option<&str>,
    ) {
        self.bar().position = if req(position) == "top" {
            bar_position_t::P_TOP
        } else {
            bar_position_t::P_BOTTOM
        };
    }

    /// C: src/config_directives.c:858 CFGFUN(bar_i3bar_command)
    fn cfg_bar_i3bar_command(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        command: Option<&str>,
    ) {
        // C: FREE(current_bar->i3bar_command); i3bar_command = sstrdup(command);
        self.bar().i3bar_command = Some(req(command).to_string());
    }

    /// C: src/config_directives.c:863 CFGFUN(bar_color)
    fn cfg_bar_color(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        colorclass: Option<&str>,
        border: Option<&str>,
        background: Option<&str>,
        text: Option<&str>,
    ) {
        let colorclass = req(colorclass);
        let border = req(border).to_string();
        let background = req(background).to_string();

        // C: #define APPLY_COLORS(classname) — new syntax (border, background,
        // text) when text is non-NULL, old syntax (text, background) otherwise.
        let apply = |border_dst: &mut Option<String>,
                     background_dst: &mut Option<String>,
                     text_dst: &mut Option<String>| match text {
            Some(text) => {
                *border_dst = Some(border.clone());
                *background_dst = Some(background.clone());
                *text_dst = Some(text.to_string());
            }
            None => {
                *background_dst = Some(background.clone());
                *text_dst = Some(border.clone());
            }
        };

        let colors = &mut self.bar().colors;
        // C: APPLY_COLORS(focused_workspace);
        if colorclass == "focused_workspace" {
            apply(
                &mut colors.focused_workspace_border,
                &mut colors.focused_workspace_bg,
                &mut colors.focused_workspace_text,
            );
        }
        // C: APPLY_COLORS(active_workspace);
        if colorclass == "active_workspace" {
            apply(
                &mut colors.active_workspace_border,
                &mut colors.active_workspace_bg,
                &mut colors.active_workspace_text,
            );
        }
        // C: APPLY_COLORS(inactive_workspace);
        if colorclass == "inactive_workspace" {
            apply(
                &mut colors.inactive_workspace_border,
                &mut colors.inactive_workspace_bg,
                &mut colors.inactive_workspace_text,
            );
        }
        // C: APPLY_COLORS(urgent_workspace);
        if colorclass == "urgent_workspace" {
            apply(
                &mut colors.urgent_workspace_border,
                &mut colors.urgent_workspace_bg,
                &mut colors.urgent_workspace_text,
            );
        }
        // C: APPLY_COLORS(binding_mode);
        if colorclass == "binding_mode" {
            apply(
                &mut colors.binding_mode_border,
                &mut colors.binding_mode_bg,
                &mut colors.binding_mode_text,
            );
        }
    }

    /// C: src/config_directives.c:889 CFGFUN(bar_socket_path)
    fn cfg_bar_socket_path(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        path: Option<&str>,
    ) {
        // C: FREE(current_bar->socket_path); socket_path = sstrdup(socket_path);
        self.bar().socket_path = Some(req(path).to_string());
    }

    /// C: src/config_directives.c:894 CFGFUN(bar_tray_output)
    fn cfg_bar_tray_output(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        output: Option<&str>,
    ) {
        // C: struct tray_output_t *tray_output = scalloc(...); TAILQ_INSERT_TAIL(&(current_bar->tray_outputs), ...);
        self.bar().tray_outputs.push_back(tray_output_t {
            output: req(output).to_string(),
        });
    }

    /// C: src/config_directives.c:900 CFGFUN(bar_tray_padding)
    fn cfg_bar_tray_padding(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        padding_px: i64,
    ) {
        self.bar().tray_padding = padding_px as i32;
    }

    /// C: src/config_directives.c:904 CFGFUN(bar_color_single)
    fn cfg_bar_color_single(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        colorclass: Option<&str>,
        color: Option<&str>,
    ) {
        let colorclass = req(colorclass);
        let color = req(color).to_string();
        let colors = &mut self.bar().colors;
        if colorclass == "background" {
            colors.background = Some(color);
        } else if colorclass == "separator" {
            colors.separator = Some(color);
        } else if colorclass == "statusline" {
            colors.statusline = Some(color);
        } else if colorclass == "focused_background" {
            colors.focused_background = Some(color);
        } else if colorclass == "focused_separator" {
            colors.focused_separator = Some(color);
        } else {
            // C: the else branch handles focused_statusline.
            colors.focused_statusline = Some(color);
        }
    }

    /// C: src/config_directives.c:920 CFGFUN(bar_status_command)
    fn cfg_bar_status_command(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        command: Option<&str>,
    ) {
        // C: FREE(current_bar->status_command); status_command = sstrdup(command);
        self.bar().status_command = Some(req(command).to_string());
    }

    /// C: src/config_directives.c:925 CFGFUN(bar_workspace_command)
    fn cfg_bar_workspace_command(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        command: Option<&str>,
    ) {
        // C: FREE(current_bar->workspace_command); workspace_command = sstrdup(command);
        self.bar().workspace_command = Some(req(command).to_string());
    }

    /// C: src/config_directives.c:930 CFGFUN(bar_binding_mode_indicator)
    fn cfg_bar_binding_mode_indicator(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.bar().hide_binding_mode_indicator = !boolstr(req(value));
    }

    /// C: src/config_directives.c:934 CFGFUN(bar_workspace_buttons)
    fn cfg_bar_workspace_buttons(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.bar().hide_workspace_buttons = !boolstr(req(value));
    }

    /// C: src/config_directives.c:938 CFGFUN(bar_workspace_min_width)
    fn cfg_bar_workspace_min_width(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        width: i64,
    ) {
        self.bar().workspace_min_width = width as i32;
    }

    /// C: src/config_directives.c:942 CFGFUN(bar_strip_workspace_numbers)
    fn cfg_bar_strip_workspace_numbers(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.bar().strip_workspace_numbers = boolstr(req(value));
    }

    /// C: src/config_directives.c:946 CFGFUN(bar_strip_workspace_name)
    fn cfg_bar_strip_workspace_name(
        &mut self,
        _ctx: &mut ConfigParserCtx<Match>,
        _result: &mut ConfigResultIr,
        value: Option<&str>,
    ) {
        self.bar().strip_workspace_name = boolstr(req(value));
    }

    /// C: src/config_directives.c:950 CFGFUN(bar_start)
    fn cfg_bar_start(&mut self, _ctx: &mut ConfigParserCtx<Match>, _result: &mut ConfigResultIr) {
        // C: current_bar = scalloc(1, sizeof(Barconfig));
        //    TAILQ_INIT(&(current_bar->bar_bindings));
        //    TAILQ_INIT(&(current_bar->tray_outputs));
        //    current_bar->tray_padding = 2;
        //    current_bar->modifier = XCB_KEY_BUT_MASK_MOD_4;
        let mut current_bar = Barconfig::default();
        current_bar.tray_padding = 2;
        current_bar.modifier = XCB_KEY_BUT_MASK_MOD_4;
        self.current_bar = Some(current_bar);
    }

    /// C: src/config_directives.c:958 CFGFUN(bar_finish)
    fn cfg_bar_finish(&mut self, _ctx: &mut ConfigParserCtx<Match>, _result: &mut ConfigResultIr) {
        // C: DLOG("\t new bar configuration finished, saving.\n");
        crate::log::dlog_at(
            file!(),
            "cfg_bar_finish",
            line!(),
            "\t new bar configuration finished, saving.\n",
        );
        // C: Simply reset the pointer, but don't free the resources.
        let mut current_bar = self
            .current_bar
            .take()
            .expect("BUG: bar_finish outside a bar block");

        // C: Generate a unique ID for this bar if not already configured.
        if current_bar.id.is_none() {
            current_bar.id = Some(format!("bar-{}", self.config.number_barconfigs));
        }

        // C: config.number_barconfigs++;
        self.config.number_barconfigs += 1;

        // C: TAILQ_INSERT_TAIL(&barconfigs, current_bar, configs);
        self.barconfigs.push_back(current_bar);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh state plus the (ctx, result) pair the hooks need: C keeps the
    /// parser state in file statics, the port passes it in explicitly
    /// (C: src/config_parser.c:81-94).
    fn parse_state() -> (ConfigDirectives, ConfigParserCtx<Match>, ConfigResultIr) {
        (
            ConfigDirectives::new(),
            ConfigParserCtx::<Match>::default(),
            ConfigResultIr::default(),
        )
    }

    /// The binding list of the given mode, or None when the mode was never
    /// created.
    fn mode_bindings<'a>(
        d: &'a ConfigDirectives,
        mode: &str,
    ) -> Option<&'a VecDeque<Binding>> {
        d.modes.iter().find(|m| m.name == mode).map(|m| &m.bindings)
    }

    /// The colorpixel a client.* color directive must have produced
    /// (C: libi3/draw_util.c:163 draw_util_hex_to_color).
    fn colorpixel(hex: &str) -> u32 {
        draw_util_hex_to_color(hex).colorpixel
    }

    /// C: src/config_directives.c:173 CFGFUN(font) — the pattern is stored and
    /// classified: pango:/xft: select pango, everything else xcb (C:
    /// libi3/font.c:159 load_font).
    #[test]
    fn cfg_font_stores_the_pattern_and_selects_the_backend() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_font(&mut ctx, &mut r, Some("pango:DejaVu Sans Mono 10"));
        assert_eq!(d.config.font.pattern, "pango:DejaVu Sans Mono 10");
        assert_eq!(d.config.font.r#type, libi3::font::FONT_TYPE_PANGO);

        // C: xft: is the deprecated alias of pango:.
        d.cfg_font(&mut ctx, &mut r, Some("xft:Fixed 9"));
        assert_eq!(d.config.font.r#type, libi3::font::FONT_TYPE_PANGO);

        d.cfg_font(&mut ctx, &mut r, Some("fixed"));
        assert_eq!(d.config.font.pattern, "fixed");
        assert_eq!(d.config.font.r#type, libi3::font::FONT_TYPE_XCB);
    }

    /// C: src/config_directives.c:178 CFGFUN(binding) → src/bindings.c:58
    /// configure_binding — bindsym stores the symbol, the modifier mask and the
    /// optional release/border/whole_window/exclude_titlebar flags, and
    /// "buttonN" makes it a mouse binding.
    #[test]
    fn cfg_binding_stores_symbols_modifiers_and_flags() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_binding(
            &mut ctx,
            &mut r,
            Some("bindsym"),
            Some("Mod4+Shift"),
            Some("m"),
            None,
            None,
            None,
            None,
            Some("move left"),
        );
        let bindings = mode_bindings(&d, DEFAULT_BINDING_MODE).expect("default mode");
        assert_eq!(bindings.len(), 1);
        let bind = &bindings[0];
        assert_eq!(bind.input_type, InputType::B_KEYBOARD);
        assert_eq!(bind.symbol.as_deref(), Some("m"));
        assert_eq!(bind.command, "move left");
        assert_eq!(
            bind.event_state_mask,
            XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_SHIFT
        );
        assert_eq!(bind.release, BindingRelease::B_UPON_KEYPRESS);
        assert!(!bind.border && !bind.whole_window && !bind.exclude_titlebar);

        // C: strncasecmp(input_code, "button", 6) == 0 → B_MOUSE.
        d.cfg_binding(
            &mut ctx,
            &mut r,
            Some("bindsym"),
            None,
            Some("button1"),
            None,
            None,
            None,
            None,
            Some("focus right"),
        );
        // The flags are "non-NULL means set" in C.
        d.cfg_binding(
            &mut ctx,
            &mut r,
            Some("bindsym"),
            Some("Mod1"),
            Some("x"),
            Some("release"),
            Some("border"),
            Some("whole_window"),
            Some("exclude_titlebar"),
            Some("kill"),
        );
        let bindings = mode_bindings(&d, DEFAULT_BINDING_MODE).unwrap();
        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[1].input_type, InputType::B_MOUSE);
        assert_eq!(bindings[1].symbol.as_deref(), Some("button1"));
        assert_eq!(bindings[2].release, BindingRelease::B_UPON_KEYRELEASE);
        assert!(bindings[2].border);
        assert!(bindings[2].whole_window);
        assert!(bindings[2].exclude_titlebar);
        assert_eq!(bindings[2].event_state_mask, XCB_KEY_BUT_MASK_MOD_1);
    }

    /// C: src/config_directives.c:178 + src/bindings.c:220-230 — bindcode
    /// parses the input code as a decimal keycode; an unparsable code drops the
    /// binding before the mode is even created (C: ELOG + FREE + return NULL).
    #[test]
    fn cfg_binding_bindcode_needs_a_decimal_input_code() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_binding(
            &mut ctx,
            &mut r,
            Some("bindcode"),
            Some("Mod4"),
            Some("not-a-keycode"),
            None,
            None,
            None,
            None,
            Some("nop"),
        );
        assert_eq!(d.modes.len(), 0, "an invalid keycode must not create a binding");

        d.cfg_binding(
            &mut ctx,
            &mut r,
            Some("bindcode"),
            Some("Mod4"),
            Some("44"),
            None,
            None,
            None,
            None,
            Some("nop"),
        );
        let bindings = mode_bindings(&d, DEFAULT_BINDING_MODE).expect("default mode");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].keycode, 44);
        assert_eq!(bindings[0].symbol, None);
        assert_eq!(bindings[0].input_type, InputType::B_KEYBOARD);
    }

    /// C: src/config_directives.c:189 CFGFUN(mode_binding) — a binding outside
    /// a mode block is dropped (current_mode == NULL, which is also the state
    /// after the rejected name "default").
    #[test]
    fn cfg_mode_binding_requires_an_open_mode_block() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_enter_mode(&mut ctx, &mut r, None, Some(DEFAULT_BINDING_MODE));
        d.cfg_mode_binding(
            &mut ctx,
            &mut r,
            Some("bindsym"),
            None,
            Some("Return"),
            None,
            None,
            None,
            None,
            Some("exec foo"),
        );
        assert_eq!(d.modes.len(), 0);

        d.cfg_enter_mode(&mut ctx, &mut r, Some("Pango"), Some("resize"));
        d.cfg_mode_binding(
            &mut ctx,
            &mut r,
            Some("bindsym"),
            None,
            Some("Return"),
            None,
            None,
            None,
            None,
            Some("exec foo"),
        );
        let bindings = mode_bindings(&d, "resize").expect("resize mode");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].command, "exec foo");
        // The pango_markup flag of the mode line is carried into
        // mode_from_name().
        assert!(d.modes.iter().any(|m| m.name == "resize" && m.pango_markup));
    }

    /// C: src/config_directives.c:198 CFGFUN(enter_mode) — "default" is
    /// reserved, and re-entering an existing mode only reports the duplicate:
    /// the bindings still land in the one mode object.
    #[test]
    fn cfg_enter_mode_keeps_the_existing_mode_object() {
        let (mut d, mut ctx, mut r) = parse_state();
        d.mode_from_name("resize", false);

        d.cfg_enter_mode(&mut ctx, &mut r, None, Some("resize"));
        assert!(!d.current_mode_pango_markup);
        d.cfg_mode_binding(
            &mut ctx,
            &mut r,
            Some("bindsym"),
            None,
            Some("j"),
            None,
            None,
            None,
            None,
            Some("resize grow width 10 px"),
        );

        assert_eq!(d.modes.len(), 1);
        assert_eq!(mode_bindings(&d, "resize").unwrap().len(), 1);
    }

    /// C: src/config_directives.c:540 CFGFUN(workspace) — the first word of the
    /// line names the workspace, every following output word reuses it through
    /// current_workspace, and a second assignment for the same workspace is
    /// rejected.
    #[test]
    fn cfg_workspace_assigns_outputs_and_rejects_duplicates() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_workspace(&mut ctx, &mut r, Some("1"), Some("DP-1"));
        d.cfg_workspace(&mut ctx, &mut r, None, Some("HDMI-1"));
        assert_eq!(d.ws_assignments.len(), 2);
        assert_eq!(d.ws_assignments[0].name, "1");
        assert_eq!(d.ws_assignments[0].output.as_deref(), Some("DP-1"));
        assert_eq!(d.ws_assignments[1].name, "1");
        assert_eq!(d.ws_assignments[1].output.as_deref(), Some("HDMI-1"));

        // Duplicate: C logs and returns before inserting the assignment.
        d.cfg_workspace(&mut ctx, &mut r, Some("1"), Some("eDP-1"));
        assert_eq!(d.ws_assignments.len(), 2);

        // A second workspace with its own output is fine.
        d.cfg_workspace(&mut ctx, &mut r, Some("2"), Some("eDP-1"));
        assert_eq!(d.ws_assignments.len(), 3);
        assert_eq!(d.ws_assignments[2].name, "2");
    }

    /// C: src/config_directives.c:323 CFGFUN(gaps) + :294 gaps_scope_to_mask —
    /// without a workspace the scope mask is applied to config.gaps; an unknown
    /// scope yields an empty mask and changes nothing.
    #[test]
    fn cfg_gaps_applies_the_scope_mask_to_the_global_gaps() {
        let (mut d, mut ctx, mut r) = parse_state();

        assert_eq!(d.config.gaps, crate::data::gaps_t::default());

        d.cfg_gaps(&mut ctx, &mut r, None, Some("inner"), 10);
        assert_eq!(d.config.gaps.inner, 10);
        assert_eq!(
            (d.config.gaps.top, d.config.gaps.right, d.config.gaps.bottom, d.config.gaps.left),
            (0, 0, 0, 0)
        );

        d.cfg_gaps(&mut ctx, &mut r, None, Some("outer"), 3);
        assert_eq!(
            (d.config.gaps.top, d.config.gaps.right, d.config.gaps.bottom, d.config.gaps.left),
            (3, 3, 3, 3)
        );
        assert_eq!(d.config.gaps.inner, 10, "outer must not touch the inner gap");

        d.cfg_gaps(&mut ctx, &mut r, None, Some("vertical"), 5);
        assert_eq!((d.config.gaps.top, d.config.gaps.bottom), (5, 5));
        assert_eq!((d.config.gaps.left, d.config.gaps.right), (3, 3));

        d.cfg_gaps(&mut ctx, &mut r, None, Some("left"), 7);
        assert_eq!(d.config.gaps.left, 7);
        assert_eq!(d.config.gaps.right, 3);

        // C: ELOG("Invalid command, cannot process scope %s") + return 0.
        d.cfg_gaps(&mut ctx, &mut r, None, Some("bogus"), 99);
        assert_eq!(
            (
                d.config.gaps.inner,
                d.config.gaps.top,
                d.config.gaps.right,
                d.config.gaps.bottom,
                d.config.gaps.left
            ),
            (10, 5, 3, 5, 7)
        );
    }

    /// C: src/config_directives.c:266 create_gaps_assignment + :323 — a
    /// "gaps workspace ..." line creates (or extends) the gaps part of the
    /// workspace assignment and leaves the output unset.
    #[test]
    fn cfg_gaps_creates_a_workspace_assignment() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_gaps(&mut ctx, &mut r, Some("2:web"), Some("inner"), 20);
        assert_eq!(d.ws_assignments.len(), 1);
        assert_eq!(d.ws_assignments[0].name, "2:web");
        assert_eq!(d.ws_assignments[0].output, None);
        assert_eq!(d.ws_assignments[0].gaps.inner, 20);
        assert_eq!(d.ws_assignments[0].gaps_mask, GAPS_INNER);

        d.cfg_gaps(&mut ctx, &mut r, Some("2:web"), Some("outer"), 8);
        assert_eq!(d.ws_assignments.len(), 1, "the existing assignment is reused");
        let assignment = &d.ws_assignments[0];
        assert_eq!(assignment.gaps_mask.0, GAPS_INNER.0 | GAPS_OUTER.0);
        assert_eq!(
            (
                assignment.gaps.top,
                assignment.gaps.right,
                assignment.gaps.bottom,
                assignment.gaps.left
            ),
            (8, 8, 8, 8)
        );
        assert_eq!(assignment.gaps.inner, 20);
    }

    /// C: src/config_directives.c:395 CFGFUN(default_border) — "1pixel" and
    /// "none" fix the width, "pixel" and "normal" take the configured one;
    /// the windowtype selects tiled versus floating.
    #[test]
    fn cfg_default_border_maps_the_style_and_the_width() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_default_border(&mut ctx, &mut r, Some("default_border"), Some("normal"), 3);
        assert_eq!(d.config.default_border, BorderStyle::BS_NORMAL);
        assert_eq!(d.config.default_border_width, 3);

        // C: new_window is the same tiled branch as default_border.
        d.cfg_default_border(&mut ctx, &mut r, Some("new_window"), Some("1pixel"), 3);
        assert_eq!(d.config.default_border, BorderStyle::BS_PIXEL);
        assert_eq!(d.config.default_border_width, 1);

        d.cfg_default_border(&mut ctx, &mut r, Some("new_window"), Some("none"), 3);
        assert_eq!(d.config.default_border, BorderStyle::BS_NONE);
        assert_eq!(d.config.default_border_width, 0);

        d.cfg_default_border(&mut ctx, &mut r, Some("new_window"), Some("pixel"), 5);
        assert_eq!(d.config.default_border, BorderStyle::BS_PIXEL);
        assert_eq!(d.config.default_border_width, 5);

        // Anything else is the floating branch (C: new_float).
        d.cfg_default_border(&mut ctx, &mut r, Some("new_float"), Some("normal"), 2);
        assert_eq!(d.config.default_floating_border, BorderStyle::BS_NORMAL);
        assert_eq!(d.config.default_floating_border_width, 2);
        assert_eq!(d.config.default_border, BorderStyle::BS_PIXEL);
    }

    /// C: src/config_directives.c:427 CFGFUN(hide_edge_borders) — the named
    /// modes map onto the HEBM_* bits, a generic boolean means vertical and
    /// everything else none.
    #[test]
    fn cfg_hide_edge_borders_maps_every_mode() {
        let (mut d, mut ctx, mut r) = parse_state();

        for (value, expected) in [
            ("smart_no_gaps", crate::data::HEBM_SMART_NO_GAPS),
            ("smart", crate::data::HEBM_SMART),
            ("vertical", crate::data::HEBM_VERTICAL),
            ("horizontal", crate::data::HEBM_HORIZONTAL),
            ("both", crate::data::HEBM_BOTH),
            ("none", crate::data::HEBM_NONE),
            ("yes", crate::data::HEBM_VERTICAL),
            ("off", crate::data::HEBM_NONE),
        ] {
            d.cfg_hide_edge_borders(&mut ctx, &mut r, Some(value));
            assert_eq!(d.config.hide_edge_borders, expected, "value {value}");
        }
    }

    /// C: src/config_directives.c:334 CFGFUN(smart_borders) — "no_gaps" always
    /// wins; a plain "yes" only sets the deprecated smart mode while
    /// hide_edge_borders is still at its default value.
    #[test]
    fn cfg_smart_borders_respects_hide_edge_borders() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_smart_borders(&mut ctx, &mut r, Some("no_gaps"));
        assert_eq!(d.config.hide_edge_borders, crate::data::HEBM_SMART_NO_GAPS);

        let (mut d, mut ctx, mut r) = parse_state();
        d.cfg_smart_borders(&mut ctx, &mut r, Some("yes"));
        assert_eq!(d.config.hide_edge_borders, crate::data::HEBM_SMART);

        // C: an explicit hide_edge_borders takes precedence (ELOG, no change).
        let (mut d, mut ctx, mut r) = parse_state();
        d.cfg_hide_edge_borders(&mut ctx, &mut r, Some("vertical"));
        d.cfg_smart_borders(&mut ctx, &mut r, Some("yes"));
        assert_eq!(d.config.hide_edge_borders, crate::data::HEBM_VERTICAL);
        d.cfg_smart_borders(&mut ctx, &mut r, Some("no"));
        assert_eq!(
            d.config.hide_edge_borders,
            crate::data::HEBM_VERTICAL,
            "smart_borders no must not reset hide_edge_borders"
        );
    }

    /// C: src/config_directives.c:522 CFGFUN(title_align) — left/center/right.
    #[test]
    fn cfg_title_align_maps_the_three_values() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_title_align(&mut ctx, &mut r, Some("center"));
        assert_eq!(d.config.title_align, title_align_t::ALIGN_CENTER);
        d.cfg_title_align(&mut ctx, &mut r, Some("right"));
        assert_eq!(d.config.title_align, title_align_t::ALIGN_RIGHT);
        d.cfg_title_align(&mut ctx, &mut r, Some("left"));
        assert_eq!(d.config.title_align, title_align_t::ALIGN_LEFT);
    }

    /// C: src/config_directives.c:534 CFGFUN(title_align) — the parser spec only
    /// produces the three valid words; anything else is C's assert(false).
    #[test]
    #[should_panic(expected = "title_align: invalid alignment")]
    fn cfg_title_align_panics_on_an_unknown_value() {
        let (mut d, mut ctx, mut r) = parse_state();
        d.cfg_title_align(&mut ctx, &mut r, Some("middle"));
    }

    /// C: src/config_directives.c:356/361 CFGFUN(floating_minimum_size) and
    /// (floating_maximum_size) — the values are stored as the given pixels.
    #[test]
    fn cfg_floating_sizes_are_stored_verbatim() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_floating_minimum_size(&mut ctx, &mut r, 75, 50);
        assert_eq!(d.config.floating_minimum_width, 75);
        assert_eq!(d.config.floating_minimum_height, 50);

        d.cfg_floating_maximum_size(&mut ctx, &mut r, -1, -1);
        assert_eq!(d.config.floating_maximum_width, -1);
        assert_eq!(d.config.floating_maximum_height, -1);
    }

    /// C: src/config_directives.c:366/370 CFGFUN(floating_modifier) and
    /// (tiling_drag_swap_modifier) — event_state_from_str() over the strings.
    #[test]
    fn cfg_modifiers_use_the_event_state_mask() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_floating_modifier(&mut ctx, &mut r, Some("Mod4"));
        assert_eq!(d.config.floating_modifier, XCB_KEY_BUT_MASK_MOD_4);

        d.cfg_tiling_drag_swap_modifier(&mut ctx, &mut r, Some("Mod1+Control"));
        assert_eq!(
            d.config.swap_modifier,
            XCB_KEY_BUT_MASK_MOD_1 | XCB_KEY_BUT_MASK_CONTROL
        );

        // C: a missing modifier argument is XCB_NONE (mask 0).
        d.cfg_floating_modifier(&mut ctx, &mut r, None);
        assert_eq!(d.config.floating_modifier, XCB_NONE);
    }

    /// C: src/config_directives.c:604 CFGFUN(color) — APPLY_COLORS sets
    /// border/background/text always, indicator/child_border only when given,
    /// and child_border falls back to the background.
    #[test]
    fn cfg_color_sets_the_client_colors() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_color(
            &mut ctx,
            &mut r,
            Some("client.focused"),
            Some("#ff0000"),
            Some("#00ff00"),
            Some("#0000ff"),
            None,
            None,
        );
        let focused = d.config.client.focused;
        assert_eq!(focused.border.colorpixel, colorpixel("#ff0000"));
        assert_eq!(focused.background.colorpixel, colorpixel("#00ff00"));
        assert_eq!(focused.text.colorpixel, colorpixel("#0000ff"));
        assert_eq!(
            focused.child_border.colorpixel,
            colorpixel("#00ff00"),
            "child_border falls back to the background"
        );
        assert_eq!(focused.indicator.colorpixel, 0, "indicator stays untouched");

        d.cfg_color(
            &mut ctx,
            &mut r,
            Some("client.focused"),
            Some("#111111"),
            Some("#222222"),
            Some("#333333"),
            Some("#444444"),
            Some("#555555"),
        );
        assert_eq!(
            d.config.client.focused.indicator.colorpixel,
            colorpixel("#444444")
        );
        assert_eq!(
            d.config.client.focused.child_border.colorpixel,
            colorpixel("#555555")
        );
    }

    /// C: src/config_directives.c:604 CFGFUN(color) — every class name is
    /// applied to its own triple; an unknown class changes nothing, and
    /// client.focused_tab_title records that it was configured.
    #[test]
    fn cfg_color_dispatches_every_color_class() {
        let (mut d, mut ctx, mut r) = parse_state();

        for class in [
            "client.focused_inactive",
            "client.unfocused",
            "client.urgent",
            "client.placeholder",
        ] {
            d.cfg_color(
                &mut ctx,
                &mut r,
                Some(class),
                Some("#010203"),
                Some("#040506"),
                Some("#070809"),
                None,
                None,
            );
        }
        for triple in [
            d.config.client.focused_inactive,
            d.config.client.unfocused,
            d.config.client.urgent,
            d.config.client.placeholder,
        ] {
            assert_eq!(triple.border.colorpixel, colorpixel("#010203"));
            assert_eq!(triple.background.colorpixel, colorpixel("#040506"));
            assert_eq!(triple.text.colorpixel, colorpixel("#070809"));
        }

        // Unknown class: C falls through the whole APPLY_COLORS list.
        let before = d.config.client.focused.text.colorpixel;
        d.cfg_color(
            &mut ctx,
            &mut r,
            Some("client.bogus"),
            Some("#ffffff"),
            Some("#ffffff"),
            Some("#ffffff"),
            None,
            None,
        );
        assert_eq!(d.config.client.focused.text.colorpixel, before);

        assert!(!d.config.client.got_focused_tab_title);
        d.cfg_color(
            &mut ctx,
            &mut r,
            Some("client.focused_tab_title"),
            Some("#aaaaaa"),
            Some("#bbbbbb"),
            Some("#cccccc"),
            None,
            None,
        );
        assert!(d.config.client.got_focused_tab_title);
        assert_eq!(
            d.config.client.focused_tab_title.background.colorpixel,
            colorpixel("#bbbbbb")
        );
    }

    /// C: src/config_directives.c:599 CFGFUN(color_single) — only
    /// client.background uses the single-color form.
    #[test]
    fn cfg_color_single_sets_the_client_background() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_color_single(&mut ctx, &mut r, Some("client.background"), Some("#123456"));
        assert_eq!(d.config.client.background.colorpixel, colorpixel("#123456"));
    }

    /// C: src/config_directives.c:98/106/115 — criteria_init resets the match
    /// (match_free + match_init), criteria_add fills it and criteria_pop_state
    /// restores the state the block was entered from.
    #[test]
    fn cfg_criteria_round_trip_through_the_match() {
        let (mut d, mut ctx, mut r) = parse_state();

        for state in [7, 3] {
            d.cfg_criteria_init(&mut ctx, &mut r, state);
            assert!(ctx.current_match.class.is_none(), "match_init wipes the criteria");
            assert_eq!(ctx.current_match.urgent, MatchUrgency::U_DONTCHECK);
            assert_eq!(ctx.current_match.window_mode, MatchWindowMode::WM_ANY);
            assert_eq!(ctx.current_match.window_type, u32::MAX);

            d.cfg_criteria_add(&mut ctx, &mut r, Some("class"), Some("Firefox"));
            assert_eq!(
                ctx.current_match.class.as_deref().map(|c| c.pattern.as_str()),
                Some("Firefox")
            );

            let mut result = ConfigResultIr::default();
            d.cfg_criteria_pop_state(&mut ctx, &mut result);
            assert_eq!(result.next_state, state);
        }
    }

    /// C: src/config_directives.c:115 CFGFUN(criteria_add) → src/match.c:295 —
    /// an unparsable id sets match->error, which makes the following
    /// assignment a no-op.
    #[test]
    fn cfg_criteria_error_blocks_the_assignment() {
        let (mut d, mut ctx, mut r) = parse_state();
        d.cfg_criteria_init(&mut ctx, &mut r, 0);
        d.cfg_criteria_add(&mut ctx, &mut r, Some("id"), Some("not-a-number"));
        assert_eq!(ctx.current_match.error.as_deref(), Some("invalid id"));

        d.cfg_assign(&mut ctx, &mut r, Some("2"), 0);
        assert_eq!(d.assignments.len(), 0);
    }

    /// C: src/config_directives.c:662 CFGFUN(assign) — a non-empty match without
    /// the floating/tiling window mode becomes A_TO_WORKSPACE, is_number makes
    /// it A_TO_WORKSPACE_NUMBER, and an unparsable number is rejected.
    #[test]
    fn cfg_assign_creates_workspace_assignments() {
        let (mut d, mut ctx, mut r) = parse_state();
        d.cfg_criteria_init(&mut ctx, &mut r, 0);
        d.cfg_criteria_add(&mut ctx, &mut r, Some("class"), Some("Firefox"));

        d.cfg_assign(&mut ctx, &mut r, Some("2:web"), 0);
        assert_eq!(d.assignments.len(), 1);
        assert_eq!(d.assignments[0].type_, A_TO_WORKSPACE);
        assert_eq!(d.assignments[0].dest.as_deref(), Some("2:web"));
        assert_eq!(
            d.assignments[0]
                .match_
                .class
                .as_deref()
                .map(|c| c.pattern.as_str()),
            Some("Firefox")
        );

        d.cfg_assign(&mut ctx, &mut r, Some("3"), 1);
        assert_eq!(d.assignments[1].type_, A_TO_WORKSPACE_NUMBER);

        // C: ws_name_to_number("abc") == -1 → ELOG + return.
        d.cfg_assign(&mut ctx, &mut r, Some("abc"), 1);
        assert_eq!(d.assignments.len(), 2);
    }

    /// C: src/config_directives.c:639/690 CFGFUN(assign_output) and (no_focus);
    /// both reject an empty match, and a window-mode criterion is unsupported
    /// for assignments.
    #[test]
    fn cfg_assign_output_and_no_focus_need_a_non_empty_match() {
        let (mut d, mut ctx, mut r) = parse_state();

        // Empty match (after match_init) → ignored.
        d.cfg_criteria_init(&mut ctx, &mut r, 0);
        d.cfg_assign_output(&mut ctx, &mut r, Some("DP-1"));
        d.cfg_no_focus(&mut ctx, &mut r);
        assert_eq!(d.assignments.len(), 0);

        d.cfg_criteria_add(&mut ctx, &mut r, Some("class"), Some("Gimp"));
        d.cfg_assign_output(&mut ctx, &mut r, Some("DP-1"));
        d.cfg_no_focus(&mut ctx, &mut r);
        assert_eq!(d.assignments.len(), 2);
        assert_eq!(d.assignments[0].type_, A_TO_OUTPUT);
        assert_eq!(d.assignments[0].dest.as_deref(), Some("DP-1"));
        assert_eq!(d.assignments[1].type_, A_NO_FOCUS);
        assert_eq!(d.assignments[1].dest, None);

        // C: window mode criteria (floating/tiling) are not supported for
        // assignments.
        d.cfg_criteria_init(&mut ctx, &mut r, 0);
        d.cfg_criteria_add(&mut ctx, &mut r, Some("floating"), None);
        d.cfg_assign(&mut ctx, &mut r, Some("4"), 0);
        d.cfg_assign_output(&mut ctx, &mut r, Some("DP-2"));
        assert_eq!(d.assignments.len(), 2);
    }

    /// C: src/config_directives.c:228 CFGFUN(for_window) — an empty match is
    /// rejected, a filled one becomes an A_COMMAND assignment.
    #[test]
    fn cfg_for_window_needs_a_non_empty_match() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_criteria_init(&mut ctx, &mut r, 0);
        d.cfg_for_window(&mut ctx, &mut r, Some("border none"));
        assert_eq!(d.assignments.len(), 0);

        d.cfg_criteria_add(&mut ctx, &mut r, Some("class"), Some("Gimp"));
        d.cfg_for_window(&mut ctx, &mut r, Some("border none"));
        assert_eq!(d.assignments.len(), 1);
        assert_eq!(d.assignments[0].type_, A_COMMAND);
        assert_eq!(d.assignments[0].dest.as_deref(), Some("border none"));
        assert_eq!(
            d.assignments[0]
                .match_
                .class
                .as_deref()
                .map(|c| c.pattern.as_str()),
            Some("Gimp")
        );
    }

    /// C: src/config_directives.c:217 CFGFUN(exec) — exectype selects the list,
    /// no_startup_id is the second argument.
    #[test]
    fn cfg_exec_fills_both_autostart_lists() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_exec(&mut ctx, &mut r, Some("exec"), None, Some("xterm"));
        d.cfg_exec(
            &mut ctx,
            &mut r,
            Some("exec_always"),
            Some("no_startup_id"),
            Some("nm-applet"),
        );

        assert_eq!(d.autostarts.len(), 1);
        assert_eq!(d.autostarts[0].command, "xterm");
        assert!(!d.autostarts[0].no_startup_id);

        assert_eq!(d.autostarts_always.len(), 1);
        assert_eq!(d.autostarts_always[0].command, "nm-applet");
        assert!(d.autostarts_always[0].no_startup_id);
    }

    /// C: src/config_directives.c:577/582/707 — the paths are stored and the
    /// ipc kill timeout is converted from milliseconds to seconds (C:
    /// ipc_set_kill_timeout(timeout_ms / 1000.0)).
    #[test]
    fn cfg_ipc_socket_restart_state_and_kill_timeout() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_ipc_socket(&mut ctx, &mut r, Some("/tmp/i3-test/socket"));
        assert_eq!(d.config.ipc_socket_path.as_deref(), Some("/tmp/i3-test/socket"));

        d.cfg_restart_state(&mut ctx, &mut r, Some("/tmp/i3-test/restart-state.json"));
        assert_eq!(
            d.config.restart_state_path.as_deref(),
            Some("/tmp/i3-test/restart-state.json")
        );

        assert_eq!(d.ipc_kill_timeout, crate::ipc::DEFAULT_KILL_TIMEOUT);
        d.cfg_ipc_kill_timeout(&mut ctx, &mut r, 2000);
        assert!((d.ipc_kill_timeout - 2.0).abs() < f64::EPSILON);
        d.cfg_ipc_kill_timeout(&mut ctx, &mut r, 250);
        assert!((d.ipc_kill_timeout - 0.25).abs() < f64::EPSILON);
    }

    /// C: src/config_directives.c:447-501 — the boolean/string directives store
    /// their value (focus_follows_mouse is inverted, as in C).
    #[test]
    fn cfg_boolean_directives_store_their_values() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_focus_follows_mouse(&mut ctx, &mut r, Some("no"));
        assert!(d.config.disable_focus_follows_mouse);
        d.cfg_focus_follows_mouse(&mut ctx, &mut r, Some("yes"));
        assert!(!d.config.disable_focus_follows_mouse);

        d.cfg_force_xinerama(&mut ctx, &mut r, Some("yes"));
        assert!(d.config.force_xinerama);
        d.cfg_disable_randr15(&mut ctx, &mut r, Some("yes"));
        assert!(d.config.disable_randr15);
        d.cfg_workspace_back_and_forth(&mut ctx, &mut r, Some("yes"));
        assert!(d.config.workspace_auto_back_and_forth);
        d.cfg_show_marks(&mut ctx, &mut r, Some("no"));
        assert!(!d.config.show_marks);

        d.cfg_fake_outputs(&mut ctx, &mut r, Some("LVDS-1,HDMI-1"));
        assert_eq!(d.config.fake_outputs.as_deref(), Some("LVDS-1,HDMI-1"));

        d.cfg_force_display_urgency_hint(&mut ctx, &mut r, 1500);
        assert!((d.config.workspace_urgency_timer - 1.5).abs() < f32::EPSILON);
    }

    /// C: src/config_directives.c:467/479 — focus_wrapping with the modern
    /// values plus the deprecated force_focus_wrapping alias.
    #[test]
    fn cfg_focus_wrapping_and_its_legacy_alias() {
        let (mut d, mut ctx, mut r) = parse_state();

        for (value, expected) in [
            ("force", FocusWrapping::FOCUS_WRAPPING_FORCE),
            ("workspace", FocusWrapping::FOCUS_WRAPPING_WORKSPACE),
            ("yes", FocusWrapping::FOCUS_WRAPPING_ON),
            ("no", FocusWrapping::FOCUS_WRAPPING_OFF),
        ] {
            d.cfg_focus_wrapping(&mut ctx, &mut r, Some(value));
            assert_eq!(d.config.focus_wrapping, expected, "value {value}");
        }

        // C: force_focus_wrapping yes → FORCE.
        d.cfg_force_focus_wrapping(&mut ctx, &mut r, Some("yes"));
        assert_eq!(d.config.focus_wrapping, FocusWrapping::FOCUS_WRAPPING_FORCE);

        // C: force_focus_wrapping no only un-forces an active FORCE.
        d.cfg_force_focus_wrapping(&mut ctx, &mut r, Some("no"));
        assert_eq!(d.config.focus_wrapping, FocusWrapping::FOCUS_WRAPPING_ON);

        d.config.focus_wrapping = FocusWrapping::FOCUS_WRAPPING_OFF;
        d.cfg_force_focus_wrapping(&mut ctx, &mut r, Some("no"));
        assert_eq!(d.config.focus_wrapping, FocusWrapping::FOCUS_WRAPPING_OFF);
    }

    /// C: src/config_directives.c:451 CFGFUN(mouse_warping) — "none" and
    /// "output" are mapped, other values keep the previous setting.
    #[test]
    fn cfg_mouse_warping_maps_none_and_output() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_mouse_warping(&mut ctx, &mut r, Some("none"));
        assert_eq!(d.config.mouse_warping, Warping::POINTER_WARPING_NONE);
        d.cfg_mouse_warping(&mut ctx, &mut r, Some("output"));
        assert_eq!(d.config.mouse_warping, Warping::POINTER_WARPING_OUTPUT);
        d.cfg_mouse_warping(&mut ctx, &mut r, Some("bogus"));
        assert_eq!(d.config.mouse_warping, Warping::POINTER_WARPING_OUTPUT);
    }

    /// C: src/config_directives.c:374/384/711 — orientation, layout and
    /// tiling_drag mappings (including the stacking/stacked alias).
    #[test]
    fn cfg_orientation_layout_and_tiling_drag() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_default_orientation(&mut ctx, &mut r, Some("horizontal"));
        assert_eq!(d.config.default_orientation, Orientation::HORIZ);
        d.cfg_default_orientation(&mut ctx, &mut r, Some("vertical"));
        assert_eq!(d.config.default_orientation, Orientation::VERT);
        d.cfg_default_orientation(&mut ctx, &mut r, Some("none"));
        assert_eq!(d.config.default_orientation, Orientation::NO_ORIENTATION);

        d.cfg_workspace_layout(&mut ctx, &mut r, Some("default"));
        assert_eq!(d.config.default_layout, Layout::L_DEFAULT);
        d.cfg_workspace_layout(&mut ctx, &mut r, Some("stacking"));
        assert_eq!(d.config.default_layout, Layout::L_STACKED);
        d.cfg_workspace_layout(&mut ctx, &mut r, Some("stacked"));
        assert_eq!(d.config.default_layout, Layout::L_STACKED);
        d.cfg_workspace_layout(&mut ctx, &mut r, Some("tabbed"));
        assert_eq!(d.config.default_layout, Layout::L_TABBED);

        for (value, expected) in [
            ("modifier", tiling_drag_t::TILING_DRAG_MODIFIER),
            ("titlebar", tiling_drag_t::TILING_DRAG_TITLEBAR),
            ("modifier,titlebar", tiling_drag_t::TILING_DRAG_MODIFIER_OR_TITLEBAR),
            ("titlebar,modifier", tiling_drag_t::TILING_DRAG_MODIFIER_OR_TITLEBAR),
            ("off", tiling_drag_t::TILING_DRAG_OFF),
        ] {
            d.cfg_tiling_drag(&mut ctx, &mut r, Some(value));
            assert_eq!(d.config.tiling_drag, expected, "value {value}");
        }
    }

    /// C: src/config_directives.c:348/587/501/505 — smart_gaps,
    /// popup_during_fullscreen and focus_on_window_activation (an unknown mode
    /// keeps the previous value and only logs).
    #[test]
    fn cfg_smart_gaps_popup_and_focus_on_activation() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_smart_gaps(&mut ctx, &mut r, Some("yes"));
        assert_eq!(d.config.smart_gaps, SmartGaps::SMART_GAPS_ON);
        d.cfg_smart_gaps(&mut ctx, &mut r, Some("no"));
        assert_eq!(d.config.smart_gaps, SmartGaps::SMART_GAPS_OFF);
        d.cfg_smart_gaps(&mut ctx, &mut r, Some("inverse_outer"));
        assert_eq!(d.config.smart_gaps, SmartGaps::SMART_GAPS_INVERSE_OUTER);

        d.cfg_popup_during_fullscreen(&mut ctx, &mut r, Some("ignore"));
        assert_eq!(
            d.config.popup_during_fullscreen,
            popup_during_fullscreen_t::PDF_IGNORE
        );
        d.cfg_popup_during_fullscreen(&mut ctx, &mut r, Some("leave_fullscreen"));
        assert_eq!(
            d.config.popup_during_fullscreen,
            popup_during_fullscreen_t::PDF_LEAVE_FULLSCREEN
        );
        d.cfg_popup_during_fullscreen(&mut ctx, &mut r, Some("all"));
        assert_eq!(
            d.config.popup_during_fullscreen,
            popup_during_fullscreen_t::PDF_ALL
        );
        d.cfg_popup_during_fullscreen(&mut ctx, &mut r, Some("smart"));
        assert_eq!(
            d.config.popup_during_fullscreen,
            popup_during_fullscreen_t::PDF_SMART
        );

        d.cfg_focus_on_window_activation(&mut ctx, &mut r, Some("urgent"));
        assert_eq!(
            d.config.focus_on_window_activation,
            focus_on_window_activation_t::FOWA_URGENT
        );
        d.cfg_focus_on_window_activation(&mut ctx, &mut r, Some("focus"));
        assert_eq!(
            d.config.focus_on_window_activation,
            focus_on_window_activation_t::FOWA_FOCUS
        );
        d.cfg_focus_on_window_activation(&mut ctx, &mut r, Some("none"));
        assert_eq!(
            d.config.focus_on_window_activation,
            focus_on_window_activation_t::FOWA_NONE
        );
        // Unknown mode: ELOG + return, the previous value stays.
        d.cfg_focus_on_window_activation(&mut ctx, &mut r, Some("bogus"));
        assert_eq!(
            d.config.focus_on_window_activation,
            focus_on_window_activation_t::FOWA_NONE
        );
    }

    /// C: src/config_directives.c:950/958 CFGFUN(bar_start) and (bar_finish) —
    /// a bar block starts with tray_padding 2 and Mod4, gets a generated id
    /// bar-<number_barconfigs> unless configured, and is appended to
    /// barconfigs.
    #[test]
    fn cfg_bar_start_and_finish_collect_the_barconfigs() {
        let (mut d, mut ctx, mut r) = parse_state();
        assert_eq!(d.config.number_barconfigs, 0);

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_finish(&mut ctx, &mut r);
        assert_eq!(d.barconfigs.len(), 1);
        assert_eq!(d.barconfigs[0].id.as_deref(), Some("bar-0"));
        assert_eq!(d.barconfigs[0].tray_padding, 2);
        assert_eq!(d.barconfigs[0].modifier, XCB_KEY_BUT_MASK_MOD_4);
        assert_eq!(d.config.number_barconfigs, 1);

        // A configured id is kept, and the counter keeps counting.
        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_id(&mut ctx, &mut r, Some("mybar"));
        d.cfg_bar_finish(&mut ctx, &mut r);
        assert_eq!(d.barconfigs.len(), 2);
        assert_eq!(d.barconfigs[1].id.as_deref(), Some("mybar"));
        assert_eq!(d.config.number_barconfigs, 2);
    }

    /// C: src/config_directives.c:776-800 — the four padding forms fill the
    /// Rect as x=left, y=top, width=right, height=bottom.
    #[test]
    fn cfg_bar_padding_forms_fill_the_rect() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_padding_one(&mut ctx, &mut r, 4);
        d.cfg_bar_finish(&mut ctx, &mut r);

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_padding_two(&mut ctx, &mut r, 2, 3);
        d.cfg_bar_finish(&mut ctx, &mut r);

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_padding_three(&mut ctx, &mut r, 1, 2, 3);
        d.cfg_bar_finish(&mut ctx, &mut r);

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_padding_four(&mut ctx, &mut r, 1, 2, 3, 4);
        d.cfg_bar_finish(&mut ctx, &mut r);

        assert_eq!(d.barconfigs[0].padding, crate::data::Rect::new(4, 4, 4, 4));
        // padding <all> <right_and_left>: x/w = right, y/h = top_and_bottom.
        assert_eq!(d.barconfigs[1].padding, crate::data::Rect::new(3, 2, 3, 2));
        assert_eq!(d.barconfigs[2].padding, crate::data::Rect::new(2, 1, 2, 3));
        assert_eq!(d.barconfigs[3].padding, crate::data::Rect::new(4, 1, 2, 3));
    }

    /// C: src/config_directives.c:731-860 — the string/enum bar options land in
    /// the barconfig, and outputs/tray_outputs accumulate.
    #[test]
    fn cfg_bar_options_are_stored_in_the_barconfig() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_font(&mut ctx, &mut r, Some("pango:monospace 8"));
        d.cfg_bar_separator_symbol(&mut ctx, &mut r, Some("|"));
        d.cfg_bar_mode(&mut ctx, &mut r, Some("hide"));
        d.cfg_bar_hidden_state(&mut ctx, &mut r, Some("hide"));
        d.cfg_bar_position(&mut ctx, &mut r, Some("top"));
        d.cfg_bar_output(&mut ctx, &mut r, Some("DP-1"));
        d.cfg_bar_output(&mut ctx, &mut r, Some("HDMI-1"));
        d.cfg_bar_verbose(&mut ctx, &mut r, Some("yes"));
        d.cfg_bar_height(&mut ctx, &mut r, 28);
        d.cfg_bar_tray_output(&mut ctx, &mut r, Some("DP-1"));
        d.cfg_bar_tray_padding(&mut ctx, &mut r, 6);
        d.cfg_bar_i3bar_command(&mut ctx, &mut r, Some("i3bar --bar_id=bar-0"));
        d.cfg_bar_status_command(&mut ctx, &mut r, Some("i3status"));
        d.cfg_bar_workspace_command(&mut ctx, &mut r, Some("i3ws"));
        d.cfg_bar_socket_path(&mut ctx, &mut r, Some("/tmp/i3-bar.sock"));
        d.cfg_bar_binding_mode_indicator(&mut ctx, &mut r, Some("no"));
        d.cfg_bar_workspace_buttons(&mut ctx, &mut r, Some("no"));
        d.cfg_bar_workspace_min_width(&mut ctx, &mut r, 30);
        d.cfg_bar_strip_workspace_numbers(&mut ctx, &mut r, Some("yes"));
        d.cfg_bar_strip_workspace_name(&mut ctx, &mut r, Some("yes"));
        d.cfg_bar_finish(&mut ctx, &mut r);

        let bar = &d.barconfigs[0];
        assert_eq!(bar.font.as_deref(), Some("pango:monospace 8"));
        assert_eq!(bar.separator_symbol.as_deref(), Some("|"));
        assert_eq!(bar.mode, bar_mode_t::M_HIDE);
        assert_eq!(bar.hidden_state, bar_hidden_state_t::S_HIDE);
        assert_eq!(bar.position, bar_position_t::P_TOP);
        assert_eq!(bar.outputs, vec!["DP-1".to_string(), "HDMI-1".to_string()]);
        assert_eq!(bar.num_outputs, 2);
        assert!(bar.verbose);
        assert_eq!(bar.bar_height, 28);
        assert_eq!(bar.tray_outputs.len(), 1);
        assert_eq!(bar.tray_outputs[0].output, "DP-1");
        assert_eq!(bar.tray_padding, 6);
        assert_eq!(bar.i3bar_command.as_deref(), Some("i3bar --bar_id=bar-0"));
        assert_eq!(bar.status_command.as_deref(), Some("i3status"));
        assert_eq!(bar.workspace_command.as_deref(), Some("i3ws"));
        assert_eq!(bar.socket_path.as_deref(), Some("/tmp/i3-bar.sock"));
        // C: the options are inverted (hide_* = !boolstr(value)).
        assert!(bar.hide_binding_mode_indicator);
        assert!(bar.hide_workspace_buttons);
        assert_eq!(bar.workspace_min_width, 30);
        assert!(bar.strip_workspace_numbers);
        assert!(bar.strip_workspace_name);
    }

    /// C: src/config_directives.c:741/745/854 — mode dock, hidden_state show and
    /// position bottom are the non-matching branches of each mapping.
    #[test]
    fn cfg_bar_enum_fallbacks() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_mode(&mut ctx, &mut r, Some("dock"));
        d.cfg_bar_hidden_state(&mut ctx, &mut r, Some("show"));
        d.cfg_bar_position(&mut ctx, &mut r, Some("bottom"));
        d.cfg_bar_workspace_buttons(&mut ctx, &mut r, Some("yes"));
        d.cfg_bar_binding_mode_indicator(&mut ctx, &mut r, Some("yes"));
        d.cfg_bar_finish(&mut ctx, &mut r);

        let bar = &d.barconfigs[0];
        assert_eq!(bar.mode, bar_mode_t::M_DOCK);
        assert_eq!(bar.hidden_state, bar_hidden_state_t::S_SHOW);
        assert_eq!(bar.position, bar_position_t::P_BOTTOM);
        assert!(!bar.hide_workspace_buttons);
        assert!(!bar.hide_binding_mode_indicator);

        // C: an unknown mode word falls through to M_INVISIBLE.
        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_mode(&mut ctx, &mut r, Some("invisible"));
        d.cfg_bar_finish(&mut ctx, &mut r);
        assert_eq!(d.barconfigs[1].mode, bar_mode_t::M_INVISIBLE);
    }

    /// C: src/config_directives.c:863 CFGFUN(bar_color) — the new syntax
    /// (border, background, text) versus the old one (text, background).
    #[test]
    fn cfg_bar_color_supports_both_syntaxes() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_color(
            &mut ctx,
            &mut r,
            Some("focused_workspace"),
            Some("#111111"),
            Some("#222222"),
            Some("#333333"),
        );
        // Old syntax: no text word, so border holds the text color.
        d.cfg_bar_color(
            &mut ctx,
            &mut r,
            Some("active_workspace"),
            Some("#444444"),
            Some("#555555"),
            None,
        );
        d.cfg_bar_color(
            &mut ctx,
            &mut r,
            Some("binding_mode"),
            Some("#666666"),
            Some("#777777"),
            Some("#888888"),
        );
        d.cfg_bar_finish(&mut ctx, &mut r);

        let colors = &d.barconfigs[0].colors;
        assert_eq!(colors.focused_workspace_border.as_deref(), Some("#111111"));
        assert_eq!(colors.focused_workspace_bg.as_deref(), Some("#222222"));
        assert_eq!(colors.focused_workspace_text.as_deref(), Some("#333333"));

        assert_eq!(colors.active_workspace_border, None, "old syntax has no border");
        assert_eq!(colors.active_workspace_bg.as_deref(), Some("#555555"));
        assert_eq!(colors.active_workspace_text.as_deref(), Some("#444444"));

        assert_eq!(colors.binding_mode_border.as_deref(), Some("#666666"));
        assert_eq!(colors.binding_mode_bg.as_deref(), Some("#777777"));
        assert_eq!(colors.binding_mode_text.as_deref(), Some("#888888"));
        // The other workspace classes stay untouched.
        assert_eq!(colors.urgent_workspace_bg, None);
    }

    /// C: src/config_directives.c:904 CFGFUN(bar_color_single) — every single
    /// color class except the four named ones falls through to
    /// focused_statusline.
    #[test]
    fn cfg_bar_color_single_dispatches_the_classes() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_color_single(&mut ctx, &mut r, Some("background"), Some("#010101"));
        d.cfg_bar_color_single(&mut ctx, &mut r, Some("separator"), Some("#020202"));
        d.cfg_bar_color_single(&mut ctx, &mut r, Some("statusline"), Some("#030303"));
        d.cfg_bar_color_single(&mut ctx, &mut r, Some("focused_background"), Some("#040404"));
        d.cfg_bar_color_single(&mut ctx, &mut r, Some("focused_separator"), Some("#050505"));
        d.cfg_bar_color_single(&mut ctx, &mut r, Some("focused_statusline"), Some("#060606"));
        d.cfg_bar_finish(&mut ctx, &mut r);

        let colors = &d.barconfigs[0].colors;
        assert_eq!(colors.background.as_deref(), Some("#010101"));
        assert_eq!(colors.separator.as_deref(), Some("#020202"));
        assert_eq!(colors.statusline.as_deref(), Some("#030303"));
        assert_eq!(colors.focused_background.as_deref(), Some("#040404"));
        assert_eq!(colors.focused_separator.as_deref(), Some("#050505"));
        assert_eq!(colors.focused_statusline.as_deref(), Some("#060606"));
    }

    /// C: src/config_directives.c:812-853 bar_configure_binding — only
    /// "buttonN" with N >= 1 is accepted, duplicates (same button and release
    /// flag) are ignored, and the deprecated wheel_*_cmd land on button4/5.
    #[test]
    fn cfg_bar_bindings_validate_the_button_and_deduplicate() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_bar_start(&mut ctx, &mut r);
        d.cfg_bar_bindsym(&mut ctx, &mut r, Some("button1"), None, Some("workspace 1"));
        // Same button and release flag → ignored.
        d.cfg_bar_bindsym(&mut ctx, &mut r, Some("button1"), None, Some("workspace 2"));
        // Not a button → ignored.
        d.cfg_bar_bindsym(&mut ctx, &mut r, Some("m"), None, Some("nop"));
        // button0 is below the accepted range → ignored.
        d.cfg_bar_bindsym(&mut ctx, &mut r, Some("button0"), None, Some("nop"));
        // A release binding is a separate entry.
        d.cfg_bar_bindsym(&mut ctx, &mut r, Some("button1"), Some("release"), Some("nop"));
        // C: wheel_up_cmd is the deprecated spelling of bindsym button4.
        d.cfg_bar_wheel_up_cmd(&mut ctx, &mut r, Some("exec nop"));
        d.cfg_bar_finish(&mut ctx, &mut r);

        let bindings = &d.barconfigs[0].bar_bindings;
        assert_eq!(bindings.len(), 3);
        assert_eq!(
            (bindings[0].input_code, bindings[0].command.as_str(), bindings[0].release),
            (1, "workspace 1", false)
        );
        assert_eq!(
            (bindings[1].input_code, bindings[1].command.as_str(), bindings[1].release),
            (1, "nop", true)
        );
        assert_eq!(
            (bindings[2].input_code, bindings[2].command.as_str(), bindings[2].release),
            (4, "exec nop", false)
        );
    }

    /// C: src/config_directives.c:18 CFGFUN(include) — a pattern that resolves
    /// to no existing file is skipped without an error.
    #[test]
    fn cfg_include_skips_unresolvable_files() {
        let (mut d, mut ctx, mut r) = parse_state();

        d.cfg_include(&mut ctx, &mut r, Some("/nonexistent-i3-rs-testdir/*.conf"));
        assert!(d.included_files.is_empty());
        assert!(!r.has_errors);
    }
}
