#!/bin/sh
# Build the two TEST_PARSER binaries of the C test suite and install them as
# test.commands_parser / test.config_parser:
#
#   crates/bins/test-commands-parser  C: src/commands_parser.c -DTEST_PARSER
#                                     (drives t/187-commands-parser.t)
#   crates/bins/test-config-parser    C: src/config_parser.c -DTEST_PARSER
#                                     (drives t/201-config-parser.t)
#
# Both are standalone cargo workspaces on purpose: they enable i3-spec's
# test_parser feature, and cargo unifies features inside a workspace, so as
# members they would switch the feature on for `cargo test --workspace` too
# (see the Cargo.toml comments).
#
# Usage: tools/build-test-parser-bins.sh [INSTALL_DIR...]
#        (default install dir: $I3_TEST_FARM/build, i.e. the test farm's build directory)
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
export PATH="$HOME/.cargo/bin:${PATH}"
export CARGO_TARGET_DIR="$repo/target"

cargo build --manifest-path "$repo/crates/bins/test-commands-parser/Cargo.toml"
cargo build --manifest-path "$repo/crates/bins/test-config-parser/Cargo.toml"

if [ "$#" -eq 0 ]; then
    set -- "${I3_TEST_FARM:?set I3_TEST_FARM to the i3 test farm root}/build"
fi

for dir in "$@"; do
    [ -d "$dir" ] || continue
    # Keep the previous file around once (the old stubs were shell scripts).
    for name in test.commands_parser test.config_parser; do
        if [ -e "$dir/$name" ] && [ ! -e "$dir/$name.bak" ]; then
            cp -p "$dir/$name" "$dir/$name.bak"
        fi
    done
    cp -f "$CARGO_TARGET_DIR/debug/test-commands-parser" "$dir/test.commands_parser"
    cp -f "$CARGO_TARGET_DIR/debug/test-config-parser" "$dir/test.config_parser"
    chmod 700 "$dir/test.commands_parser" "$dir/test.config_parser"
    printf 'installed %s/test.commands_parser and %s/test.config_parser\n' "$dir" "$dir"
done