#!/usr/bin/env python3
"""Sends a raw GET_CONFIG (type 9) request to an i3 IPC socket and prints the
reply JSON as received.

i3-msg prints only the "config" member of a GET_CONFIG reply, so the
"included_configs" array (paths, raw_contents, variable_replaced_contents) can
only be compared with a raw client.

Usage: ipc-raw-get-config.py /path/to/ipc-socket
"""
import json
import socket
import struct
import sys

MAGIC = b"i3-ipc"
GET_CONFIG = 9

sock = sys.argv[1]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock)
s.sendall(MAGIC + struct.pack("<II", 0, GET_CONFIG))
header = b""
while len(header) < 14:
    header += s.recv(14 - len(header))
length, _type = struct.unpack("<II", header[6:14])
body = b""
while len(body) < length:
    body += s.recv(length - len(body))
print(json.dumps(json.loads(body), ensure_ascii=False, sort_keys=True))
