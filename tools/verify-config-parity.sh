#!/usr/bin/env bash
# Config-parser parity check: run the C i3 and the Rust i3 with -C (validate
# only) over a corpus and diff the output byte for byte after normalising the
# locale-dependent log timestamp prefix.
#
# Usage: I3_C_BASELINE=/path/to/i3-rpm-root tools/verify-config-parity.sh [CORPUS_DIR]
#
#   I3_C_BASELINE  directory of the extracted i3 4.25.1 rpm (needs usr/bin/i3
#                  and usr/lib64). Build one with:
#                    dnf download --destdir /tmp/i3rpm i3 yajl xcb-util-xrm
#                    for f in /tmp/i3rpm/*.rpm; do rpm2cpio "$f" | cpio -idm -D "$BASE"; done
#   I3_RUST_BIN    the Rust binary to compare (default ./target/debug/i3)
set -u
CORPUS="${1:-testcases/config-corpus}"
BASE="${I3_C_BASELINE:?set I3_C_BASELINE to the extracted i3 4.25.1 rpm root}"
C_I3="$BASE/usr/bin/i3"
R_I3="${I3_RUST_BIN:-./target/debug/i3}"
export LD_LIBRARY_PATH="$BASE/usr/lib64"

if [ ! -x "$C_I3" ]; then echo "C i3 not found at $C_I3" >&2; exit 2; fi
if [ ! -x "$R_I3" ]; then echo "Rust i3 not found at $R_I3 (run cargo build)" >&2; exit 2; fi

norm() {
    sed -E 's/^[0-9]{4}年[0-9]{1,2}月[0-9]{1,2}日 [0-9]{1,2}時[0-9]{1,2}分[0-9]{1,2}秒 - //; s/^[0-9]{2}\/[0-9]{2}\/[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2} - //'
}

pass=0; fail=0
for cf in "$CORPUS"/*.conf; do
    [ -e "$cf" ] || continue
    n=$(basename "$cf")
    "$C_I3" -C -c "$cf" >/tmp/i3-c.out 2>&1; ce=$?
    "$R_I3" -C -c "$cf" >/tmp/i3-r.out 2>&1; re=$?
    if diff -q <(norm </tmp/i3-c.out) <(norm </tmp/i3-r.out) >/dev/null && [ "$ce" = "$re" ]; then
        pass=$((pass+1))
    else
        fail=$((fail+1))
        echo "DIFF: $n (exit C=$ce R=$re)"
        diff <(norm </tmp/i3-c.out) <(norm </tmp/i3-r.out) | head -10
    fi
done
echo "config parity: PASS=$pass FAIL=$fail"
[ "$fail" = 0 ]
