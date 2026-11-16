# Contributing

This repository is a **port**, not a fork with its own roadmap. The rules below
are what kept it honest and are the ones to keep:

## The contract

* **Behaviour is defined by i3 4.25 (upstream commit `8a97b0f`).** When the port
  and the C source disagree, the C source wins — including its bugs.
* **Every ported item says where it comes from.** Functions carry
  `// C: src/con.c:2754`. If you cannot point at the statement you are
  reproducing, you are writing new code, not porting it.
* **No new features, no refactors, no renames.** Enum values, struct fields and
  the control flow keep their C names and shape (`L_SPLITH`, `CT_WORKSPACE`,
  `i3String`, `A__NET_WM_NAME`, …). Crates use `#![allow(non_snake_case, …)]`
  for exactly this reason; do not "fix" the naming.
* **Do not run `cargo fmt` over existing modules.** The code is laid out to stay
  comparable with the C source line by line; a reformat destroys the diff
  against upstream. (CI formats only as an advisory, `continue-on-error`.)

## Checklist for a change

1. Find the C statement (`src/*.c`, `libi3/*.c`, `include/*.h`).
2. Port it 1:1 and add the `// C: path:line` comment.
3. `cargo test --workspace` — unit and integration tests must stay green.
4. Run the parity harnesses when your change touches config, IPC or rendering:

   ```sh
   I3_C_BASELINE=/path/to/i3-4.25.1-rpm-root ./tools/verify-config-parity.sh
   I3_C_BASELINE=/path/to/i3-4.25.1-rpm-root ./tools/verify-ipc-parity.sh :7
   ```

5. If the change can affect window management, run the upstream suite (see
   `docs/STATUS.md` for the harness); a change is only acceptable when no test
   that passes on the C build starts failing on the port.

## Tests

* Unit tests live next to the code (`#[cfg(test)] mod tests`).
* Integration tests that need an X server start **Xvfb** and skip themselves
  when none is available (`I3_TEST_XVFB` or `Xvfb` in `$PATH`). Never use
  Xephyr in CI or on a developer desktop: it opens a real window.
* Expected output that comes from the C build (e.g. `baseline/i3-msg-*.txt`) is
  recorded, never hand-written.

## Licensing

BSD-3-Clause, like i3. Files under `third_party/i3` are verbatim upstream
copies; see `NOTICE`.
