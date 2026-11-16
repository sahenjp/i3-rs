//! C: src/commands_parser.c — hand-written parser to parse commands
//! (commands are what you bind on keys and what you can send to i3 using
//! the IPC interface, like "move left" or "workspace 4"), ported 1:1.
//!
//! The parser is basically a state machine which looks for literals or
//! strings and can push either on a stack. After identifying a literal or
//! string, it will either transition to the current state, to a different
//! state, or call a function (like cmd_move()).
//!
//! The generated parts (state enum, token tables, call dispatch and the
//! CommandHooks trait) come from parser-specs/commands.spec via build.rs,
//! the 1:1 port of generate-command-parser.pl, and are included at the end
//! of this module.

use crate::parser_util::{
    parser_clear_stack, parser_get_long, parser_get_string, parser_push_long,
    parser_push_string, Stack,
};

/// C: include/commands_parser.h:31-49 — struct CommandResultIR.
///
/// The ctx, json_gen and client members of the C struct are not fields here:
/// C stored raw pointers back into the parser context and into the IPC state
/// (a self-referential pointer), which the Rust port expresses by passing
/// the whole CommandParserCtx to the CommandHooks methods — see
/// command_generated_call() in the generated code included below. The
/// needs_tree_render flag of the C subcommand_output is kept.
#[derive(Clone, Default, Debug)]
pub struct CommandResultIr {
    /// C: int next_state — the next state to transition to. Set by the
    /// generated call before the handler runs; a handler can override it.
    pub next_state: i32,
    /// C: bool needs_tree_render
    pub needs_tree_render: bool,
}

/// C: include/commands_parser.h:17-21 — typedef struct owindow. Helper data
/// structure for an operation window (window on which the operation will be
/// performed). Used to build the TAILQ owindows (a Vec here).
pub struct Owindow<Con> {
    /// C: Con *con
    pub con: Con,
}

/// C: include/commands_parser.h:71-84 — struct cmd_parser_ctx. Context
/// structure for the command parser, making it re-entrant.
pub struct CommandParserCtx<M, Con> {
    /// C: int state
    pub state: CommandState,
    /// C: Match current_match
    pub current_match: M,
    /// C: struct stack stack — the (small) stack where identified literals
    /// are stored during the parsing of a single command (like $workspace).
    pub stack: Stack,
    /// C: struct owindows_head owindows — list of operation windows (windows
    /// on which operations will be performed). A TAILQ in C, a Vec here.
    pub owindows: Vec<Owindow<Con>>,
    /// C: struct CommandResultIR subcommand_output
    pub subcommand_output: CommandResultIr,
    /// C: struct CommandResultIR command_output (only needs_tree_render is
    /// consumed; json_gen/client live on the CommandHooks implementor)
    pub command_output: CommandResultIr,
}

