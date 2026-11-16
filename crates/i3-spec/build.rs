// C: generate-command-parser.pl:1-252 (i3 4.25, commit 8a97b0f) — 1:1 Rust
// port of the spec-to-parser generator, run as the build script of this crate.
//
// It reads ../../i3-src/parser-specs/commands.spec and config.spec (the single
// source of truth, no copies are made) exactly like the perl script does and
// writes the state machines as Rust into $OUT_DIR/generated_commands.rs and
// $OUT_DIR/generated_config.rs, which src/commands_parser.rs and
// src/config_parser.rs include!().
//
// The steps mirror the perl script:
//   1. slurp the file and join token definitions spread over multiple lines
//      (perl:37-60)
//   2. build the states and their token rules (perl:62-107)
//   3. generate the enum values, sorted descendingly by name length
//      (perl:109-130)
//   4. generate the call function with one call_id per call token
//      (perl:132-207)
//   5. generate the token tables (perl:209-251)

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"));
    // The two spec files are vendored under third_party/i3/parser-specs (copies of
    // i3 4.25, BSD-3-Clause, see third_party/i3/LICENSE) so that the crate builds
    // from a plain checkout. Set I3_SRC to a full i3 source tree to use that
    // instead (handy while porting: the spec files stay the single source of
    // truth there).
    let spec_dir = match env::var_os("I3_SRC") {
        Some(src) => PathBuf::from(src).join("parser-specs"),
        None => manifest_dir.join("../../third_party/i3/parser-specs"),
    };
    let commands_spec = spec_dir.join("commands.spec");
    let config_spec = spec_dir.join("config.spec");

    // C: the spec files are the single source of truth (no copy is made).
    println!("cargo:rerun-if-changed={}", commands_spec.display());
    println!("cargo:rerun-if-changed={}", config_spec.display());

    let commands_model = Model::build(&commands_spec, "command");
    let config_model = Model::build(&config_spec, "config");

    fs::write(out_dir.join("generated_commands.rs"), emit_commands(&commands_model))
        .expect("write $OUT_DIR/generated_commands.rs");
    fs::write(out_dir.join("generated_config.rs"), emit_config(&config_model))
        .expect("write $OUT_DIR/generated_config.rs");

    // Diagnostics (visible with cargo -vv).
    for model in [&commands_model, &config_model] {
        let n_tokens: usize = model.keys.iter().map(|k| model.tokens[k].len()).sum();
        eprintln!(
            "i3-spec: parsed {}.spec: {} states (+__CALL), {} tokens, {} calls, {} hook functions",
            model.prefix,
            model.keys.len(),
            n_tokens,
            model.calls.len(),
            model.functions.len()
        );
    }
}

/* ---------------------------------------------------------------------------
 * Spec parsing (perl steps 1 and 2)
 * ------------------------------------------------------------------------ */

/// C: generate-command-parser.pl:95-99 — one token rule of a state.
#[derive(Clone)]
struct SpecToken {
    /// C: token — literal tokens keep their leading single quote and lose the
    /// trailing one (perl:220-224).
    token: String,
    /// C: identifier — NULL when the token has no identifier.
    identifier: Option<String>,
    /// C: next_state — a state name, or "call <id>" after the call processing
    /// step.
    next_state: String,
}

/// C: generate-command-parser.pl:28-60 — reads in a whole file, then joins
/// token definitions which are spread over multiple lines. Comment lines
/// (^\s*#) and empty lines (^\s*$) are skipped; a line starting with
/// whitespace followed by "->" is a continuation and is appended to the
/// previous line.
fn slurp_and_preprocess(path: &Path) -> Vec<String> {
    let content = fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "generate-command-parser.pl:25: Input file {:?} cannot be read: {}",
            path, e
        )
    });
    let raw_lines: Vec<&str> = content.split('\n').collect();
    let mut lines: Vec<String> = Vec::new();
    for line in raw_lines {
        // C: next if $line =~ /^\s*#/ || $line =~ /^\s*$/;
        let trimmed = trim_perl_ws(line);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // C: if ($line =~ /^\s+->) { $lines[$#lines] .= $line; }
        if starts_with_ws_arrow(line) {
            let last = lines
                .last_mut()
                .expect("continuation line without a previous line");
            last.push_str(line);
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

/// C: \s in perl — space, \t, \r, \n, \f, \v.
fn is_perl_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0b' | '\x0c')
}

/// C: $line =~ /^\s+->/
fn starts_with_ws_arrow(line: &str) -> bool {
    let mut chars = line.chars();
    if !chars.next().map_or(false, is_perl_ws) {
        return false;
    }
    let mut rest = line.chars();
    while let Some(c) = rest.next() {
        if !is_perl_ws(c) {
            return c == '-' && rest.next() == Some('>');
        }
    }
    false
}

/// C: $token =~ s/^\s*//g; s/\s*$//g;
fn trim_perl_ws(s: &str) -> &str {
    s.trim_matches(is_perl_ws)
}

/// C: generate-command-parser.pl:68 — $line =~ /^state ([A-Z0-9_]+):$/
fn parse_state_line(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("state ")?;
    let name = rest.strip_suffix(':')?;
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Some(name)
    } else {
        None
    }
}

/// C: generate-command-parser.pl:76-88 — the token definition regex
/// ^\s*([a-z_]+\s*=\s*|)(.*?)->\s*(.*) with /x. Returns
/// (identifier, tokens, action); the identifier is already cleaned up like
/// perl:85 ($identifier =~ s/^\s*(\S+)\s*=\s*$/$1/g).
fn parse_token_line(line: &str) -> Option<(Option<String>, &str, &str)> {
    let bytes = line.as_bytes();
    // ^\s* — greedy leading whitespace.
    let mut i = 0;
    while i < bytes.len() && is_perl_ws(bytes[i] as char) {
        i += 1;
    }
    // Optional identifier: ([a-z_]+\s*=\s*|). The perl alternation is tried
    // first, so it matches whenever the maximal [a-z_]+ run at the first
    // non-whitespace position is followed by optional whitespace and '='
    // (shorter runs can never work: the next char would still be [a-z_]).
    let mut identifier = None;
    let mut after_ident = i;
    let mut j = i;
    while j < bytes.len() && (bytes[j].is_ascii_lowercase() || bytes[j] == b'_') {
        j += 1;
    }
    if j > i {
        let mut k = j;
        while k < bytes.len() && is_perl_ws(bytes[k] as char) {
            k += 1;
        }
        if k < bytes.len() && bytes[k] == b'=' {
            k += 1;
            while k < bytes.len() && is_perl_ws(bytes[k] as char) {
                k += 1;
            }
            identifier = Some(line[i..j].to_string());
            after_ident = k;
        }
    }
    // (.*?)->\s*(.*) — the lazy dot finds the first '->' after the identifier.
    let rest = &line[after_ident..];
    let arrow = rest.find("->")?;
    let tokens = &rest[..arrow];
    let action_bytes = rest.as_bytes();
    let mut action_start = arrow + 2;
    while action_start < action_bytes.len() && is_perl_ws(action_bytes[action_start] as char) {
        action_start += 1;
    }
    Some((identifier, tokens, &rest[action_start..]))
}

