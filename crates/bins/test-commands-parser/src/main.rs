//! C: src/commands_parser.c compiled with -DTEST_PARSER — the stand-alone
//! binary test.commands_parser which t/187-commands-parser.t drives.
//!
//! The C main allocates a yajl generator and calls parse_command(argv[1],
//! gen, NULL); the generated GENERATED_call() of that build prints every
//! cmd_* call to stderr instead of running it. i3-spec provides exactly that
//! through the test_parser feature (TestParserHooks is the no-op
//! CommandHooks implementor, the trace goes to stderr).

use std::process::ExitCode;

use i3_spec::commands_parser::{parse_command, TestParserHooks};

fn main() -> ExitCode {
    let mut args = std::env::args();
    let argv0 = args.next().unwrap_or_else(|| "test.commands_parser".to_string());

    // C: if (argc < 2) { fprintf(stderr, "Syntax: %s <command>\n", argv[0]);
    //    return 1; }
    let Some(input) = args.next() else {
        eprintln!("Syntax: {} <command>", argv0);
        return ExitCode::from(1);
    };

    // C: yajl_gen gen = yajl_gen_alloc(NULL);
    //    CommandResult *result = parse_command(argv[1], gen, NULL);
    //    command_result_free(result); yajl_gen_free(gen);
    let mut hooks = TestParserHooks;
    let _result = parse_command(&input, &mut hooks);

    ExitCode::SUCCESS
}