/// C: include/commands_parser.h:86-97 — struct CommandResult. A struct that
/// contains useful information about the result of a command as a whole
/// (e.g. a compound command like "floating enable, border none").
/// needs_tree_render is true if needs_tree_render of any individual command
/// was true.
#[derive(Clone, Default, Debug)]
pub struct CommandResult {
    /// C: bool parse_error
    pub parse_error: bool,
    /// C: char *error_message — the error_message is currently only set for
    /// parse errors (char * NULL in C, Option here).
    pub error_message: Option<String>,
    /// C: bool needs_tree_render
    pub needs_tree_render: bool,
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
/// of length n on the input at pos. The C input is NUL-terminated, so a
/// shorter remainder never matches.
fn strncasecmp_eq(buf: &[u8], pos: usize, lit: &str) -> bool {
    let litb = lit.as_bytes();
    if pos + litb.len() > buf.len() {
        return false;
    }
    buf[pos..pos + litb.len()].eq_ignore_ascii_case(litb)
}

/// C: strtol(walk, &end, 10) — decimal only, with the ERANGE behaviour of
/// commands_parser.c:219-222 (out-of-range numbers make the token fail).
/// Returns (number, position of the first non-number character) or None
/// when no conversion could be performed (C: end == walk).
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

/// C: src/commands_parser.c:95 — parses a string (or word, if as_word is
/// true). Extracted out of parse_command so that it can be used in
/// src/workspace.c for interpreting workspace commands.
///
/// walk is a byte index into buf; it is advanced like the C **walk pointer.
/// The input in C is NUL-terminated, at() supplies the sentinel byte.
pub fn parse_string(buf: &[u8], walk: &mut usize, as_word: bool) -> Option<String> {
    let mut beginning = *walk;
    // Handle quoted strings (or words).
    if at(buf, *walk) == b'"' {
        beginning += 1;
        *walk += 1;
        while at(buf, *walk) != b'\0' && at(buf, *walk) != b'"' {
            if at(buf, *walk) == b'\\' && at(buf, *walk + 1) != b'\0' {
                *walk += 1;
            }
            *walk += 1;
        }
    } else {
        if !as_word {
            // For a string (starting with an s), the delimiters are comma
            // (,) and semicolon (;) which introduce a new operation or
            // command, respectively. Also, newlines end a command.
            while at(buf, *walk) != b';'
                && at(buf, *walk) != b','
                && at(buf, *walk) != b'\0'
                && at(buf, *walk) != b'\r'
                && at(buf, *walk) != b'\n'
            {
                *walk += 1;
            }
        } else {
            // For a word, the delimiters are white space (space or tab),
            // closing square bracket (]), comma (,) and semicolon (;).
            while at(buf, *walk) != b' '
                && at(buf, *walk) != b'\t'
                && at(buf, *walk) != b']'
                && at(buf, *walk) != b','
                && at(buf, *walk) != b';'
                && at(buf, *walk) != b'\r'
                && at(buf, *walk) != b'\n'
                && at(buf, *walk) != b'\0'
            {
                *walk += 1;
            }
        }
    }
    if *walk == beginning {
        // C: return NULL (an empty quoted string counts as no match).
        return None;
    }

    // We copy manually to handle escaping of characters. The bytes between
    // delimiters of a UTF-8 input are always valid UTF-8 themselves because
    // all delimiters are ASCII, so the lossy conversion never triggers for
    // &str inputs.
    let mut out: Vec<u8> = Vec::new();
    let mut inpos = beginning;
    while inpos < *walk {
        // We only handle escaped double quotes and backslashes to not break
        // backwards compatibility with people using \w in regular
        // expressions etc.
        if at(buf, inpos) == b'\\'
            && (at(buf, inpos + 1) == b'"' || at(buf, inpos + 1) == b'\\')
        {
            inpos += 1;
        }
        out.push(at(buf, inpos));
        inpos += 1;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// C: src/commands_parser.c:66 — advances the state machine. When the token
/// leads to __CALL, the generated call dispatch runs the handler and the
/// next state is whatever the handler left in ctx.subcommand_output.
fn next_state<H: CommandHooks>(
    token: &CommandToken,
    ctx: &mut CommandParserCtx<H::Match, H::Con>,
    hooks: &mut H,
) {
    if token.next_state == CommandState::__CALL {
        // C: cmd_ctx->subcommand_output.ctx = cmd_ctx;
        //    cmd_ctx->subcommand_output.json_gen = cmd_ctx->command_output.json_gen;
        //    cmd_ctx->subcommand_output.client = cmd_ctx->command_output.client;
        // (ctx/json_gen/client live on the CommandHooks implementor in the
        // Rust port, see CommandResultIr above)
        ctx.subcommand_output.needs_tree_render = false;
        command_generated_call(hooks, ctx, token.call_identifier);
        ctx.state = CommandState::from_i32(ctx.subcommand_output.next_state)
            .unwrap_or_else(|| {
                // C would index the tokens array out of bounds here; make
                // the bug loud instead of undefined behaviour.
                panic!(
                    "BUG in the parser. state = {}",
                    ctx.subcommand_output.next_state
                )
            });
        // If any subcommand requires a tree_render(), we need to make the
        // whole parser result request a tree_render().
        if ctx.subcommand_output.needs_tree_render {
            ctx.command_output.needs_tree_render = true;
        }
        parser_clear_stack(&mut ctx.stack);
        return;
    }

    ctx.state = token.next_state;
    if ctx.state == CommandState::INITIAL {
        parser_clear_stack(&mut ctx.stack);
    }
}

/// C: src/commands_parser.c:158 — parses and executes the given command. If
/// a json reply is required, the CommandHooks implementor carries a json
/// generator (C: the yajl_gen gen parameter) and produces the reply through
/// the y_* hook methods; pass an implementor without a generator if no json
/// reply is required (C: gen == NULL).
///
/// C: CommandResult *parse_command(const char *input, yajl_gen gen,
/// ipc_client *client) — the client parameter (which caused the command to
/// be executed) is likewise held by the implementor. Freeing the returned
/// CommandResult is command_result_free() in C; Rust drops it.
pub fn parse_command<H: CommandHooks>(input: &str, hooks: &mut H) -> CommandResult {
    // C: DLOG("COMMAND: *%.4000s*\n", input); — debug logging lives in
    // i3-core (src/log.c) and is not available in this crate.
    let mut ctx = CommandParserCtx::<H::Match, H::Con> {
        state: CommandState::INITIAL,
        current_match: H::Match::default(),
        stack: Stack::default(),
        owindows: Vec::new(),
        subcommand_output: CommandResultIr::default(),
        command_output: CommandResultIr::default(),
    };
    let mut result = CommandResult::default();

    // C: y(array_open);
    hooks.y_array_open();
    ctx.command_output.needs_tree_render = false;

    let buf = input.as_bytes();
    let len = buf.len(); // C: const size_t len = strlen(input);
    let mut walk = 0usize; // C: const char *walk = input;

    // C: #ifndef TEST_PARSER — cmd_criteria_init(&cmd_ctx.current_match,
    //    &cmd_ctx.subcommand_output);
    #[cfg(not(feature = "test_parser"))]
    hooks.cmd_criteria_init(&mut ctx);

    // The "<=" operator is intentional: We also handle the terminating
    // 0-byte explicitly by looking for an end token. (C comment)
    while walk <= len {
        // skip whitespace and newlines before every token
        while matches!(at(buf, walk), b' ' | b'\t' | b'\r' | b'\n') {
            walk += 1;
        }

        // C: const cmdp_token_ptr *ptr = &(tokens[cmd_ctx.state]);
        let ptr: &[CommandToken] = command_tokens(ctx.state);
        let mut token_handled = false;
        for token in ptr {
            // A literal.
            if token.name.as_bytes().first() == Some(&b'\'') {
                // C: token->name + 1 (the literal without the leading quote)
                let lit = &token.name[1..];
                if strncasecmp_eq(buf, walk, lit) {
                    if let Some(identifier) = token.identifier {
                        // C: parser_push_string(&cmd_ctx.stack,
                        //    token->identifier, token->name + 1);
                        parser_push_string(&mut ctx.stack, identifier, lit);
                    }
                    // C: walk += strlen(token->name) - 1;
                    walk += token.name.len() - 1;
                    next_state(token, &mut ctx, hooks);
                    token_handled = true;
                    break;
                }
                continue;
            }

            if token.name == "number" {
                // Handle numbers. We only accept decimal numbers for now.
                // C: strtol + the ERANGE/errno checks.
                match strtol10(buf, walk) {
                    Some((num, end)) => {
                        if let Some(identifier) = token.identifier {
                            parser_push_long(&mut ctx.stack, identifier, num);
                        }
                        // Set walk to the first non-number character.
                        walk = end;
                        next_state(token, &mut ctx, hooks);
                        token_handled = true;
                        break;
                    }
                    None => continue,
                }
            }

            if token.name == "string" || token.name == "word" {
                // C: parse_string(&walk, (token->name[0] != 's'));
                if let Some(str) = parse_string(buf, &mut walk, token.name.as_bytes()[0] != b's')
                {
                    if let Some(identifier) = token.identifier {
                        parser_push_string(&mut ctx.stack, identifier, &str);
                    }
                    // If we are at the end of a quoted string, skip the
                    // ending double quote.
                    if at(buf, walk) == b'"' {
                        walk += 1;
                    }
                    next_state(token, &mut ctx, hooks);
                    token_handled = true;
                    break;
                }
            }

            if token.name == "end" {
                if at(buf, walk) == b'\0' || at(buf, walk) == b',' || at(buf, walk) == b';' {
                    next_state(token, &mut ctx, hooks);
                    token_handled = true;
                    // To make sure we start with an appropriate matching
                    // datastructure for commands which do *not* specify any
                    // criteria, we re-initialize the criteria system after
                    // every command.
                    #[cfg(not(feature = "test_parser"))]
                    if at(buf, walk) == b'\0' || at(buf, walk) == b';' {
                        hooks.cmd_criteria_init(&mut ctx);
                    }
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
                    // Any other token is copied to the error message
                    // enclosed with angle brackets.
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

            // Contains the same amount of characters as input has, but with
            // the unparsable part highlighted using ^ characters.
            let position: String = buf
                .iter()
                .enumerate()
                .map(|(i, _)| if i >= walk { '^' } else { ' ' })
                .collect();

            // C: ELOG("%s\n", errormessage); ELOG("Your command: %s\n",
            //    input); ELOG("              %s\n", position);
            // — error logging lives in i3-core (src/log.c). The TEST_PARSER
            // build (t/187-commands-parser.t) captures stderr, so the lines
            // are written there like errorlog() does ("ERROR: " prefix from
            // include/log.h:29).
            #[cfg(feature = "test_parser")]
            {
                eprintln!("ERROR: {}", errormessage);
                eprintln!("ERROR: Your command: {}", input);
                eprintln!("ERROR:               {}", position);
            }

            result.parse_error = true;
            result.error_message = Some(errormessage);

            // Format this error message as a JSON reply.
            hooks.y_map_open();
            hooks.y_string("success");
            hooks.y_bool(false);
            // We set parse_error to true to distinguish this from other
            // errors. i3-nagbar is spawned upon keypresses only for parser
            // errors.
            hooks.y_string("parse_error");
            hooks.y_bool(true);
            hooks.y_string("error");
            hooks.y_string(result.error_message.as_deref().unwrap_or(""));
            hooks.y_string("input");
            hooks.y_string(input);
            hooks.y_string("errorposition");
            hooks.y_string(&position);
            hooks.y_map_close();

            parser_clear_stack(&mut ctx.stack);
            break;
        }
    }

    // C: y(array_close);
    hooks.y_array_close();

    result.needs_tree_render = ctx.command_output.needs_tree_render;
    // Clean up owindows entries: C frees the TAILQ entries here, the Vec
    // drops automatically together with the context.
    result
}

// C: commands_parser.c:373-380 — command_result_free(): freeing the
// CommandResult is automatic in Rust (Drop of the error_message String).

// The generated state machine (C: GENERATED_command_enums.h,
// GENERATED_command_tokens.h, GENERATED_command_call.h), built by build.rs
// from parser-specs/commands.spec with the same logic as
// generate-command-parser.pl.
include!(concat!(env!("OUT_DIR"), "/generated_commands.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    /// C: parse_string with a quoted string keeps the walk at the closing
    /// quote and unescapes \" and \\.
    #[test]
    fn parse_string_quoted() {
        let buf = br#""foo\"bar" rest"#;
        let mut walk = 0;
        let s = parse_string(buf, &mut walk, false).unwrap();
        assert_eq!(s, "foo\"bar");
        // C: parse_string stops AT the closing quote; the caller (commands_parser.c:250)
        // is the one that skips it with walk++.
        assert_eq!(walk, 9); // at the closing quote
        assert_eq!(buf[walk], b'"');
    }

    /// C: parse_string leaves other escape sequences untouched (\w etc.).
    #[test]
    fn parse_string_keeps_unknown_escapes() {
        let buf = br#""a\wb\\c""#;
        let mut walk = 0;
        let s = parse_string(buf, &mut walk, false).unwrap();
        assert_eq!(s, r"a\wb\c");
    }

    /// C: an unquoted string runs until ; , \r \n or the end.
    #[test]
    fn parse_string_unquoted() {
        let buf = b"foo bar, x";
        let mut walk = 0;
        let s = parse_string(buf, &mut walk, false).unwrap();
        assert_eq!(s, "foo bar");
        assert_eq!(buf[walk], b',');
    }

    /// C: an unquoted word runs until whitespace, ], , or ;.
    #[test]
    fn parse_string_word() {
        let buf = b"foo bar] x";
        let mut walk = 0;
        let s = parse_string(buf, &mut walk, true).unwrap();
        assert_eq!(s, "foo");
        assert_eq!(buf[walk], b' ');
    }

    /// C: an empty quoted string returns NULL (walk == beginning).
    #[test]
    fn parse_string_empty_quoted_is_null() {
        let buf = br#""" next"#;
        let mut walk = 0;
        assert!(parse_string(buf, &mut walk, false).is_none());
    }

    /// C: an unterminated quoted string still consumes to the end.
    #[test]
    fn parse_string_unterminated_quote() {
        let buf = br#""abc"#;
        let mut walk = 0;
        let s = parse_string(buf, &mut walk, false).unwrap();
        assert_eq!(s, "abc");
        assert_eq!(walk, buf.len());
    }

    /// C: strtol semantics — sign, digits, stop at first non-number.
    #[test]
    fn strtol10_prefix() {
        assert_eq!(strtol10(b"12px", 0), Some((12, 2)));
        assert_eq!(strtol10(b"-3", 0), Some((-3, 2)));
        assert_eq!(strtol10(b"+4", 0), Some((4, 2)));
        assert_eq!(strtol10(b"0x10", 0), Some((0, 1)));
        assert_eq!(strtol10(b"px", 0), None);
        assert_eq!(strtol10(b"", 0), None);
    }

    /// C: strncasecmp literal match is case-insensitive.
    #[test]
    fn literal_match_case_insensitive() {
        assert!(strncasecmp_eq(b"MOVE left", 0, "move"));
        assert!(strncasecmp_eq(b"move left", 0, "move"));
        assert!(!strncasecmp_eq(b"mov", 0, "move"));
        assert!(!strncasecmp_eq(b"", 0, "move"));
    }
}