/// C: generate-command-parser.pl:65-107 — build the data structure containing
/// all states and their token rules. States are returned in file order (the
/// perl hash loses that order, but all later iteration goes over the sorted
/// @keys; the token order inside a state is the file order, which is kept).
fn parse_states(lines: &[String]) -> Vec<(String, Vec<SpecToken>)> {
    let mut states: Vec<(String, Vec<SpecToken>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut current_state = String::new();
    for line in lines {
        if let Some(state) = parse_state_line(line) {
            // C: $current_state = $state;
            current_state = state.to_string();
            if !index.contains_key(state) {
                index.insert(state.to_string(), states.len());
                states.push((state.to_string(), Vec::new()));
            }
            continue;
        }
        // Must be a token definition: [identifier = ] <tokens> -> <action>
        let (identifier, tokens, action) = match parse_token_line(line) {
            Some(v) => v,
            // A line without '->' makes the perl regex fail; no tokens are
            // pushed in that case.
            None => continue,
        };
        // C: $action = $current_state if length($action) == 0;
        let action = if action.is_empty() {
            current_state.clone()
        } else {
            action.to_string()
        };
        // C: for my $token (split(',', $tokens)) — trimmed, then stored.
        for token in tokens.split(',') {
            let token = trim_perl_ws(token);
            let store_token = SpecToken {
                token: token.to_string(),
                identifier: identifier.clone(),
                next_state: action.clone(),
            };
            let slot = &mut states[index[&current_state]].1;
            slot.push(store_token);
        }
    }
    states
}

/* ---------------------------------------------------------------------------
 * Model building (perl steps 3 and 4)
 * ------------------------------------------------------------------------ */

/// C: the call arguments after the perl substitutions (perl:148-155):
/// $ident (parser_get_string), &ident (parser_get_long), NULL, string
/// literals, numbers and state references (replaced by their enum value).
#[derive(Clone, Debug, PartialEq)]
enum ArgSpec {
    /// $ident — C: parser_get_string(stack, "ident")
    StackString(String),
    /// &ident — C: parser_get_long(stack, "ident")
    StackLong(String),
    /// NULL
    Null,
    /// "literal" — a string literal from the spec
    Str(String),
    /// a number literal, e.g. 0, -1, 10
    Num(i64),
    /// a state name reference (e.g. cfg_criteria_init(FOR_WINDOW_COMMAND))
    State(String),
}

impl ArgSpec {
    fn param_type(&self) -> ParamType {
        match self {
            ArgSpec::StackString(_) | ArgSpec::Null | ArgSpec::Str(_) => ParamType::Str,
            ArgSpec::StackLong(_) | ArgSpec::Num(_) => ParamType::Long,
            ArgSpec::State(_) => ParamType::Int,
        }
    }
}

/// The Rust parameter type used for a call argument position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ParamType {
    /// C: const char * — Option<&str>
    Str,
    /// C: long — i64
    Long,
    /// C: int — i32
    Int,
}

impl ParamType {
    fn rust_type(self) -> &'static str {
        match self {
            ParamType::Str => "Option<&str>",
            ParamType::Long => "i64",
            ParamType::Int => "i32",
        }
    }
}

/// C: one case of GENERATED_call (perl:174-199).
#[derive(Clone, Debug)]
struct CallSpec {
    call_id: usize,
    /// function name, e.g. cmd_criteria_add
    func: String,
    args: Vec<ArgSpec>,
    /// state name the call returns to (perl:147: $next_state ||= 'INITIAL')
    next_state: String,
    /// original action text from the spec, for doc comments
    spec_text: String,
}

/// One hook trait method parameter.
#[derive(Clone, Debug)]
struct ParamSpec {
    ty: ParamType,
    name: String,
}

/// One hook trait method (the C cmd_*/cfg_* function).
#[derive(Clone, Debug)]
struct FuncSpec {
    name: String,
    params: Vec<ParamSpec>,
    /// distinct call forms, for doc comments
    forms: Vec<String>,
}

struct Model {
    /// C: --prefix (command / config)
    prefix: &'static str,
    /// C: perl:115 — @keys = sort { (length($b) <=> length($a)) or ($a cmp $b) }
    keys: Vec<String>,
    /// tokens per state name; call actions are rewritten to "call <id>"
    tokens: HashMap<String, Vec<SpecToken>>,
    /// C: the GENERATED_call switch, indexed by call_id
    calls: Vec<CallSpec>,
    /// hook trait inventory, in first-appearance order
    functions: Vec<FuncSpec>,
    /// resolved parameter type per (function, position) — a literal 0 can act
    /// as the C null pointer constant in an otherwise const char * position
    /// (see Model::build)
    param_types: HashMap<(String, usize), ParamType>,
}

/// Parameter names for call positions that only ever receive literals in the
/// spec (there is no $identifier to take the name from). The names are taken
/// from the C prototypes so the generated traits match
/// include/commands.h / include/config_directives.h.
const ARG_NAME_HINTS: &[(&str, usize, &str)] = &[
    // C: include/commands.h:201
    ("cmd_focus", 0, "focus_workspace"),
    // C: include/commands.h:141
    ("cmd_move_con_to_output", 1, "move_workspace"),
    // C: include/config_directives.h:67
    ("cfg_assign", 1, "is_number"),
    // C: include/config_directives.h:38
    ("cfg_criteria_init", 0, "_state"),
];

