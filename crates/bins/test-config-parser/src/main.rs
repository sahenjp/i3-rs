//! C: src/config_parser.c compiled with -DTEST_PARSER — the stand-alone
//! binary test.config_parser which t/201-config-parser.t drives.
//!
//! The C main sets up a stack, a parser_ctx with use_nagbar = false and the
//! filename "<stdin>", then runs parse_config(argv[1]) on the config text
//! passed as the first argument. The generated GENERATED_config_call() of
//! that build prints every cfg_* call to stderr (and keeps the side effects
//! of the cfg_criteria_* functions).

use std::process::ExitCode;

use i3_spec::config_parser::{parse_config, ConfigParserCtx, ParserContext, TestParserHooks};

fn main() -> ExitCode {
    let mut args = std::env::args();
    let argv0 = args.next().unwrap_or_else(|| "test.config_parser".to_string());

    // C: if (argc < 2) { fprintf(stderr, "Syntax: %s <command>\n", argv[0]);
    //    return 1; }
    let Some(input) = args.next() else {
        eprintln!("Syntax: {} <command>", argv0);
        return ExitCode::from(1);
    };

    // C: struct parser_ctx ctx = { .use_nagbar = false, .stack = &stack };
    //    SLIST_INIT(&(ctx.variables));
    //    struct context context;
    //    context.filename = "<stdin>";
    let mut ctx = ConfigParserCtx::<()>::default();
    let mut hooks = TestParserHooks::default();
    let mut context = ParserContext {
        filename: "<stdin>".to_string(),
        ..ParserContext::default()
    };

    parse_config(&mut ctx, &mut hooks, &input, &mut context);

    ExitCode::SUCCESS
}
