//! i3-spec — the parser state machines generated from the parser-specs of
//! i3 4.25 (commit 8a97b0f), ported 1:1 from the C implementation.
//!
//! * build.rs is the 1:1 port of generate-command-parser.pl: it reads
//!   ../../i3-src/parser-specs/commands.spec and config.spec and generates
//!   the state enums, token tables, call dispatch and hook traits into
//!   OUT_DIR/generated_commands.rs and generated_config.rs.
//! * parser_util is the 1:1 port of src/parser_util.c (the identifier
//!   stack).
//! * commands_parser is the 1:1 port of src/commands_parser.c (the
//!   parse_command runtime) plus the generated parts of the command parser.
//! * config_parser holds the generated parts of the config parser and the
//!   cfg_* dispatch interface (ConfigHooks).
//!
//! The command and config handlers themselves (the cmd_* functions of
//! src/commands.c and the cfg_* functions of src/config_directives.c) are
//! implemented by i3-core through the CommandHooks and ConfigHooks traits.
//!
//! TODO (not ported yet, in porting order):
//! * src/config_parser.c — the config parser runtime: parse_config with
//!   its statelist error recovery, parse_file, variable substitution
//!   (upsert_variable/get_variable/variable_name), the "line"/"error"
//!   token handling, start_config_error_nagbar, free_variables. The
//!   ConfigParserCtx.variables field and the Variable type belong to that
//!   port.
//! * src/workspace.c:parse_workspace_name — reuses parse_string (exposed
//!   here as commands_parser::parse_string).
//! * The yajl json generation behind the y_* hook methods (the y()/ystr()
//!   macros of commands_parser.c) and the ELOG/DLOG logging — both live in
//!   i3-core.
//! * i3-core: implement CommandHooks and ConfigHooks with the real Match
//!   and Con types and port src/commands.c / src/config_directives.c.
//! * t/187-commands-parser.t and t/188-commands-parser-*: the C test suite
//!   driving the TEST_PARSER build; the test_parser feature provides the
//!   equivalent call logging.

pub mod commands_parser;
pub mod config_parser;
pub mod parser_util;

pub use commands_parser::{
    command_generated_call, command_tokens, parse_command, parse_string, CommandHooks,
    CommandParserCtx, CommandResult, CommandResultIr, CommandState, CommandToken, Owindow,
};
pub use config_parser::{
    config_generated_call, config_tokens, ConfigHooks, ConfigParserCtx, ConfigResultIr,
    ConfigState, ConfigToken,
};
pub use parser_util::{
    parser_clear_stack, parser_get_long, parser_get_string, parser_push_long, parser_push_string,
    Stack, StackEntry, StackType, StackVal,
};