/// C: include/commands.h — line numbers of the cmd_* prototypes, used for doc
/// comments on the generated CommandHooks methods.
const CMD_PROTO_LINES: &[(&str, u32)] = &[
    ("cmd_criteria_init", 24),
    ("cmd_criteria_match_windows", 31),
    ("cmd_criteria_add", 38),
    ("cmd_move_con_to_workspace", 45),
    ("cmd_move_con_to_workspace_back_and_forth", 51),
    ("cmd_move_con_to_workspace_name", 57),
    ("cmd_move_con_to_workspace_number", 63),
    ("cmd_resize_set", 69),
    ("cmd_resize", 75),
    ("cmd_border", 81),
    ("cmd_nop", 87),
    ("cmd_append_layout", 93),
    ("cmd_workspace", 99),
    ("cmd_workspace_number", 105),
    ("cmd_workspace_back_and_forth", 111),
    ("cmd_workspace_name", 117),
    ("cmd_mark", 123),
    ("cmd_unmark", 129),
    ("cmd_mode", 135),
    ("cmd_move_con_to_output", 141),
    ("cmd_move_con_to_mark", 147),
    ("cmd_floating", 153),
    ("cmd_split", 159),
    ("cmd_kill", 165),
    ("cmd_exec", 171),
    ("cmd_focus_direction", 177),
    ("cmd_focus_sibling", 183),
    ("cmd_focus_window_mode", 189),
    ("cmd_focus_level", 195),
    ("cmd_focus", 201),
    ("cmd_fullscreen", 207),
    ("cmd_sticky", 213),
    ("cmd_move_direction", 219),
    ("cmd_layout", 225),
    ("cmd_layout_toggle", 231),
    ("cmd_exit", 237),
    ("cmd_reload", 243),
    ("cmd_restart", 249),
    ("cmd_open", 255),
    ("cmd_focus_output", 261),
    ("cmd_move_window_to_position", 267),
    ("cmd_move_window_to_center", 273),
    ("cmd_move_window_to_mouse", 279),
    ("cmd_move_scratchpad", 285),
    ("cmd_scratchpad_show", 291),
    ("cmd_swap", 297),
    ("cmd_title_format", 303),
    ("cmd_rename_workspace", 309),
    ("cmd_bar_mode", 315),
    ("cmd_bar_hidden_state", 321),
    ("cmd_shmlog", 327),
    ("cmd_debuglog", 333),
    ("cmd_gaps", 339),
    ("cmd_title_window_icon", 345),
];

/// C: include/config_directives.h — line numbers of the CFGFUN() prototypes,
/// used for doc comments on the generated ConfigHooks methods.
const CFG_PROTO_LINES: &[(&str, u32)] = &[
    ("cfg_criteria_init", 38),
    ("cfg_criteria_add", 39),
    ("cfg_criteria_pop_state", 40),
    ("cfg_include", 42),
    ("cfg_font", 43),
    ("cfg_exec", 44),
    ("cfg_for_window", 45),
    ("cfg_gaps", 46),
    ("cfg_smart_borders", 47),
    ("cfg_smart_gaps", 48),
    ("cfg_floating_minimum_size", 49),
    ("cfg_floating_maximum_size", 50),
    ("cfg_default_orientation", 51),
    ("cfg_workspace_layout", 52),
    ("cfg_workspace_back_and_forth", 53),
    ("cfg_focus_follows_mouse", 54),
    ("cfg_mouse_warping", 55),
    ("cfg_focus_wrapping", 56),
    ("cfg_force_focus_wrapping", 57),
    ("cfg_force_xinerama", 58),
    ("cfg_disable_randr15", 59),
    ("cfg_fake_outputs", 60),
    ("cfg_force_display_urgency_hint", 61),
    ("cfg_focus_on_window_activation", 62),
    ("cfg_title_align", 63),
    ("cfg_show_marks", 64),
    ("cfg_hide_edge_borders", 65),
    ("cfg_assign_output", 66),
    ("cfg_assign", 67),
    ("cfg_no_focus", 68),
    ("cfg_ipc_socket", 69),
    ("cfg_ipc_kill_timeout", 70),
    ("cfg_tiling_drag", 71),
    ("cfg_tiling_drag_swap_modifier", 72),
    ("cfg_restart_state", 73),
    ("cfg_popup_during_fullscreen", 74),
    ("cfg_color", 75),
    ("cfg_color_single", 76),
    ("cfg_floating_modifier", 77),
    ("cfg_default_border", 78),
    ("cfg_workspace", 79),
    ("cfg_binding", 80),
    ("cfg_enter_mode", 82),
    ("cfg_mode_binding", 83),
    ("cfg_bar_font", 85),
    ("cfg_bar_separator_symbol", 86),
    ("cfg_bar_mode", 87),
    ("cfg_bar_hidden_state", 88),
    ("cfg_bar_id", 89),
    ("cfg_bar_output", 90),
    ("cfg_bar_verbose", 91),
    ("cfg_bar_height", 92),
    ("cfg_bar_padding_one", 93),
    ("cfg_bar_padding_two", 94),
    ("cfg_bar_padding_three", 95),
    ("cfg_bar_padding_four", 96),
    ("cfg_bar_modifier", 97),
    ("cfg_bar_wheel_up_cmd", 98),
    ("cfg_bar_wheel_down_cmd", 99),
    ("cfg_bar_bindsym", 100),
    ("cfg_bar_position", 101),
    ("cfg_bar_i3bar_command", 102),
    ("cfg_bar_color", 103),
    ("cfg_bar_socket_path", 104),
    ("cfg_bar_tray_output", 105),
    ("cfg_bar_tray_padding", 106),
    ("cfg_bar_color_single", 107),
    ("cfg_bar_status_command", 108),
    ("cfg_bar_workspace_command", 109),
    ("cfg_bar_binding_mode_indicator", 110),
    ("cfg_bar_workspace_buttons", 111),
    ("cfg_bar_workspace_min_width", 112),
    ("cfg_bar_strip_workspace_numbers", 113),
    ("cfg_bar_strip_workspace_name", 114),
    ("cfg_bar_start", 115),
    ("cfg_bar_finish", 116),
];

