# i3-rs

A **1:1 Rust port of [i3](https://i3wm.org/) 4.25** (upstream commit `8a97b0f`).

Not a re-imagining and not a compatible alternative: the goal is that, given the
same config and the same clients, this binary does what the C binary does —
same tiling, same IPC bytes, same `get_tree` JSON, same error messages, same
pixels. Nothing was added; every ported function carries a `// C: src/foo.c:123`
comment pointing at the statement it reproduces, including the C control flow
and its quirks.

```
cargo build --release
target/release/i3                       # the window manager
target/release/{i3bar,i3-msg,i3-nagbar} # the usual companions
```

## Status

| | |
|---|---|
| upstream test suite (Perl/TAP) | **294 files / 4,032 assertions**, 2 failures — both also fail with C 4.25.1 |
| tests that fail only on the port | **0** |
| unit tests | **668**, all green |
| compiler warnings | **0** |
| code | ~78k lines of Rust across 7 binaries + 3 libraries |

Measured against the reference C build (i3 4.25.1 rpm) on a headless X server:

* `-C` config validation: **byte-identical** output, including error text, line
  numbers and exit codes (15-config corpus in `testcases/config-corpus`).
* IPC: `get_outputs` byte-identical, `get_workspaces`/`get_tree` identical apart
  from opaque ids (C prints a `Con *`, the port a slotmap key).
* Rendering: window decorations and i3bar are **pixel-identical** (AE = 0),
  including `-t` transparency and the XEmbed system tray.
* in-place restart and `append_layout` restore produce an identical tree.

See [docs/STATUS.md](docs/STATUS.md) for the per-round measurements and the
exact list of remaining differences.

## What works

* full i3 config language — all 76 directives, variables, `include`, modes,
  `for_window`, `assign`, gaps, borders, colours, bar blocks, …
* the complete IPC surface (13 message types, 7 event types), so `i3-msg`,
  `i3status`, `i3blocks`, polybar's i3 module and `i3-save-tree` just work
* i3bar: workspaces, status line (`short_text`, `min_width`, markup), clicks,
  `-t` transparency, system tray
* keyboard (xkbcommon, XKB groups, NumLock/CapsLock fallback), mouse bindings,
  drag/resize, floating, scratchpad, fullscreen, multi-monitor (RandR 1.5,
  Xinerama, fake outputs), in-place restart, `i3-dump-log -f`
* existing rices work unchanged: the config parser and the tools are
  compatible; i3 also spawns its own i3bar for every `bar {}` block like C does

Known gaps are listed at the end of [docs/STATUS.md](docs/STATUS.md); the
important one is that `533-randr15.t` needs a C-only test proxy.

## Build

Rust 1.100 or newer, plus the usual X11/Cairo/Pango development packages:

```sh
# Fedora
sudo dnf install libxcb-devel xcb-util-devel xcb-util-wm-devel xcb-util-cursor-devel \
                 xcb-util-keysyms-devel xcb-util-xrm-devel xkbcommon-x11-devel \
                 cairo-devel pango-devel
# Debian/Ubuntu
sudo apt install libxcb1-dev libxcb-randr0-dev libxcb-xkb-dev libxcb-xinerama0-dev \
                 libxcb-shape0-dev libxcb-icccm4-dev libxcb-keysyms1-dev \
                 libxcb-cursor-dev libxcb-util-dev libxcb-xrm-dev libxkbcommon-x11-dev \
                 libcairo2-dev libpango1.0-dev
cargo build --release
```

## Run

```sh
# i3 starts one i3bar per `bar {}` block itself, so only i3 has to be started
I3SOCK=/run/user/$(id -u)/i3/ipc-socket.$$ ./target/release/i3 -c ~/.i3/config
```

Validate an existing rice config first:

```sh
./target/release/i3 -C -c ~/.i3/config
```

## Testing

```sh
cargo test --workspace            # unit + integration tests (no X needed)
```

### Parity harnesses against the C reference

Both scripts need an extracted i3 4.25.1 rpm (no root required):

```sh
BASE=/tmp/i3-c; mkdir -p "$BASE"
dnf download --destdir /tmp/i3rpm i3 yajl xcb-util-xrm
for f in /tmp/i3rpm/*.rpm; do rpm2cpio "$f" | cpio -idm -D "$BASE"; done

I3_C_BASELINE="$BASE" ./tools/verify-config-parity.sh   # 15 configs, byte-identical
I3_C_BASELINE="$BASE" ./tools/verify-ipc-parity.sh :7   # 9 IPC messages
```

### The upstream test suite

i3's own Perl/TAP suite can run against this binary. The harness lives outside
this repository (it needs Xvfb/Xephyr, Perl modules and an ASCII path); set up
the farm once, then:

```sh
cd "$I3_TEST_FARM/build" && bash run-suite.sh /tmp/all294.txt 8
```

`tools/build-test-parser-bins.sh` installs the `test.commands_parser` /
`test.config_parser` helpers (the C suite calls them) built from
`crates/bins/test-*`.

## Repository layout

| path | C counterpart |
|---|---|
| `crates/libi3` | `libi3/` — IPC client, i3String, fonts, drawing, dpi, xkb, slurm… |
| `crates/i3-spec` | `parser-specs/*.spec` + `generate-command-parser.pl`; the state machines are generated in `build.rs` |
| `crates/i3-core` | `src/*.c` — tree, layout, X11 layer, IPC server, commands, config, log |
| `crates/bins/i3*` | the seven binaries (`i3`, `i3bar`, `i3-msg`, `i3-nagbar`, `i3-input`, `i3-dump-log`, `i3-config-wizard`) |
| `crates/bins/test-*` | the `-DTEST_PARSER` helper binaries used by tests `187`/`201` |
| `third_party/i3` | verbatim copies of the two parser specs (+ helper) from i3 4.25 |
| `tools/` | parity harnesses against the C build |
| `docs/` | porting plan and status/measurements |

## License

BSD 3-Clause, same as i3. See [LICENSE](LICENSE) and [NOTICE](NOTICE) for the
vendored upstream files.
