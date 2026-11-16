#!/bin/bash
# Compares the IPC replies of the C i3 baseline and the Rust port on a real X
# server. Both WMs grab WM_Sn on the same display, so they run one after the
# other; the replies are stored per binary and compared structurally with
# python3.
#
# Usage: tools/verify-ipc-parity.sh [DISPLAY] [CONFIG]
#   DISPLAY default :7, CONFIG default /tmp/i3test.conf
#
#   I3_C_BASELINE  extracted i3 4.25.1 rpm root (see verify-config-parity.sh)
#   I3_RUST_BIN    the Rust binary to compare (default ./target/debug/i3)
#
# Known, intentional differences (reported separately instead of as failures):
#   * get_version .patch / .human_readable — the baseline rpm is 4.25.1 while
#     the port follows the i3 4.25 source (config.h) it is ported from.
#   * get_workspaces .id — the C value is the Con pointer, the Rust value is
#     the ConId ffi value; both are opaque and unique per process.

set -u

DISPLAY_NUM=${1:-:7}
CONF=${2:-/tmp/i3test.conf}
OUT=/tmp/ipc-parity
SOCK=/tmp/ipc-parity.sock
CROOT="${I3_C_BASELINE:?set I3_C_BASELINE to the extracted i3 4.25.1 rpm root}"
CBIN=$CROOT/usr/bin/i3
CMSG=$CROOT/usr/bin/i3-msg
RBIN="${I3_RUST_BIN:-./target/debug/i3}"

QUERIES=(get_version get_binding_modes get_config get_marks get_workspaces get_outputs)

# i3-msg comes from the baseline rpm and needs its bundled libraries.
export LD_LIBRARY_PATH=$CROOT/usr/lib64

kill_i3() {
    for p in $(pgrep -x i3); do kill -9 "$p" 2>/dev/null; done
    sleep 0.5
}

# $1 = binary, $2 = output dir, $3 = log tag
capture() {
    local bin=$1 dir=$2 tag=$3
    mkdir -p "$dir"
    # Xwayland keeps WM_Sn owned for a moment after the previous WM is
    # killed; retry the whole start until the socket answers.
    local attempt
    for attempt in 1 2 3 4 5; do
        ( cd "$dir" && DISPLAY=$DISPLAY_NUM I3SOCK=$SOCK \
            LD_LIBRARY_PATH=$CROOT/usr/lib64 \
            setsid "$bin" -c "$CONF" >"$dir/log-$tag" 2>&1 & )
        local ok=0
        for _ in $(seq 1 50); do
            if $CMSG -s $SOCK -t get_version >/dev/null 2>&1; then ok=1; break; fi
            sleep 0.2
        done
        if [ "$ok" = 1 ]; then break; fi
        kill_i3
        sleep 1
    done
    for q in "${QUERIES[@]}"; do
        timeout 5 $CMSG -s $SOCK -t "$q" >"$dir/$q.json" 2>"$dir/$q.err"
    done
    # GET_CONFIG also carries included_configs, which i3-msg does not print.
    timeout 5 python3 "$(dirname "$0")/ipc-raw-get-config.py" $SOCK \
        >"$dir/raw_get_config.json" 2>"$dir/raw_get_config.err"
    timeout 5 $CMSG -s $SOCK 'nop' >"$dir/nop.json" 2>&1
    timeout 5 $CMSG -s $SOCK 'bogus_command_xyz' >"$dir/invalid.txt" 2>&1
    kill_i3
}

rm -rf "$OUT"
kill_i3
mkdir -p "$OUT"
(cd "$(dirname "$0")/.." && export PATH=$HOME/.cargo/bin:$PATH && cargo build >"$OUT/build.log" 2>&1) \
    || { echo "cargo build failed, see $OUT/build.log"; exit 1; }

echo "=== C baseline ($CBIN)"
capture "$CBIN" "$OUT/c" c
echo "=== Rust ($RBIN)"
capture "$RBIN" "$OUT/rust" rust

python3 "$(dirname "$0")/compare-ipc-json.py" "$OUT"
