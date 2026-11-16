//! C: src/config_parser.c — handwritten parser to parse configuration
//! directives, ported 1:1.
//!
//! See also src/commands_parser.c (ported as commands_parser.rs) for the
//! rationale of the custom parser. The differences of the config parser:
//!
//! 1. It supports the 'number' token type (in addition to 'word' and
//!    'string'). Numbers are referred to using &num (like $str).
//! 2. Criteria are not executed immediately, they are just stored.
//! 3. It recognizes \n and \r as 'end' token, while commands_parser
//!    ignores them.
//! 4. It skips the current line on invalid inputs and follows the nearest
//!    <error> token.
//!
//! The generated parts (state enum, token tables, call dispatch and the
//! ConfigHooks trait) come from parser-specs/config.spec via build.rs, the
//! 1:1 port of generate-command-parser.pl, and are included at the end of
//! this module.
//!
//! The parts of src/config_parser.c that need file I/O and the i3 log
//! (parse_file, the variable substitution and the nagbar startup) live in
//! i3-core (config.rs), which drives parse_config below.

use crate::parser_util::{
    parser_clear_stack, parser_get_long, parser_get_string, parser_push_long,
    parser_push_string, Stack,
};

/// C: include/config_parser.h:76-80 — parse_file_result_t. The result of a
/// parse_file() call (parse_file itself lives in i3-core/config.rs since it
/// reads the file and substitutes the variables).
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseFileResult {
    PARSE_FILE_FAILED = -1,
    PARSE_FILE_SUCCESS = 0,
    PARSE_FILE_CONFIG_ERRORS = 1,
}
pub use ParseFileResult as parse_file_result_t;

/// C: include/configuration.h:67 — struct Variable. Holds a user-assigned
/// variable ($key, without the dollar sign — C keeps it in the key) for
/// parsing the configuration file; the key is replaced by value in every
/// following line of the file. The next_match member of the C struct is
/// parser-internal scratch space used by the substitution routine, which
/// lives in i3-core (config.rs), so it is not a field here.
#[derive(Clone, Debug)]
pub struct Variable {
    /// C: char *key — the "$name" including the dollar sign.
    pub key: String,
    /// C: char *value
    pub value: String,
}
pub use Variable as variable_t;

/// C: include/configuration.h:34 — struct context. Used during the config
/// file lexing/parsing to keep the state of the lexer in order to provide
/// useful error messages. The line_number/line_copy/compact_error/
/// first_column/last_column members are leftovers of the bison-based parser
/// that config_parser.c replaced (only has_errors and filename are read by
/// the hand-written parser); they are kept for the 1:1 struct shape.
///
/// C reports parse errors through ELOG(), which is a link-time sink of the
/// embedding binary; the Rust port collects the formatted lines in error_log
/// so that i3-core (and tests) can log or assert on them.
#[derive(Clone, Debug, Default)]
pub struct ParserContext {
    /// C: bool has_errors
    pub has_errors: bool,
    /// C: bool has_warnings
    pub has_warnings: bool,
    /// C: int line_number (bison-era, unused by config_parser.c)
    pub line_number: i32,
    /// C: char *line_copy (bison-era, unused by config_parser.c)
    pub line_copy: Option<String>,
    /// C: const char *filename
    pub filename: String,
    /// C: char *compact_error (bison-era, unused by config_parser.c)
    pub compact_error: Option<String>,
    /// C: int first_column (bison-era, unused by config_parser.c)
    pub first_column: i32,
    /// C: int last_column (bison-era, unused by config_parser.c)
    pub last_column: i32,
    /// The formatted ELOG("CONFIG: ...") lines produced by parse_config's
    /// error reporting. C prints them directly; the log sink lives in
    /// i3-core (src/log.c), so they are collected here.
    pub error_log: Vec<String>,
}
pub use ParserContext as context_t;

/// C: include/config_parser.h:43-57 — struct ConfigResultIR. An intermediate
/// representation of the result of a parse_config call.
///
/// The ctx member of the C struct is not a field here: C stored a pointer
/// back to the parser_ctx, which the Rust port expresses by passing the
/// ConfigParserCtx as a separate argument to every ConfigHooks method —
/// see config_generated_call() in the generated code included below.
#[derive(Clone, Default, Debug)]
pub struct ConfigResultIr {
    /// C: int next_state — the next state to transition to. Passed to the
    /// function so that we can determine the next state as a result of a
    /// function call, like cfg_criteria_pop_state() does.
    pub next_state: i32,
    /// C: bool has_errors — whether any error happened while processing
    /// this config directive.
    pub has_errors: bool,
}

/// C: include/config_parser.h:17-42 — struct parser_ctx. Context structure
/// for the config parser, making it re-entrant.
///
/// The C keeps the stack behind a pointer (struct stack *stack); the Rust
/// port owns it. The variables member (C: struct variables_head variables,
/// the SLIST of defined variables used for substitution) is owned by the
/// ctx as well; cfg_include() moves it in and out of inner parse contexts
/// exactly like the C code copies the list head around.
pub struct ConfigParserCtx<M> {
    /// C: bool use_nagbar
    pub use_nagbar: bool,
    /// C: int state
    pub state: i32,
    /// C: Match current_match
    pub current_match: M,
    /// C: int statelist[10] — the states that lead to the current state.
    /// This list is used to process the nearest error token.
    pub statelist: [i32; 10],
    /// C: int statelist_idx — points to where the next entry will be
    /// inserted.
    pub statelist_idx: i32,
    /// C: struct stack *stack — the (small) stack where identified literals
    /// are stored during the parsing of a single config directive (like
    /// $workspace).
    pub stack: Stack,
    /// C: struct variables_head variables — the defined variables (C: an
    /// SLIST; a Vec here, ordered exactly like the C list).
    pub variables: Vec<Variable>,
    /// C: bool has_errors
    pub has_errors: bool,
}