fn is_state_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn is_lower_ident(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// C: perl:148-155 — classify one call argument. State names must be known
/// (the perl replaces them with their enum value; an unknown one would be a C
/// compile error, which surfaces here as a build error).
fn classify_arg(arg: &str, statenum: &HashMap<String, usize>, prefix: &str) -> ArgSpec {
    if arg.starts_with('"') && arg.ends_with('"') && arg.len() >= 2 {
        return ArgSpec::Str(arg[1..arg.len() - 1].to_string());
    }
    if arg == "NULL" {
        return ArgSpec::Null;
    }
    if let Some(ident) = arg.strip_prefix('$') {
        assert!(
            is_lower_ident(ident),
            "{}.spec: invalid $ identifier {:?}",
            prefix,
            arg
        );
        return ArgSpec::StackString(ident.to_string());
    }
    if let Some(ident) = arg.strip_prefix('&') {
        assert!(
            is_lower_ident(ident),
            "{}.spec: invalid & identifier {:?}",
            prefix,
            arg
        );
        return ArgSpec::StackLong(ident.to_string());
    }
    if let Ok(n) = arg.parse::<i64>() {
        return ArgSpec::Num(n);
    }
    if statenum.contains_key(arg) {
        return ArgSpec::State(arg.to_string());
    }
    panic!("{}.spec: unrecognised call argument {:?}", prefix, arg);
}

impl Model {
    /// C: generate-command-parser.pl:62-207 — parse the spec, sort the state
    /// keys, number the states and assign a call_id to every call token.
    fn build(path: &Path, prefix: &'static str) -> Model {
        let lines = slurp_and_preprocess(path);
        let states = parse_states(&lines);
        let mut tokens: HashMap<String, Vec<SpecToken>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (name, list) in states {
            if !tokens.contains_key(&name) {
                order.push(name.clone());
            }
            tokens.insert(name, list);
        }
        // C: perl:115 — sort descendingly by length, then ascending
        // bytewise, so that occurrences of a state name can be replaced even
        // when one state name is included in another one (like FOR_WINDOW is
        // in FOR_WINDOW_COMMAND).
        let mut keys = order;
        keys.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.as_bytes().cmp(b.as_bytes())));

        let statenum: HashMap<String, usize> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i))
            .collect();
        assert!(
            statenum.contains_key("INITIAL"),
            "{}.spec has no INITIAL state (perl:147 needs it as the default next state)",
            prefix
        );

        let mut model = Model {
            prefix,
            keys,
            tokens,
            calls: Vec::new(),
            functions: Vec::new(),
            param_types: HashMap::new(),
        };

        // C: perl:138-201 — the call function: iterate the states in @keys
        // order and the tokens in file order; every token whose action starts
        // with "call " gets its own call_id.
        let mut forms: HashMap<String, Vec<(Vec<ArgSpec>, String)>> = HashMap::new();
        let mut function_order: Vec<String> = Vec::new();
        for state in model.keys.clone() {
            let list = model.tokens.get_mut(&state).expect("state exists");
            for token in list.iter_mut() {
                if !token.next_state.starts_with("call ") {
                    continue;
                }
                let full = token.next_state.clone();
                // C: my ($cmd) = ($token->{next_state} =~ /^call (.*)/);
                let cmd = full["call ".len()..].to_string();
                // C: my ($next_state) = ($cmd =~ /; ([A-Z_]+)$/);
                //    $cmd =~ s/; ([A-Z_]+)$//; $next_state ||= 'INITIAL';
                let (cmd, next_state) = match cmd.rfind("; ") {
                    Some(pos) if is_state_name(&cmd[pos + 2..]) => {
                        (cmd[..pos].to_string(), cmd[pos + 2..].to_string())
                    }
                    _ => (cmd, "INITIAL".to_string()),
                };
                assert!(
                    statenum.contains_key(&next_state),
                    "{}.spec: call target state {:?} is not a state",
                    prefix,
                    next_state
                );
                let open = cmd.find('(').expect("call action has a parenthesis");
                let func = cmd[..open].to_string();
                let args_text = cmd[open + 1..]
                    .strip_suffix(')')
                    .expect("call action ends with ')'")
                    .to_string();
                let args: Vec<ArgSpec> = if args_text.is_empty() {
                    Vec::new()
                } else {
                    args_text
                        .split(',')
                        .map(|a| classify_arg(trim_perl_ws(a), &statenum, prefix))
                        .collect()
                };
                let call_id = model.calls.len();
                model.calls.push(CallSpec {
                    call_id,
                    func: func.clone(),
                    args: args.clone(),
                    next_state: next_state.clone(),
                    spec_text: full.clone(),
                });
                // C: $token->{next_state} = "call $call_id";
                token.next_state = format!("call {}", call_id);
                let entry = forms.entry(func.clone()).or_insert_with(|| {
                    function_order.push(func.clone());
                    Vec::new()
                });
                if !entry.iter().any(|(a, _)| *a == args) {
                    entry.push((args, full["call ".len()..].to_string()));
                }
            }
        }

        // Validate every next_state is a real state (the C build fails on
        // unknown state names in the generated headers).
        for state in &model.keys {
            for token in &model.tokens[state] {
                if token.next_state.starts_with("call ") {
                    continue;
                }
                assert!(
                    statenum.contains_key(&token.next_state),
                    "{}.spec: token {:?} in state {} goes to unknown state {:?}",
                    prefix,
                    token.token,
                    state,
                    token.next_state
                );
            }
        }

        // Build the hook trait inventory: one method per function, parameters
        // typed by the union of all call forms. A literal 0 unifies with a
        // const char * position because it is the C null pointer constant
        // there (e.g. cmd_resize_set(..., 0, 0) passes mode_height = NULL).
        let mut functions: Vec<FuncSpec> = Vec::new();
        let mut param_types: HashMap<(String, usize), ParamType> = HashMap::new();
        for name in function_order {
            let fn_forms = &forms[&name];
            let arity = fn_forms.iter().map(|(a, _)| a.len()).max().unwrap();
            for (args, _) in fn_forms {
                assert!(
                    args.len() == arity,
                    "{}.spec: {} is called with differing argument counts",
                    prefix,
                    name
                );
            }
            let mut params: Vec<ParamSpec> = Vec::new();
            for pos in 0..arity {
                let mut has_str = false;
                let mut has_long = false;
                let mut long_all_zero = true;
                let mut has_int = false;
                for (args, _) in fn_forms {
                    match args[pos].param_type() {
                        ParamType::Str => has_str = true,
                        ParamType::Long => {
                            has_long = true;
                            if !matches!(args[pos], ArgSpec::Num(0)) {
                                long_all_zero = false;
                            }
                        }
                        ParamType::Int => has_int = true,
                    }
                }
                let ty = if has_int {
                    assert!(
                        !has_str && !has_long,
                        "{}.spec: argument {} of {} mixes a state reference with other types",
                        prefix,
                        pos,
                        name
                    );
                    ParamType::Int
                } else if has_str {
                    // C: a literal 0 is the null pointer constant for const
                    // char * arguments, so it unifies with the string type.
                    assert!(
                        !has_long || long_all_zero,
                        "{}.spec: argument {} of {} mixes a non-zero number with strings",
                        prefix,
                        pos,
                        name
                    );
                    ParamType::Str
                } else {
                    ParamType::Long
                };
                param_types.insert((name.clone(), pos), ty);
                // Name: first $/& identifier at this position, else the C
                // prototype name hint, else arg<pos>.
                let mut pname: Option<String> = None;
                for (args, _) in fn_forms {
                    match &args[pos] {
                        ArgSpec::StackString(id) | ArgSpec::StackLong(id) => {
                            pname = Some(id.clone());
                            break;
                        }
                        _ => {}
                    }
                }
                let pname = pname
                    .or_else(|| {
                        ARG_NAME_HINTS
                            .iter()
                            .find(|(f, p, _)| *f == name && *p == pos)
                            .map(|(_, _, n)| n.to_string())
                    })
                    .unwrap_or_else(|| format!("arg{}", pos));
                params.push(ParamSpec {
                    ty,
                    name: pname,
                });
            }
            functions.push(FuncSpec {
                name,
                params,
                forms: fn_forms.iter().map(|(_, f)| f.clone()).collect(),
            });
        }
        model.functions = functions;
        model.param_types = param_types;
        model
    }
}

