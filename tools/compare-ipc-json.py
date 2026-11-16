#!/usr/bin/env python3
"""Structurally compares the IPC replies captured by verify-ipc-parity.sh.

$1 is the capture root containing c/ and rust/ subdirectories.
Exit code 0 = every reply matches (key order is irrelevant, values are not).
"""
import json
import os
import sys

OUT = sys.argv[1]
QUERIES = ["get_version", "get_binding_modes", "get_config", "get_marks",
           "get_workspaces", "get_outputs"]
# Values that differ by design; see the header of verify-ipc-parity.sh.
IGNORED = {("get_version", "patch"), ("get_version", "human_readable")}


def load(path):
    with open(path, "rb") as f:
        raw = f.read()
    try:
        return json.loads(raw)
    except Exception:
        return raw.decode("utf-8", "replace")


def strip(obj, path):
    if isinstance(obj, dict):
        res = {}
        for k, v in obj.items():
            if (path, k) in IGNORED:
                res[k] = "<ignored>"
            elif path.startswith("get_workspaces") and k == "id":
                res[k] = "<con-id>"
            else:
                res[k] = strip(v, path + "." + k)
        return res
    if isinstance(obj, list):
        return [strip(v, path + "[]") for v in obj]
    return obj


def diff(a, b, where=""):
    lines = []
    if type(a) is not type(b):
        return ["%s: type %s != %s (%r vs %r)" % (where, type(a).__name__, type(b).__name__, a, b)]
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a:
                lines.append("%s.%s: missing in C, Rust has %r" % (where, k, b[k]))
            elif k not in b:
                lines.append("%s.%s: missing in Rust, C has %r" % (where, k, a[k]))
            else:
                lines += diff(a[k], b[k], "%s.%s" % (where, k))
        return lines
    if isinstance(a, list):
        if len(a) != len(b):
            lines.append("%s: length %d (C) != %d (Rust)" % (where, len(a), len(b)))
        for i, (x, y) in enumerate(zip(a, b)):
            lines += diff(x, y, "%s[%d]" % (where, i))
        return lines
    if a != b:
        return ["%s: %r (C) != %r (Rust)" % (where, a, b)]
    return []


def last_json(text):
    for line in reversed([l for l in text.splitlines() if l.strip()]):
        if line.lstrip().startswith("["):
            return json.loads(line)
    return {"__no_json__": text}


def main():
    fail = 0
    for q in QUERIES:
        c = strip(load(os.path.join(OUT, "c", q + ".json")), q)
        r = strip(load(os.path.join(OUT, "rust", q + ".json")), q)
        d = diff(c, r, q)
        if d:
            fail = 1
            print("DIFF %s" % q)
            for line in d:
                print("  " + line)
        else:
            print("OK   %s" % q)

    c = load(os.path.join(OUT, "c", "nop.json"))
    r = load(os.path.join(OUT, "rust", "nop.json"))
    d = diff(json.loads(c) if isinstance(c, str) else c,
             json.loads(r) if isinstance(r, str) else r, "nop")
    if d:
        fail = 1
        print("DIFF nop")
        for line in d:
            print("  " + line)
    else:
        print("OK   nop")

    # Invalid command: i3-msg prints the parse error on stderr and the JSON
    # error reply on stdout, both come from the reply (i3-msg/main.c:92-95).
    ctext = open(os.path.join(OUT, "c", "invalid.txt"), encoding="utf-8", errors="replace").read()
    rtext = open(os.path.join(OUT, "rust", "invalid.txt"), encoding="utf-8", errors="replace").read()
    if ctext == rtext:
        print("OK   invalid")
    else:
        d = diff(last_json(ctext), last_json(rtext), "invalid")
        fail = 1
        print("DIFF invalid (text differs)")
        for line in (d or ["  " + repr(ctext) + " != " + repr(rtext)]):
            print("  " + line if not line.startswith("  ") else line)

    rc = load(os.path.join(OUT, "c", "raw_get_config.json"))
    rr = load(os.path.join(OUT, "rust", "raw_get_config.json"))
    d = diff(rc if not isinstance(rc, str) else {"__raw__": rc},
             rr if not isinstance(rr, str) else {"__raw__": rr},
             "raw_get_config")
    if d:
        fail = 1
        print("DIFF raw_get_config (GET_CONFIG incl. included_configs)")
        for line in d:
            print("  " + line)
    else:
        print("OK   raw_get_config")

    cv = load(os.path.join(OUT, "c", "get_version.json"))
    rv = load(os.path.join(OUT, "rust", "get_version.json"))
    if cv.get("patch") != rv.get("patch"):
        print("NOTE get_version.patch: C=%r Rust=%r (baseline rpm 4.25.1 vs i3 4.25 source)"
              % (cv.get("patch"), rv.get("patch")))

    print("PASS" if not fail else "FAIL")
    return fail


if __name__ == "__main__":
    sys.exit(main())