impl<M: Default> Default for ConfigParserCtx<M> {
    fn default() -> Self {
        // C: struct parser_ctx ctx = { .use_nagbar = false, .stack = &stack };
        // in the TEST_PARSER main (config_parser.c:490-496) — zeroed.
        ConfigParserCtx {
            use_nagbar: false,
            state: 0,
            current_match: M::default(),
            statelist: [0; 10],
            statelist_idx: 0,
            stack: Stack::default(),
            variables: Vec::new(),
            has_errors: false,
        }
    }
}

/// Returns the byte at index i of buf, or 0 past the end. C dereferences the
/// NUL terminator of the input string, which the Rust byte slices do not
/// carry, so the sentinel is produced here.
fn at(buf: &[u8], i: usize) -> u8 {
    if i < buf.len() {
        buf[i]
    } else {
        0
    }
}

/// C: strncasecmp(walk, lit, n) == 0 — case-insensitive ASCII prefix match
/// of length lit.len() on the input at pos. The C input is NUL-terminated,
/// so a shorter remainder never matches.
fn strncasecmp_eq(buf: &[u8], pos: usize, lit: &str) -> bool {
    let litb = lit.as_bytes();
    if pos + litb.len() > buf.len() {
        return false;
    }
    buf[pos..pos + litb.len()].eq_ignore_ascii_case(litb)
}

/// C: strtol(walk, &end, 10) — decimal only, with the ERANGE behaviour of
/// config_parser.c:207-220 (out-of-range numbers and strings without a
/// leading number make the token fail). Returns (number, position of the
/// first non-number character) or None when no conversion could be
/// performed (C: end == walk).
fn strtol10(buf: &[u8], pos: usize) -> Option<(i64, usize)> {
    // strtol skips leading whitespace (already skipped by the parser, but
    // kept for 1:1).
    let mut i = pos;
    while matches!(at(buf, i), b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c') {
        i += 1;
    }
    let mut j = i;
    let mut neg = false;
    if at(buf, j) == b'+' || at(buf, j) == b'-' {
        neg = at(buf, j) == b'-';
        j += 1;
    }
    let digits_start = j;
    // Accumulate in i128 to detect the ERANGE overflow like C does.
    let mut acc: i128 = 0;
    while at(buf, j).is_ascii_digit() {
        acc = acc * 10 + (at(buf, j) - b'0') as i128;
        j += 1;
    }
    if j == digits_start {
        // No valid numbers found (C: end == walk).
        return None;
    }
    if neg {
        acc = -acc;
    }
    let num = i64::try_from(acc).ok()?;
    Some((num, j))
}

/// C: src/config_parser.c:81 next_state — advances the state machine. When
/// the token leads to __CALL, the generated call dispatch runs the handler
/// and the next state is whatever the handler left in the ConfigResultIR.
fn next_state<H: ConfigHooks>(
    token: &ConfigToken,
    ctx: &mut ConfigParserCtx<H::Match>,
    hooks: &mut H,
) {
    let mut _next_state = token.next_state as i32;

    if token.next_state == ConfigState::__CALL {
        // C: struct ConfigResultIR subcommand_output = { .ctx = ctx };
        let mut subcommand_output = ConfigResultIr::default();
        config_generated_call(hooks, ctx, token.call_identifier, &mut subcommand_output);
        if subcommand_output.has_errors {
            ctx.has_errors = true;
        }
        _next_state = subcommand_output.next_state;
        parser_clear_stack(&mut ctx.stack);
    }

    ctx.state = _next_state;
    if ctx.state == ConfigState::INITIAL as i32 {
        parser_clear_stack(&mut ctx.stack);
    }

    // See if we are jumping back to a state in which we were in previously
    // (statelist contains INITIAL) and just move statelist_idx accordingly.
    for i in 0..ctx.statelist_idx {
        if ctx.statelist[i as usize] != _next_state {
            continue;
        }
        ctx.statelist_idx = i + 1;
        return;
    }

    // Otherwise, the state is new and we add it to the list. C would index
    // statelist out of bounds here; Rust panics, which makes the bug loud
    // instead of undefined behaviour.
    ctx.statelist[ctx.statelist_idx as usize] = _next_state;
    ctx.statelist_idx += 1;
}

/// C: src/config_parser.c:120 — returns the index of the start of the line
/// (one byte after the previous \r or \n), or 0 if this is the first line.
/// walk may point up to two bytes before the input (C passes error_line - 2
/// at config_parser.c:385/388); a negative walk behaves like the C pointer
/// comparison and yields the start of the input.
fn start_of_line(buf: &[u8], walk: i64) -> usize {
    let mut w = walk;
    loop {
        if w < 0 {
            return 0;
        }
        let c = at(buf, w as usize);
        if c == b'\n' || c == b'\r' {
            return w as usize + 1;
        }
        if w == 0 {
            return 0;
        }
        w -= 1;
    }
}

/// C: src/config_parser.c:134 — copies the line starting at start and
/// terminates it at the next \n, if any. (C cuts at \n only, not \r.)
/// Freeing the result is automatic in Rust (the C caller has to free()).
fn single_line(buf: &[u8], start: usize) -> String {
    let mut end = start;
    while end < buf.len() && buf[end] != b'\n' {
        end += 1;
    }
    String::from_utf8_lossy(&buf[start..end]).into_owned()
}

/// C: include/log.h:29 — ELOG(fmt, ...) → errorlog("ERROR: " fmt, ...).
///
/// The Rust port collects every formatted line in ParserContext::error_log
/// (the log sink lives in i3-core, src/log.c). The TEST_PARSER build
/// (t/201-config-parser.t) captures stderr, so the line is also written
/// there immediately — printing at the end would break the interleaving
/// with the cfg_* call trace, which is part of what the test asserts.
macro_rules! elog {
    ($context:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        #[cfg(feature = "test_parser")]
        eprint!("ERROR: {}", msg);
        $context.error_log.push(msg);
    }};
}