/* ---------------------------------------------------------------------------
 * Code emission (perl steps 3, 4 and 5 as Rust)
 * ------------------------------------------------------------------------ */

fn rust_str_lit(s: &str) -> String {
    format!("{:?}", s)
}

/// Emit the state enum (C: GENERATED_<prefix>_enums.h, perl:117-130).
fn emit_enum(out: &mut String, model: &Model, enum_name: &str) {
    out.push_str(&format!(
        "/// C: GENERATED_{}_enums.h (generate-command-parser.pl:117-130) — the\n\
         /// cmdp_state enum. States are sorted descendingly by name length\n\
         /// (perl:115) so that name replacement works when one state name is\n\
         /// contained in another; __CALL is the last value.\n",
        model.prefix
    ));
    out.push_str(&format!(
        "#[allow(non_camel_case_types)]\n#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]\npub enum {} {{\n",
        enum_name
    ));
    for (i, name) in model.keys.iter().enumerate() {
        out.push_str(&format!("    {} = {},\n", name, i));
    }
    out.push_str(&format!("    __CALL = {},\n}}\n\n", model.keys.len()));

    out.push_str(&format!(
        "impl {} {{\n    /// C: the parser stores plain int state values (see\n    /// commands_parser.c:160 ctx.state and the next_state fields), so the\n    /// generated call targets convert back with this function.\n    pub fn from_i32(v: i32) -> Option<{}> {{\n        match v {{\n",
        enum_name, enum_name
    ));
    for (i, name) in model.keys.iter().enumerate() {
        out.push_str(&format!(
            "            {} => Some({}::{}),\n",
            i, enum_name, name
        ));
    }
    out.push_str(&format!(
        "            {} => Some({}::__CALL),\n            _ => None,\n        }}\n    }}\n}}\n\n",
        model.keys.len(),
        enum_name
    ));
}

/// Emit the token struct + per-state token tables + the table accessor
/// (C: GENERATED_<prefix>_tokens.h, perl:209-251).
fn emit_tokens(out: &mut String, model: &Model, enum_name: &str, token_name: &str, accessor: &str) {
    out.push_str(&format!(
        "/// C: commands_parser.c:43-51 — typedef struct token (cmdp_token).\n\
         /// Literal names keep the leading single quote and lose the trailing\n\
         /// one (generate-command-parser.pl:220-224) so that the runtime can\n\
         /// strdup(name + 1) like the C code does.\n",
    ));
    out.push_str(&format!(
        "#[derive(Clone, Copy)]\npub struct {} {{\n    /// C: char *name\n    pub name: &'static str,\n    /// C: char *identifier (NULL when there is none)\n    pub identifier: Option<&'static str>,\n    /// C: cmdp_state next_state — this might be __CALL\n    pub next_state: {},\n    /// C: union {{ uint16_t call_identifier; }} extra\n    pub call_identifier: u16,\n}}\n\n",
        token_name, enum_name
    ));
    for state in &model.keys {
        out.push_str(&format!(
            "// C: GENERATED_{}_tokens.h — static cmdp_token tokens_{}[]\nstatic TOKENS_{}: &[{}] = &[\n",
            model.prefix, state, state, token_name
        ));
        for token in &model.tokens[state] {
            let (next_state, call_identifier) = if token.next_state.starts_with("call ") {
                (
                    format!("{}::__CALL", enum_name),
                    token.next_state["call ".len()..].to_string(),
                )
            } else {
                (format!("{}::{}", enum_name, token.next_state), "0".to_string())
            };
            // C: perl:219-224 — literals keep the leading quote, lose the
            // trailing one; double quotes are escaped (rust_str_lit).
            let mut name = token.token.clone();
            if name.starts_with('\'') {
                name = name.strip_suffix('\'').unwrap_or(&name).to_string();
            }
            let identifier = match &token.identifier {
                Some(id) => format!("Some({})", rust_str_lit(id)),
                None => "None".to_string(),
            };
            out.push_str(&format!(
                "    {} {{ name: {}, identifier: {}, next_state: {}, call_identifier: {} }},\n",
                token_name,
                rust_str_lit(&name),
                identifier,
                next_state,
                call_identifier
            ));
        }
        out.push_str("];\n\n");
    }
    // C: GENERATED_<prefix>_tokens.h:245-250 — static cmdp_token_ptr tokens[]
    out.push_str(&format!(
        "/// C: GENERATED_{}_tokens.h:245-250 — static cmdp_token_ptr tokens[],\n/// indexed by state in C; dispatched on the state enum here. Indexing with\n/// __CALL reads out of bounds in C — the panic makes that bug loud.\npub fn {}(state: {}) -> &'static [{}] {{\n    match state {{\n",
        model.prefix, accessor, enum_name, token_name
    ));
    for state in &model.keys {
        out.push_str(&format!(
            "            {}::{} => TOKENS_{},\n",
            enum_name, state, state
        ));
    }
    out.push_str(&format!(
        "            {}::__CALL => panic!(\"BUG in the parser. state = {{}}\", state as i32),\n        }}\n    }}\n\n",
        enum_name
    ));
}

/// Rust keywords are escaped with the raw identifier syntax so that C
/// parameter names like type survive.
fn escape_ident(name: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "type", "ref", "fn", "box", "in", "if", "else", "match", "for", "while", "loop", "move",
        "mut", "pub", "use", "mod", "where", "as", "break", "continue", "return", "let", "const",
        "static", "struct", "enum", "trait", "impl", "dyn", "unsafe", "extern", "true", "false",
        "self", "super", "crate",
    ];
    if KEYWORDS.contains(&name) {
        format!("r#{}", name)
    } else {
        name.to_string()
    }
}