/// C: src/config_parser.c:143 parse_config — parses the given config
/// directives input into the hooks. The context carries the filename and
/// error state (C: struct context *context); error reports are appended to
/// context.error_log (C: ELOG).
pub fn parse_config<H: ConfigHooks>(
    ctx: &mut ConfigParserCtx<H::Match>,
    hooks: &mut H,
    input: &str,
    context: &mut ParserContext,
) {
    // C: src/config_parser.c:144-158 — the whole config file is dumped into
    // the debug log, one line per DLOG message (a log message must not
    // exceed 4 KiB). DLOG lives in i3-core (src/log.c); skipped here.

    ctx.state = ConfigState::INITIAL as i32;
    for i in 0..10 {
        ctx.statelist[i] = ConfigState::INITIAL as i32;
    }
    ctx.statelist_idx = 1;

    let buf = input.as_bytes();
    let len = buf.len(); // C: const size_t len = strlen(input);
    let mut walk = 0usize; // C: const char *walk = input;
    let mut linecnt: i32 = 1; // C: int c; const cmdp_token *token; bool token_handled; linecnt = 1;

    // C: #ifndef TEST_PARSER
    //     struct ConfigResultIR subcommand_output = { .ctx = ctx };
    //     cfg_criteria_init(&(ctx->current_match), &subcommand_output, INITIAL);
    //   #endif
    #[cfg(not(feature = "test_parser"))]
    {
        let mut subcommand_output = ConfigResultIr::default();
        hooks.cfg_criteria_init(ctx, &mut subcommand_output, ConfigState::INITIAL as i32);
    }

    // The "<=" operator is intentional: We also handle the terminating 0-byte
    // explicitly by looking for an 'end' token. (C comment)
    while walk <= len {
        // Skip whitespace before every token, newlines are relevant since they
        // separate configuration directives.
        while at(buf, walk) == b' ' || at(buf, walk) == b'\t' {
            walk += 1;
        }

        // C: const cmdp_token_ptr *ptr = &(tokens[ctx->state]);
        let state = ConfigState::from_i32(ctx.state)
            .unwrap_or_else(|| panic!("BUG in the parser. state = {}", ctx.state));
        let ptr: &[ConfigToken] = config_tokens(state);
        let mut token_handled = false;
        for token in ptr {
            // A literal.
            if token.name.as_bytes().first() == Some(&b'\'') {
                if strncasecmp_eq(buf, walk, &token.name[1..]) {
                    if let Some(identifier) = token.identifier {
                        // C: parser_push_string(ctx->stack, token->identifier, token->name + 1);
                        parser_push_string(&mut ctx.stack, identifier, &token.name[1..]);
                    }
                    // C: walk += strlen(token->name) - 1;
                    walk += token.name.len() - 1;
                    next_state::<H>(token, ctx, hooks);
                    token_handled = true;
                    break;
                }
                continue;
            }

            if token.name == "number" {
                // Handle numbers. We only accept decimal numbers for now.
                // C: strtol + the ERANGE/errno checks (config_parser.c:207-220).
                match strtol10(buf, walk) {
                    Some((num, end)) => {
                        if let Some(identifier) = token.identifier {
                            parser_push_long(&mut ctx.stack, identifier, num);
                        }
                        // Set walk to the first non-number character.
                        walk = end;
                        next_state::<H>(token, ctx, hooks);
                        token_handled = true;
                        break;
                    }
                    None => continue,
                }
            }

            if token.name == "string" || token.name == "word" {
                let mut beginning = walk;
                // Handle quoted strings (or words).
                if at(buf, walk) == b'"' {
                    beginning += 1;
                    walk += 1;
                    while at(buf, walk) != b'\0' && (at(buf, walk) != b'"' || at(buf, walk - 1) == b'\\') {
                        walk += 1;
                    }
                } else {
                    if token.name.as_bytes()[0] == b's' {
                        while at(buf, walk) != b'\0' && at(buf, walk) != b'\r' && at(buf, walk) != b'\n' {
                            walk += 1;
                        }
                    } else {
                        // For a word, the delimiters are white space (' ' or
                        // '\t'), closing square bracket (]), comma (,) and
                        // semicolon (;).
                        while at(buf, walk) != b' '
                            && at(buf, walk) != b'\t'
                            && at(buf, walk) != b']'
                            && at(buf, walk) != b','
                            && at(buf, walk) != b';'
                            && at(buf, walk) != b'\r'
                            && at(buf, walk) != b'\n'
                            && at(buf, walk) != b'\0'
                        {
                            walk += 1;
                        }
                    }
                }
                if walk != beginning {
                    // We copy manually to handle escaping of characters. We
                    // only handle escaped double quotes to not break backwards
                    // compatibility with people using \w in regular
                    // expressions etc.
                    let mut out: Vec<u8> = Vec::new();
                    let mut inpos = beginning;
                    while inpos < walk {
                        if at(buf, inpos) == b'\\' && at(buf, inpos + 1) == b'"' {
                            inpos += 1;
                        }
                        out.push(at(buf, inpos));
                        inpos += 1;
                    }
                    let str = String::from_utf8_lossy(&out).into_owned();
                    if let Some(identifier) = token.identifier {
                        parser_push_string(&mut ctx.stack, identifier, &str);
                    }
                    // If we are at the end of a quoted string, skip the ending
                    // double quote.
                    if at(buf, walk) == b'"' {
                        walk += 1;
                    }
                    next_state::<H>(token, ctx, hooks);
                    token_handled = true;
                    break;
                }
            }

            if token.name == "line" {
                while at(buf, walk) != b'\0' && at(buf, walk) != b'\n' && at(buf, walk) != b'\r' {
                    walk += 1;
                }
                next_state::<H>(token, ctx, hooks);
                token_handled = true;
                linecnt += 1;
                walk += 1;
                break;
            }

            if token.name == "end" {
                if at(buf, walk) == b'\0' || at(buf, walk) == b'\n' || at(buf, walk) == b'\r' {
                    next_state::<H>(token, ctx, hooks);
                    token_handled = true;
                    // To make sure we start with an appropriate matching
                    // datastructure for commands which do *not* specify any
                    // criteria, we re-initialize the criteria system after
                    // every command.
                    #[cfg(not(feature = "test_parser"))]
                    {
                        let mut subcommand_output = ConfigResultIr::default();
                        hooks.cfg_criteria_init(ctx, &mut subcommand_output, ConfigState::INITIAL as i32);
                    }
                    linecnt += 1;
                    walk += 1;
                    break;
                }
            }
        }

        if !token_handled {
            // Build up a decent error message. We include the problem, the
            // full input, and underline the position where the parser
            // currently is.
            let mut possible_tokens = String::new();
            for (c, token) in ptr.iter().enumerate() {
                if token.name.as_bytes().first() == Some(&b'\'') {
                    // A literal is copied to the error message enclosed with
                    // single quotes.
                    possible_tokens.push('\'');
                    possible_tokens.push_str(&token.name[1..]);
                    possible_tokens.push('\'');
                } else {
                    // Skip error tokens in error messages, they are used
                    // internally only and might confuse users.
                    if token.name == "error" {
                        continue;
                    }
                    // Any other token is copied to the error message enclosed
                    // with angle brackets.
                    possible_tokens.push('<');
                    possible_tokens.push_str(token.name);
                    possible_tokens.push('>');
                }
                if c + 1 < ptr.len() {
                    possible_tokens.push_str(", ");
                }
            }
            // C: sasprintf(&errormessage, "Expected one of these tokens: %s",
            //    possible_tokens);
            let errormessage = format!("Expected one of these tokens: {}", possible_tokens);

            // Go back to the beginning of the line.
            let error_line = start_of_line(buf, walk as i64);

            // Contains the same amount of characters as the error line has,
            // but with the unparsable part highlighted using ^ characters.
            let mut position = String::new();
            let mut copywalk = error_line;
            while at(buf, copywalk) != b'\n' && at(buf, copywalk) != b'\r' && at(buf, copywalk) != b'\0' {
                position.push(if copywalk >= walk {
                    '^'
                } else if at(buf, copywalk) == b'\t' {
                    '\t'
                } else {
                    ' '
                });
                copywalk += 1;
            }

            // C: ELOG("%s\n", errormessage); ELOG("CONFIG: (in file %s)\n", ...);
            // — the i3 log lives in i3-core (src/log.c); the formatted lines
            // are collected in context.error_log (and printed to stderr by the
            // TEST_PARSER build, see the elog! macro).
            elog!(context, "CONFIG: {}\n", errormessage);
            elog!(context, "CONFIG: (in file {})\n", context.filename);

            let error_copy = single_line(buf, error_line);

            // Print context lines *before* the error, if any.
            if linecnt > 1 {
                let context_p1_start = start_of_line(buf, error_line as i64 - 2);
                let context_p1_line = single_line(buf, context_p1_start);
                if linecnt > 2 {
                    let context_p2_start = start_of_line(buf, context_p1_start as i64 - 2);
                    let context_p2_line = single_line(buf, context_p2_start);
                    elog!(
                        context,
                        "CONFIG: Line {:3}: {}\n",
                        linecnt - 2,
                        context_p2_line
                    );
                }
                elog!(
                    context,
                    "CONFIG: Line {:3}: {}\n",
                    linecnt - 1,
                    context_p1_line
                );
            }
            elog!(context, "CONFIG: Line {:3}: {}\n", linecnt, error_copy);
            elog!(context, "CONFIG:           {}\n", position);

            // Print context lines *after* the error, if any.
            let mut error_line = error_line;
            for i in 0..2 {
                // C: strchr(error_line, '\n')
                let error_line_end = buf
                    .get(error_line..)
                    .and_then(|rest| rest.iter().position(|&b| b == b'\n'))
                    .map(|off| error_line + off);
                if let Some(end) = error_line_end {
                    if end + 1 < buf.len() {
                        // C: *(error_line_end + 1) != '\0'
                        error_line = end + 1;
                        let error_copy = single_line(buf, error_line);
                        elog!(
                            context,
                            "CONFIG: Line {:3}: {}\n",
                            linecnt + i + 1,
                            error_copy
                        );
                    }
                }
            }

            context.has_errors = true;

            // Skip the rest of this line, but continue parsing.
            while walk <= len && at(buf, walk) != b'\n' {
                walk += 1;
            }

            parser_clear_stack(&mut ctx.stack);

            // To figure out in which state to go (e.g. MODE or INITIAL),
            // we find the nearest state which contains an <error> token
            // and follow that one.
            let mut error_token_found = false;
            let mut i = ctx.statelist_idx - 1;
            while i >= 0 && !error_token_found {
                let errstate = ConfigState::from_i32(ctx.statelist[i as usize])
                    .unwrap_or_else(|| panic!("BUG in the parser. state = {}", ctx.statelist[i as usize]));
                let errptr: &[ConfigToken] = config_tokens(errstate);
                for j in 0..errptr.len() {
                    if errptr[j].name != "error" {
                        continue;
                    }
                    next_state::<H>(&errptr[j], ctx, hooks);
                    error_token_found = true;
                    break;
                }
                i -= 1;
            }

            assert!(error_token_found);
        }
    }
}