/// Argument expression for the real (non-test_parser) hook call.
fn real_arg_expr(arg: &ArgSpec, enum_name: &str, param_ty: ParamType) -> String {
    match arg {
        ArgSpec::StackString(id) => format!("arg_{}.as_deref()", id),
        ArgSpec::StackLong(id) => format!("arg_{}", id),
        ArgSpec::Null => "None".to_string(),
        ArgSpec::Str(s) => format!("Some({})", rust_str_lit(s)),
        // C: a literal 0 in a const char * position is the null pointer
        // constant (see Model::build).
        ArgSpec::Num(0) if param_ty == ParamType::Str => "None".to_string(),
        ArgSpec::Num(n) => format!("{}", n),
        ArgSpec::State(st) => format!("{}::{} as i32", enum_name, st),
    }
}

/// (format part, argument expression) for the test_parser eprintln! trace.
/// C: perl:156-190 — the debug format uses %d for numbers and state ids, %s
/// for strings; literal NULL arguments stay in the format string while the
/// argument is dropped (perl:189: s/, NULL//g).
fn debug_arg_parts(arg: &ArgSpec, enum_name: &str) -> (String, Option<String>) {
    match arg {
        // Borrow instead of moving the Option<String> out of the arm: the
        // cfg_criteria_* arms print the trace *and* pass the same arguments
        // to the hook afterwards (generate-command-parser.pl:188-195), which
        // does not compile with a moved value.
        ArgSpec::StackString(id) => (
            "{}".to_string(),
            Some(format!("arg_{}.as_deref().unwrap_or(\"(null)\")", id)),
        ),
        ArgSpec::StackLong(id) => ("{}".to_string(), Some(format!("arg_{}", id))),
        ArgSpec::Null => ("NULL".to_string(), None),
        ArgSpec::Str(s) => ("{}".to_string(), Some(rust_str_lit(s))),
        ArgSpec::Num(n) => ("{}".to_string(), Some(format!("{}", n))),
        ArgSpec::State(st) => (
            "{}".to_string(),
            Some(format!("{}::{} as i32", enum_name, st)),
        ),
    }
}

fn emit_arm_prelude(out: &mut String, model: &Model, call: &CallSpec) {
    out.push_str(&format!("        {} => {{\n", call.call_id));
    out.push_str(&format!(
        "            // C: parser-specs/{}.spec — {}\n",
        model.prefix, call.spec_text
    ));
}

fn emit_stack_arg_lets(out: &mut String, call: &CallSpec) {
    for arg in &call.args {
        match arg {
            ArgSpec::StackString(id) => out.push_str(&format!(
                "            let arg_{} = parser_get_string(&ctx.stack, {}).map(str::to_owned);\n",
                id,
                rust_str_lit(id)
            )),
            ArgSpec::StackLong(id) => out.push_str(&format!(
                "            let arg_{} = parser_get_long(&ctx.stack, {});\n",
                id,
                rust_str_lit(id)
            )),
            _ => {}
        }
    }
}

/// Emit GENERATED_call for the commands spec (C: GENERATED_command_call.h,
/// perl:133-207).
fn emit_commands_call(out: &mut String, model: &Model) {
    out.push_str(&format!(
        "/// C: GENERATED_{}_call.h — GENERATED_call() (generate-command-parser.pl:133-207).\n///\n/// The C function receives (Match *current_match, struct stack *stack,\n/// const int call_identifier, struct CommandResultIR *result) as separate\n/// pointers, with result->ctx pointing back into the cmd_parser_ctx. The\n/// Rust port passes the whole CommandParserCtx instead: current_match is\n/// ctx.current_match, the stack is ctx.stack and the result IR is\n/// ctx.subcommand_output (see next_state() in commands_parser.rs). The\n/// json_gen/client members of the C CommandResultIR live on the\n/// CommandHooks implementor.\n///\n/// With the test_parser feature (C: -DTEST_PARSER) the call sequence is\n/// logged to stderr instead of dispatching to the handlers.\n#[allow(unused_variables)]\npub fn command_generated_call<H: CommandHooks>(\n    hooks: &mut H,\n    ctx: &mut CommandParserCtx<H::Match, H::Con>,\n    call_identifier: u16,\n) {{\n    match call_identifier {{\n",
        model.prefix
    ));
    for call in &model.calls {
        emit_arm_prelude(out, model, call);
        out.push_str(&format!(
            "            ctx.subcommand_output.next_state = CommandState::{} as i32;\n",
            call.next_state
        ));
        emit_stack_arg_lets(out, call);
        let real_args: Vec<String> = call
            .args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                real_arg_expr(a, "CommandState", model.param_types[&(call.func.clone(), i)])
            })
            .collect();
        out.push_str(&format!(
            "            #[cfg(not(feature = \"test_parser\"))]\n            hooks.{}(ctx{}{});\n",
            call.func,
            if real_args.is_empty() { "" } else { ", " },
            real_args.join(", ")
        ));
        let mut fmt_parts: Vec<String> = Vec::new();
        let mut debug_args: Vec<String> = Vec::new();
        for arg in &call.args {
            let (fmt, expr) = debug_arg_parts(arg, "CommandState");
            fmt_parts.push(fmt);
            if let Some(e) = expr {
                debug_args.push(e);
            }
        }
        let fmt_str = format!("{}({})", call.func, fmt_parts.join(", "));
        if debug_args.is_empty() {
            out.push_str(&format!(
                "            #[cfg(feature = \"test_parser\")]\n            eprintln!({});\n",
                rust_str_lit(&fmt_str)
            ));
        } else {
            out.push_str(&format!(
                "            #[cfg(feature = \"test_parser\")]\n            eprintln!({}, {});\n",
                rust_str_lit(&fmt_str),
                debug_args.join(", ")
            ));
        }
        out.push_str("        }\n");
    }
    out.push_str(&format!(
        "        _ => panic!(\"BUG in the parser. state = {{}}\", call_identifier),\n    }}\n}}\n\n"
    ));
}

/// Emit GENERATED_call for the config spec (C: GENERATED_config_call.h,
/// perl:133-207). The config runtime (config_parser.c) creates a fresh local
/// ConfigResultIR per call, so the result is a separate parameter here.
fn emit_config_call(out: &mut String, model: &Model) {
    out.push_str(&format!(
        "/// C: GENERATED_{}_call.h — GENERATED_call() (generate-command-parser.pl:133-207).\n///\n/// The C function receives (Match *current_match, struct stack *stack,\n/// const int call_identifier, struct ConfigResultIR *result) with\n/// result->ctx pointing back to the parser_ctx. The Rust port passes the\n/// parser ctx and the result IR separately (config_parser.c:81-94 creates a\n/// fresh local ConfigResultIR for every call).\n///\n/// With the test_parser feature (C: -DTEST_PARSER) the call sequence is\n/// logged to stderr; the cfg_criteria functions keep their side effects\n/// (generate-command-parser.pl:191-195).\n#[allow(unused_variables)]\npub fn config_generated_call<H: ConfigHooks>(\n    hooks: &mut H,\n    ctx: &mut ConfigParserCtx<H::Match>,\n    call_identifier: u16,\n    result: &mut ConfigResultIr,\n) {{\n    match call_identifier {{\n",
        model.prefix
    ));
    for call in &model.calls {
        emit_arm_prelude(out, model, call);
        out.push_str(&format!(
            "            result.next_state = ConfigState::{} as i32;\n",
            call.next_state
        ));
        emit_stack_arg_lets(out, call);
        let real_args: Vec<String> = call
            .args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                real_arg_expr(a, "ConfigState", model.param_types[&(call.func.clone(), i)])
            })
            .collect();
        let call_expr = format!(
            "hooks.{}(ctx, result{}{})",
            call.func,
            if real_args.is_empty() { "" } else { ", " },
            real_args.join(", ")
        );
        out.push_str(&format!(
            "            #[cfg(not(feature = \"test_parser\"))]\n            {};\n",
            call_expr
        ));
        let mut fmt_parts: Vec<String> = Vec::new();
        let mut debug_args: Vec<String> = Vec::new();
        for arg in &call.args {
            let (fmt, expr) = debug_arg_parts(arg, "ConfigState");
            fmt_parts.push(fmt);
            if let Some(e) = expr {
                debug_args.push(e);
            }
        }
        let fmt_str = format!("{}({})", call.func, fmt_parts.join(", "));
        let is_criteria = call.func.starts_with("cfg_criteria");
        let eprintln_expr = if debug_args.is_empty() {
            format!("eprintln!({})", rust_str_lit(&fmt_str))
        } else {
            format!("eprintln!({}, {})", rust_str_lit(&fmt_str), debug_args.join(", "))
        };
        out.push_str(&format!(
            "            #[cfg(feature = \"test_parser\")]\n            {{\n                {};\n",
            eprintln_expr
        ));
        if is_criteria {
            out.push_str(&format!(
                "                // C: generate-command-parser.pl:191-195 — the cfg_criteria functions\n                // have side effects which are important for testing.\n                {};\n",
                call_expr
            ));
        }
        out.push_str("            }\n");
        out.push_str("        }\n");
    }
    out.push_str(&format!(
        "        _ => panic!(\"BUG in the parser. state = {{}}\", call_identifier),\n    }}\n}}\n\n"
    ));
}

fn emit_hook_signature(
    out: &mut String,
    func: &FuncSpec,
    proto_lines: &[(&str, u32)],
    impl_file: &str,
    extra_params: &str,
) {
    let line = proto_lines
        .iter()
        .find(|(n, _)| *n == func.name)
        .map(|(_, l)| *l);
    match line {
        Some(l) => out.push_str(&format!(
            "    /// C: include/{} — {} (prototype line {}, implementation in src/{}).\n",
            if impl_file == "commands.c" { "commands.h" } else { "config_directives.h" },
            func.name,
            l,
            impl_file
        )),
        None => out.push_str(&format!(
            "    /// C: src/{} — {}.\n",
            impl_file, func.name
        )),
    }
    if !func.forms.is_empty() {
        out.push_str(&format!(
            "    /// Spec call forms: {}.\n",
            func.forms.join(" | ")
        ));
    }
    out.push_str(&format!("    fn {}(\n        &mut self,\n", func.name));
    if !extra_params.is_empty() {
        out.push_str(&format!("        {},\n", extra_params));
    }
    for (i, p) in func.params.iter().enumerate() {
        let comma = if i + 1 < func.params.len() { "," } else { "" };
        out.push_str(&format!(
            "        {}: {}{}\n",
            escape_ident(&p.name),
            p.ty.rust_type(),
            comma
        ));
    }
    out.push_str("    );\n");
}

/// Emit the CommandHooks trait (the C cmd_* functions of src/commands.c).
fn emit_commands_trait(out: &mut String, model: &Model) {
    out.push_str(
        "/// Dispatch interface for the call targets of parser-specs/commands.spec.\n///\n/// C: the cmd_* functions live in src/commands.c (prototypes in\n/// include/commands.h) and are called from GENERATED_command_call.h. The\n/// Rust port turns them into this trait, which i3-core implements.\n///\n/// C passes (Match *current_match, struct CommandResultIR *cmd_output) to\n/// every cmd_ function, with cmd_output->ctx pointing back to the whole\n/// cmd_parser_ctx. The Rust methods receive the CommandParserCtx instead:\n/// current_match is ctx.current_match, the result IR is\n/// ctx.subcommand_output (the handler can set next_state on it, like\n/// cfg_criteria_pop_state() does on the config side) and the ctx backpointer\n/// is the ctx parameter itself (C handlers reach ctx->owindows through it).\npub trait CommandHooks {\n    /// C: include/match.h Match — the criteria match type. Zero-initialised\n    /// in C (commands_parser.c:160 struct cmd_parser_ctx cmd_ctx = {0}),\n    /// Default-constructed here.\n    type Match: Default;\n    /// C: include/data.h Con — the container stored in Owindow.\n    type Con;\n\n    // C: commands_parser.c:29-31 — the y()/ystr() yajl wrapper macros. The C\n    // macros are no-ops when json_gen is NULL; the default bodies mirror\n    // that, implementors override them when a json reply is required.\n    /// C: #define y(x, ...) — yajl_gen_array_open\n    fn y_array_open(&mut self) {}\n    /// C: #define y(x, ...) — yajl_gen_array_close\n    fn y_array_close(&mut self) {}\n    /// C: #define y(x, ...) — yajl_gen_map_open\n    fn y_map_open(&mut self) {}\n    /// C: #define y(x, ...) — yajl_gen_map_close\n    fn y_map_close(&mut self) {}\n    /// C: #define y(x, ...) — yajl_gen_bool\n    fn y_bool(&mut self, _bool: bool) {}\n    /// C: #define ystr(str) — yajl_gen_string\n    fn y_string(&mut self, _str: &str) {}\n\n",
    );
    for func in &model.functions {
        emit_hook_signature(
            out,
            func,
            CMD_PROTO_LINES,
            "commands.c",
            "ctx: &mut CommandParserCtx<Self::Match, Self::Con>",
        );
    }
    out.push_str("}\n");
}