/// C: src/config_parser.c:599 free_variables — releases the memory of all
/// variables in ctx (Rust drops the Strings; clearing the Vec is equivalent).
pub fn free_variables<M>(ctx: &mut ConfigParserCtx<M>) {
    ctx.variables.clear();
}

// C: src/config_parser.c:81-100 — the GENERATED parts of the config parser
// (C: GENERATED_config_enums.h, GENERATED_config_tokens.h,
// GENERATED_config_call.h), built by build.rs from parser-specs/config.spec
// with the same logic as generate-command-parser.pl.
include!(concat!(env!("OUT_DIR"), "/generated_config.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only ConfigHooks implementation: records the cfg_* calls like
    /// the C TEST_PARSER build logs them (config_parser.c:447-500).
    struct Recorder {
        calls: Vec<String>,
        /// C: config_parser.c:472 static int criteria_next_state
        criteria_next_state: i32,
    }

    impl ConfigHooks for Recorder {
        type Match = ();

        #[allow(unused_variables)]
        fn cfg_force_display_urgency_hint(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, duration_ms: i64) {
            self.calls.push(format!("cfg_force_display_urgency_hint({:?})", duration_ms));
        }
        #[allow(unused_variables)]
        fn cfg_floating_maximum_size(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, width: i64, height: i64) {
            self.calls.push(format!("cfg_floating_maximum_size({:?}, {:?})", width, height));
        }
        #[allow(unused_variables)]
        fn cfg_floating_minimum_size(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, width: i64, height: i64) {
            self.calls.push(format!("cfg_floating_minimum_size({:?}, {:?})", width, height));
        }
        #[allow(unused_variables)]
        fn cfg_bar_strip_workspace_numbers(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_bar_strip_workspace_numbers({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_bar_binding_mode_indicator(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_bar_binding_mode_indicator({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_bar_workspace_min_width(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, width: i64) {
            self.calls.push(format!("cfg_bar_workspace_min_width({:?})", width));
        }
        #[allow(unused_variables)]
        fn cfg_focus_on_window_activation(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, mode: Option<&str>) {
            self.calls.push(format!("cfg_focus_on_window_activation({:?})", mode));
        }
        #[allow(unused_variables)]
        fn cfg_tiling_drag_swap_modifier(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, modifiers: Option<&str>) {
            self.calls.push(format!("cfg_tiling_drag_swap_modifier({:?})", modifiers));
        }
        #[allow(unused_variables)]
        fn cfg_bar_strip_workspace_name(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_bar_strip_workspace_name({:?})", value));
        }
        fn cfg_criteria_add(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, ctype: Option<&str>, cvalue: Option<&str>) {
            self.calls.push(format!("cfg_criteria_add({:?}, {:?})", ctype, cvalue));
        }
        #[allow(unused_variables)]
        fn cfg_default_border(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, windowtype: Option<&str>, border: Option<&str>, width: i64) {
            self.calls.push(format!("cfg_default_border({:?}, {:?}, {:?})", windowtype, border, width));
        }
        #[allow(unused_variables)]
        fn cfg_workspace_back_and_forth(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_workspace_back_and_forth({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_assign(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, number: Option<&str>, is_number: i64) {
            self.calls.push(format!("cfg_assign({:?}, {:?})", number, is_number));
        }
        #[allow(unused_variables)]
        fn cfg_popup_during_fullscreen(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_popup_during_fullscreen({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_bar_workspace_buttons(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_bar_workspace_buttons({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_bar_workspace_command(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, command: Option<&str>) {
            self.calls.push(format!("cfg_bar_workspace_command({:?})", command));
        }
        #[allow(unused_variables)]
        fn cfg_workspace(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, workspace: Option<&str>, output: Option<&str>) {
            self.calls.push(format!("cfg_workspace({:?}, {:?})", workspace, output));
        }
        #[allow(unused_variables)]
        fn cfg_bar_separator_symbol(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, separator: Option<&str>) {
            self.calls.push(format!("cfg_bar_separator_symbol({:?})", separator));
        }
        #[allow(unused_variables)]
        fn cfg_force_focus_wrapping(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_force_focus_wrapping({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_bar_bindsym(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, button: Option<&str>, release: Option<&str>, command: Option<&str>) {
            self.calls.push(format!("cfg_bar_bindsym({:?}, {:?}, {:?})", button, release, command));
        }
        #[allow(unused_variables)]
        fn cfg_bar_tray_padding(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, padding_px: i64) {
            self.calls.push(format!("cfg_bar_tray_padding({:?})", padding_px));
        }
        #[allow(unused_variables)]
        fn cfg_default_orientation(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, orientation: Option<&str>) {
            self.calls.push(format!("cfg_default_orientation({:?})", orientation));
        }
        #[allow(unused_variables)]
        fn cfg_focus_follows_mouse(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_focus_follows_mouse({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_bar_padding_three(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, top_or_all: i64, right_or_right_and_left: i64, bottom: i64) {
            self.calls.push(format!("cfg_bar_padding_three({:?}, {:?}, {:?})", top_or_all, right_or_right_and_left, bottom));
        }
        #[allow(unused_variables)]
        fn cfg_bar_status_command(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, command: Option<&str>) {
            self.calls.push(format!("cfg_bar_status_command({:?})", command));
        }
        #[allow(unused_variables)]
        fn cfg_bar_wheel_down_cmd(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, command: Option<&str>) {
            self.calls.push(format!("cfg_bar_wheel_down_cmd({:?})", command));
        }
        #[allow(unused_variables)]
        fn cfg_color(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, colorclass: Option<&str>, border: Option<&str>, background: Option<&str>, text: Option<&str>, indicator: Option<&str>, child_border: Option<&str>) {
            self.calls.push(format!("cfg_color({:?}, {:?}, {:?}, {:?}, {:?}, {:?})", colorclass, border, background, text, indicator, child_border));
        }
        #[allow(unused_variables)]
        fn cfg_for_window(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, command: Option<&str>) {
            self.calls.push(format!("cfg_for_window({:?})", command));
        }
        #[allow(unused_variables)]
        fn cfg_bar_color_single(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, colorclass: Option<&str>, color: Option<&str>) {
            self.calls.push(format!("cfg_bar_color_single({:?}, {:?})", colorclass, color));
        }
        #[allow(unused_variables)]
        fn cfg_bar_padding_two(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, top_or_all: i64, right_or_right_and_left: i64) {
            self.calls.push(format!("cfg_bar_padding_two({:?}, {:?})", top_or_all, right_or_right_and_left));
        }
        #[allow(unused_variables)]
        fn cfg_floating_modifier(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, modifiers: Option<&str>) {
            self.calls.push(format!("cfg_floating_modifier({:?})", modifiers));
        }
        #[allow(unused_variables)]
        fn cfg_hide_edge_borders(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, hide_borders: Option<&str>) {
            self.calls.push(format!("cfg_hide_edge_borders({:?})", hide_borders));
        }
        #[allow(unused_variables)]
        fn cfg_bar_hidden_state(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, hidden_state: Option<&str>) {
            self.calls.push(format!("cfg_bar_hidden_state({:?})", hidden_state));
        }
        #[allow(unused_variables)]
        fn cfg_bar_padding_four(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, top_or_all: i64, right_or_right_and_left: i64, bottom: i64, left: i64) {
            self.calls.push(format!("cfg_bar_padding_four({:?}, {:?}, {:?}, {:?})", top_or_all, right_or_right_and_left, bottom, left));
        }
        #[allow(unused_variables)]
        fn cfg_bar_wheel_up_cmd(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, command: Option<&str>) {
            self.calls.push(format!("cfg_bar_wheel_up_cmd({:?})", command));
        }
        #[allow(unused_variables)]
        fn cfg_ipc_kill_timeout(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, timeout: i64) {
            self.calls.push(format!("cfg_ipc_kill_timeout({:?})", timeout));
        }
        #[allow(unused_variables)]
        fn cfg_mode_binding(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, bindtype: Option<&str>, modifiers: Option<&str>, key: Option<&str>, release: Option<&str>, border: Option<&str>, whole_window: Option<&str>, exclude_titlebar: Option<&str>, command: Option<&str>) {
            self.calls.push(format!("cfg_mode_binding({:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?})", bindtype, modifiers, key, release, border, whole_window, exclude_titlebar, command));
        }
        #[allow(unused_variables)]
        fn cfg_tiling_drag(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_tiling_drag({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_workspace_layout(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, layout: Option<&str>) {
            self.calls.push(format!("cfg_workspace_layout({:?})", layout));
        }
        #[allow(unused_variables)]
        fn cfg_bar_i3bar_command(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, command: Option<&str>) {
            self.calls.push(format!("cfg_bar_i3bar_command({:?})", command));
        }
        #[allow(unused_variables)]
        fn cfg_bar_color(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, colorclass: Option<&str>, border: Option<&str>, background: Option<&str>, text: Option<&str>) {
            self.calls.push(format!("cfg_bar_color({:?}, {:?}, {:?}, {:?})", colorclass, border, background, text));
        }
        #[allow(unused_variables)]
        fn cfg_bar_padding_one(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, top_or_all: i64) {
            self.calls.push(format!("cfg_bar_padding_one({:?})", top_or_all));
        }
        #[allow(unused_variables)]
        fn cfg_bar_socket_path(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, path: Option<&str>) {
            self.calls.push(format!("cfg_bar_socket_path({:?})", path));
        }
        #[allow(unused_variables)]
        fn cfg_bar_tray_output(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, output: Option<&str>) {
            self.calls.push(format!("cfg_bar_tray_output({:?})", output));
        }
        #[allow(unused_variables)]
        fn cfg_disable_randr15(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_disable_randr15({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_focus_wrapping(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_focus_wrapping({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_force_xinerama(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_force_xinerama({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_assign_output(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, output: Option<&str>) {
            self.calls.push(format!("cfg_assign_output({:?})", output));
        }
        #[allow(unused_variables)]
        fn cfg_mouse_warping(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_mouse_warping({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_restart_state(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, path: Option<&str>) {
            self.calls.push(format!("cfg_restart_state({:?})", path));
        }
        #[allow(unused_variables)]
        fn cfg_smart_borders(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, enabled: Option<&str>) {
            self.calls.push(format!("cfg_smart_borders({:?})", enabled));
        }
        #[allow(unused_variables)]
        fn cfg_bar_modifier(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, modifiers: Option<&str>) {
            self.calls.push(format!("cfg_bar_modifier({:?})", modifiers));
        }
        #[allow(unused_variables)]
        fn cfg_bar_position(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, position: Option<&str>) {
            self.calls.push(format!("cfg_bar_position({:?})", position));
        }
        #[allow(unused_variables)]
        fn cfg_color_single(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, colorclass: Option<&str>, color: Option<&str>) {
            self.calls.push(format!("cfg_color_single({:?}, {:?})", colorclass, color));
        }
        #[allow(unused_variables)]
        fn cfg_fake_outputs(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, outputs: Option<&str>) {
            self.calls.push(format!("cfg_fake_outputs({:?})", outputs));
        }
        #[allow(unused_variables)]
        fn cfg_no_focus(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr) {
            self.calls.push(format!("cfg_no_focus()", ));
        }
        #[allow(unused_variables)]
        fn cfg_bar_verbose(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_bar_verbose({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_binding(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, bindtype: Option<&str>, modifiers: Option<&str>, key: Option<&str>, release: Option<&str>, border: Option<&str>, whole_window: Option<&str>, exclude_titlebar: Option<&str>, command: Option<&str>) {
            self.calls.push(format!("cfg_binding({:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?})", bindtype, modifiers, key, release, border, whole_window, exclude_titlebar, command));
        }
        #[allow(unused_variables)]
        fn cfg_title_align(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, alignment: Option<&str>) {
            self.calls.push(format!("cfg_title_align({:?})", alignment));
        }
        #[allow(unused_variables)]
        fn cfg_bar_height(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: i64) {
            self.calls.push(format!("cfg_bar_height({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_bar_output(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, output: Option<&str>) {
            self.calls.push(format!("cfg_bar_output({:?})", output));
        }
        fn cfg_criteria_init(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, _state: i32) {
            // C: config_parser.c:474 cfg_criteria_init — stores the next state.
            self.criteria_next_state = _state;
        }
        #[allow(unused_variables)]
        fn cfg_ipc_socket(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, path: Option<&str>) {
            self.calls.push(format!("cfg_ipc_socket({:?})", path));
        }
        #[allow(unused_variables)]
        fn cfg_show_marks(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, value: Option<&str>) {
            self.calls.push(format!("cfg_show_marks({:?})", value));
        }
        #[allow(unused_variables)]
        fn cfg_smart_gaps(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, enabled: Option<&str>) {
            self.calls.push(format!("cfg_smart_gaps({:?})", enabled));
        }
        #[allow(unused_variables)]
        fn cfg_bar_start(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr) {
            self.calls.push(format!("cfg_bar_start()", ));
        }
        #[allow(unused_variables)]
        fn cfg_bar_font(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, font: Option<&str>) {
            self.calls.push(format!("cfg_bar_font({:?})", font));
        }
        #[allow(unused_variables)]
        fn cfg_bar_mode(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, mode: Option<&str>) {
            self.calls.push(format!("cfg_bar_mode({:?})", mode));
        }
        fn cfg_criteria_pop_state(&mut self, _ctx: &mut ConfigParserCtx<()>, result: &mut ConfigResultIr) {
            // C: config_parser.c:481 cfg_criteria_pop_state.
            result.next_state = self.criteria_next_state;
        }
        #[allow(unused_variables)]
        fn cfg_gaps(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, workspace: Option<&str>, scope: Option<&str>, value: i64) {
            self.calls.push(format!("cfg_gaps({:?}, {:?}, {:?})", workspace, scope, value));
        }
        #[allow(unused_variables)]
        fn cfg_enter_mode(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, pango_markup: Option<&str>, modename: Option<&str>) {
            self.calls.push(format!("cfg_enter_mode({:?}, {:?})", pango_markup, modename));
        }
        #[allow(unused_variables)]
        fn cfg_include(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, pattern: Option<&str>) {
            self.calls.push(format!("cfg_include({:?})", pattern));
        }
        #[allow(unused_variables)]
        fn cfg_bar_id(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, bar_id: Option<&str>) {
            self.calls.push(format!("cfg_bar_id({:?})", bar_id));
        }
        #[allow(unused_variables)]
        fn cfg_exec(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, exectype: Option<&str>, no_startup_id: Option<&str>, command: Option<&str>) {
            self.calls.push(format!("cfg_exec({:?}, {:?}, {:?})", exectype, no_startup_id, command));
        }
        #[allow(unused_variables)]
        fn cfg_font(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr, font: Option<&str>) {
            self.calls.push(format!("cfg_font({:?})", font));
        }
        #[allow(unused_variables)]
        fn cfg_bar_finish(&mut self, _ctx: &mut ConfigParserCtx<()>, _result: &mut ConfigResultIr) {
            self.calls.push(format!("cfg_bar_finish()", ));
        }
    }

    /// Runs parse_config like the C TEST_PARSER main (config_parser.c:485-500)
    /// and returns the recorded hook calls plus the parser context.
    fn parse(input: &str, filename: &str) -> (Recorder, ParserContext) {
        let mut ctx = ConfigParserCtx::<()>::default();
        let mut hooks = Recorder { calls: Vec::new(), criteria_next_state: 0 };
        let mut context = ParserContext {
            filename: filename.to_string(),
            ..Default::default()
        };
        parse_config(&mut ctx, &mut hooks, input, &mut context);
        (hooks, context)
    }

    /// C: parser-specs/config.spec — the spec has 117 state declarations and
    /// 93 call sites; assert the generated state machine preserves both.
    #[test]
    fn spec_sanity_counts() {
        // C: GENERATED_config_enums.h — __CALL is the sentinel value equal to
        // the number of states (117).
        assert_eq!(ConfigState::__CALL as i32, 117);

        let spec = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../i3-src/parser-specs/config.spec"
        ))
        .unwrap();
        let states = spec.lines().filter(|l| l.starts_with("state ")).count();
        let calls = spec.lines().filter(|l| l.contains("-> call cfg_")).count();
        assert_eq!(states, 117, "state declarations in config.spec");
        assert_eq!(calls, 93, "call sites in config.spec");
    }

    /// C: parse_config — a number/number directive reaches the handler.
    #[test]
    fn parse_simple_directive() {
        let (hooks, context) = parse("floating_minimum_size 10 x 20", "<stdin>");
        assert!(hooks.calls.contains(&"cfg_floating_minimum_size(10, 20)".to_string()));
        assert!(!context.has_errors);
    }

    /// C: literals are matched with strncasecmp — case-insensitive.
    #[test]
    fn parse_literal_case_insensitive() {
        let (hooks, context) = parse("FoNT pango:Terminus 8", "<stdin>");
        assert!(hooks.calls.contains(&"cfg_font(Some(\"pango:Terminus 8\"))".to_string()));
        assert!(!context.has_errors);
    }

    /// C: quoted strings support \" escaping (config_parser.c:240).
    #[test]
    fn parse_quoted_string_with_escape() {
        let (hooks, context) = parse("include \"some path\\\"with quotes\"", "<stdin>");
        assert!(hooks
            .calls
            .contains(&"cfg_include(Some(\"some path\\\"with quotes\"))".to_string()));
        assert!(!context.has_errors);
    }

    /// C: word tokens stop at whitespace, ], , and ; (config_parser.c:252).
    #[test]
    fn parse_criteria_word_token() {
        let (hooks, context) = parse("for_window [class=Firefox] echo hi", "<stdin>");
        assert!(hooks
            .calls
            .contains(&"cfg_criteria_add(Some(\"class\"), Some(\"Firefox\"))".to_string()));
        assert!(hooks.calls.contains(&"cfg_for_window(Some(\"echo hi\"))".to_string()));
        assert!(!context.has_errors);
    }

    /// C: config_parser.c:319-438 — the error report contains the expected
    /// token list, the filename, the numbered offending line and a caret
    /// position line; parsing continues after the error.
    #[test]
    fn parse_error_reports_line_number_and_recovers() {
        let input = "# comment\nfont pango:x\nbogus line\nfont pango:y\n";
        let (hooks, context) = parse(input, "test.conf");
        assert!(context.has_errors);

        let joined = context.error_log.concat();
        assert!(joined.contains("CONFIG: Expected one of these tokens: "));
        assert!(joined.contains("CONFIG: (in file test.conf)\n"));
        // context line before, the offending line with %3d formatting and the
        // caret line, then the following line.
        assert!(joined.contains("CONFIG: Line   2: font pango:x\n"));
        assert!(joined.contains("CONFIG: Line   3: bogus line\n"));
        assert!(joined.contains("CONFIG: Line   4: font pango:y\n"));
        assert!(joined.contains("CONFIG:           ^^^^^^^^^^\n"));
        // C: ELOG("CONFIG:           %s\n", position) — the position line
        // underlines from the start of the unparsable token onwards.
        // The font directive after the error is still parsed.
        assert!(hooks.calls.contains(&"cfg_font(Some(\"pango:x\"))".to_string()));
        assert!(hooks.calls.contains(&"cfg_font(Some(\"pango:y\"))".to_string()));
    }

    /// C: config_parser.c:212-220 — numbers overflowing long (ERANGE) make
    /// the token fail, so the whole line is a parse error.
    #[test]
    fn parse_number_overflow_fails() {
        let (hooks, context) = parse("floating_minimum_size 99999999999999999999 20\n", "<stdin>");
        assert!(context.has_errors);
        assert!(!hooks.calls.iter().any(|c| c.starts_with("cfg_floating_minimum_size(")));
    }

    /// C: config_parser.c:421-437 — after an error inside a mode block, the
    /// nearest <error> token leads back to the MODE state and the following
    /// lines of the mode are parsed.
    #[test]
    fn parse_error_inside_mode_recovers_to_mode() {
        let input = "mode \"foo\" {\nbindsym x exec bar\nbogusline\nbindsym y nop\n}\n";
        let (hooks, context) = parse(input, "<stdin>");
        assert!(context.has_errors);
        assert!(hooks
            .calls
            .iter()
            .any(|c| c.starts_with("cfg_mode_binding(") && c.contains("Some(\"x\")")));
        assert!(hooks
            .calls
            .iter()
            .any(|c| c.starts_with("cfg_mode_binding(") && c.contains("Some(\"y\")")));
        assert!(hooks
            .calls
            .contains(&"cfg_enter_mode(None, Some(\"foo\"))".to_string()));
    }

    /// C: config_parser.c:301-316 — the end token recognizes \\0, \\n and \\r.
    #[test]
    fn parse_cr_line_endings() {
        let (hooks, context) = parse("font pango:a\rfont pango:b", "<stdin>");
        assert!(hooks.calls.contains(&"cfg_font(Some(\"pango:a\"))".to_string()));
        assert!(hooks.calls.contains(&"cfg_font(Some(\"pango:b\"))".to_string()));
        assert!(!context.has_errors);
    }

    /// C: config_parser.c:323-329 — when the error is on the first line there
    /// is no context line before it; the line number starts at 1.
    #[test]
    fn parse_error_on_first_line_has_no_preceding_context() {
        let (hooks, context) = parse("bogus line\nfont pango:x\n", "<stdin>");
        assert!(context.has_errors);
        let joined = context.error_log.concat();
        assert!(joined.contains("CONFIG: Line   1: bogus line\n"), "{}", joined);
        assert!(joined.contains("CONFIG: Line   2: font pango:x\n"), "{}", joined);
        // No "Line   0" / negative line numbers.
        assert!(!joined.contains("Line   0:"), "{}", joined);
        // The remaining directive is still parsed.
        assert!(hooks.calls.contains(&"cfg_font(Some(\"pango:x\"))".to_string()));
    }

    /// C: config_parser.c:325-339 — two lines of context are printed before
    /// the offending line (numbered linecnt-2 and linecnt-1), then the error
    /// line itself, then at most two lines after it.
    #[test]
    fn parse_error_prints_two_context_lines_around() {
        let input = "font pango:a\nfont pango:b\nfont pango:c\nbogus line\nfont pango:d\nfont pango:e\nfont pango:f\n";
        let (_hooks, context) = parse(input, "<stdin>");
        assert!(context.has_errors);
        let joined = context.error_log.concat();
        assert!(joined.contains("CONFIG: Line   2: font pango:b\n"), "{}", joined);
        assert!(joined.contains("CONFIG: Line   3: font pango:c\n"), "{}", joined);
        assert!(joined.contains("CONFIG: Line   4: bogus line\n"), "{}", joined);
        // Only two lines of trailing context.
        assert!(joined.contains("CONFIG: Line   5: font pango:d\n"), "{}", joined);
        assert!(joined.contains("CONFIG: Line   6: font pango:e\n"), "{}", joined);
        assert!(!joined.contains("font pango:f"), "{}", joined);
        // Line 1 is outside the two-line window.
        assert!(!joined.contains("Line   1:"), "{}", joined);
    }

    /// C: config_parser.c:343-361 — the caret line marks the unparsable token
    /// to the end of the line and leaves the rest of the line blank.
    #[test]
    fn parse_error_caret_line_covers_the_token() {
        let (_hooks, context) = parse("font pango:x\nbogus stuff\n", "<stdin>");
        let joined = context.error_log.concat();
        // "bogus stuff" is five characters of directive + the rest of the line.
        assert!(joined.contains("CONFIG:           ^^^^^^^^^^^\n"), "{}", joined);
    }

    /// C: config_parser.c:319-321 — the expected-token list of the INITIAL
    /// state starts with the first literal of the spec ("set", ...), and the
    /// error is reported for the given filename.
    #[test]
    fn parse_error_lists_tokens_and_filename() {
        let (_hooks, context) = parse("bogus\n", "some/file.conf");
        let joined = context.error_log.concat();
        assert!(joined.contains("CONFIG: Expected one of these tokens: "), "{}", joined);
        assert!(joined.contains("CONFIG: (in file some/file.conf)\n"), "{}", joined);
        assert!(joined.contains("CONFIG: Line   1: bogus\n"), "{}", joined);
    }

    /// C: config_parser.c:755-816 — the variable substitution runs before the
    /// parser, so an undefined $variable reaches the parser verbatim and is a
    /// word token (it is not rejected by the lexer).
    #[test]
    fn undefined_variable_reaches_the_parser_verbatim() {
        let (hooks, context) = parse("font $undefined\n", "<stdin>");
        assert!(!context.has_errors);
        assert!(hooks.calls.contains(&"cfg_font(Some(\"$undefined\"))".to_string()));
    }
}