/// Emit the ConfigHooks trait (the C cfg_* functions of
/// src/config_directives.c).
fn emit_config_trait(out: &mut String, model: &Model) {
    out.push_str(
        "/// Dispatch interface for the call targets of parser-specs/config.spec.\n///\n/// C: the cfg_* functions live in src/config_directives.c (prototypes in\n/// include/config_directives.h) and are called from GENERATED_config_call.h.\n/// The Rust port turns them into this trait, which i3-core implements.\n///\n/// C passes (Match *current_match, struct ConfigResultIR *result) to every\n/// cfg_ function, with result->ctx pointing back to the parser_ctx. The Rust\n/// methods receive the ConfigParserCtx and the result IR separately\n/// (config_parser.c:85-87 creates a fresh local ConfigResultIR per call and\n/// sets .ctx = ctx): current_match is ctx.current_match and the ctx\n/// backpointer is the ctx parameter (C handlers reach ctx->use_nagbar and\n/// ctx->variables through it).\npub trait ConfigHooks {\n    /// C: include/match.h Match — the criteria match type (Default here,\n    /// zero-initialised in C).\n    type Match: Default;\n\n",
    );
    for func in &model.functions {
        emit_hook_signature(
            out,
            func,
            CFG_PROTO_LINES,
            "config_directives.c",
            "ctx: &mut ConfigParserCtx<Self::Match>,\n        result: &mut ConfigResultIr",
        );
    }
    out.push_str("}\n");
}

/// Emit the dummy CommandHooks implementation used by the TEST_PARSER build
/// (C: commands_parser.c compiled with -DTEST_PARSER, driving
/// t/187-commands-parser.t).
///
/// C compiles src/commands.c out of the test parser build: GENERATED_call
/// only prints the call to stderr and never runs a cmd_* handler. The Rust
/// trait still needs an implementor for the generic parse_command(), so every
/// method is a no-op.
fn emit_commands_test_hooks(out: &mut String, model: &Model) {
    out.push_str(
        "/// Dummy CommandHooks implementor of the TEST_PARSER build (C:\n/// commands_parser.c with -DTEST_PARSER — the cmd_* handlers are compiled\n/// out, GENERATED_call only logs the call to stderr).\n#[cfg(feature = \"test_parser\")]\n#[derive(Default)]\npub struct TestParserHooks;\n\n#[cfg(feature = \"test_parser\")]\nimpl CommandHooks for TestParserHooks {\n    type Match = ();\n    type Con = ();\n",
    );
    for func in &model.functions {
        out.push_str("    #[allow(unused_variables)]\n    fn ");
        out.push_str(&func.name);
        out.push_str("(\n        &mut self,\n        ctx: &mut CommandParserCtx<(), ()>");
        for p in &func.params {
            out.push_str(&format!(",\n        {}: {}", escape_ident(&p.name), p.ty.rust_type()));
        }
        out.push_str(",\n    ) {\n    }\n");
    }
    out.push_str("}\n\n");
}

/// Emit the dummy ConfigHooks implementation used by the TEST_PARSER build
/// (C: config_parser.c compiled with -DTEST_PARSER, driving
/// t/201-config-parser.t).
///
/// The cfg_criteria functions keep their side effects (C:
/// config_parser.c:472-483): cfg_criteria_init() remembers the state the
/// criteria block returns to and cfg_criteria_pop_state() hands it back
/// through result->next_state, which drives the parser state machine. All
/// other cfg_* handlers are compiled out in C and are no-ops here.
fn emit_config_test_hooks(out: &mut String, model: &Model) {
    out.push_str(
        "/// Dummy ConfigHooks implementor of the TEST_PARSER build (C:\n/// config_parser.c with -DTEST_PARSER — the cfg_* handlers are stubbed,\n/// only the cfg_criteria_* functions keep their side effects,\n/// config_parser.c:472-483).\n#[cfg(feature = \"test_parser\")]\n#[derive(Default)]\npub struct TestParserHooks {\n    /// C: config_parser.c:472 — static int criteria_next_state\n    pub criteria_next_state: i32,\n}\n\n#[cfg(feature = \"test_parser\")]\nimpl ConfigHooks for TestParserHooks {\n    type Match = ();\n",
    );
    for func in &model.functions {
        out.push_str("    #[allow(unused_variables)]\n    fn ");
        out.push_str(&func.name);
        out.push_str("(\n        &mut self,\n        ctx: &mut ConfigParserCtx<()>,\n        result: &mut ConfigResultIr");
        for p in &func.params {
            out.push_str(&format!(",\n        {}: {}", escape_ident(&p.name), p.ty.rust_type()));
        }
        out.push_str(",\n    ) {\n");
        match func.name.as_str() {
            // C: config_parser.c:474-476 — cfg_criteria_init(I3_CFG, int _state)
            // { criteria_next_state = _state; }
            "cfg_criteria_init" => {
                let state = func
                    .params
                    .first()
                    .map(|p| escape_ident(&p.name))
                    .unwrap_or_else(|| "0".to_string());
                out.push_str(&format!(
                    "        self.criteria_next_state = {};\n",
                    state
                ));
            }
            // C: config_parser.c:480-482 — cfg_criteria_pop_state(I3_CFG)
            // { result->next_state = criteria_next_state; }
            "cfg_criteria_pop_state" => {
                out.push_str("        result.next_state = self.criteria_next_state;\n");
            }
            _ => {}
        }
        out.push_str("    }\n");
    }
    out.push_str("}\n\n");
}

fn emit_commands(model: &Model) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "// @generated by build.rs — DO NOT EDIT.\n// Generated from parser-specs/{}.spec exactly like i3's\n// generate-command-parser.pl (i3 4.25) generates GENERATED_{}_enums.h,\n// GENERATED_{}_call.h and GENERATED_{}_tokens.h.\n\n",
        model.prefix, model.prefix, model.prefix, model.prefix
    ));
    emit_enum(&mut out, model, "CommandState");
    emit_tokens(
        &mut out,
        model,
        "CommandState",
        "CommandToken",
        "command_tokens",
    );
    emit_commands_call(&mut out, model);
    emit_commands_trait(&mut out, model);
    emit_commands_test_hooks(&mut out, model);
    out
}

fn emit_config(model: &Model) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "// @generated by build.rs — DO NOT EDIT.\n// Generated from parser-specs/{}.spec exactly like i3's\n// generate-command-parser.pl (i3 4.25) generates GENERATED_{}_enums.h,\n// GENERATED_{}_call.h and GENERATED_{}_tokens.h.\n\n",
        model.prefix, model.prefix, model.prefix, model.prefix
    ));
    emit_enum(&mut out, model, "ConfigState");
    emit_tokens(&mut out, model, "ConfigState", "ConfigToken", "config_tokens");
    emit_config_call(&mut out, model);
    emit_config_trait(&mut out, model);
    emit_config_test_hooks(&mut out, model);
    out
}
