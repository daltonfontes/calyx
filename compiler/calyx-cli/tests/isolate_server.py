#!/usr/bin/env python3
"""An MCP server with one tool, `probe(box, secret, outside, port)`, that
tries what an isolated tool must not do, and one thing it must: write in its
sandbox. It answers with what worked, for compiler/calyx-cli/tests/isolate.rs.
"""
import json
import os
import socket
import sys

TOOL = {"name": "probe", "inputSchema": {"type": "object", "properties": {
    "box": {"type": "string"}, "secret": {"type": "string"},
    "outside": {"type": "string"}, "port": {"type": "string"}}}}


def attempt(f):
    try:
        f()
        return "yes"
    except Exception as e:  # noqa: BLE001 - any refusal is the answer
        return "no: " + type(e).__name__


def probe(a):
    def read_secret():
        with open(a["secret"]) as f:
            f.read()

    def write_outside():
        with open(a["outside"], "w") as f:
            f.write("x")

    def write_inside():
        with open(os.path.join(a["box"], "made-by-the-tool.txt"), "w") as f:
            f.write("ok")

    def connect():
        socket.create_connection(("127.0.0.1", int(a["port"])), timeout=2).close()

    def temp():
        with open(os.path.join(os.environ.get("TMPDIR", "/tmp"), "t"), "w") as f:
            f.write("ok")

    return {"read_secret": attempt(read_secret), "write_outside": attempt(write_outside),
            "write_inside": attempt(write_inside), "connect": attempt(connect),
            "temp": attempt(temp)}


for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    m = msg["method"]
    if m == "initialize":
        r = {"protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {}},
             "serverInfo": {"name": "probe", "version": "0"}}
    elif m == "tools/list":
        r = {"tools": [TOOL]}
    else:
        r = {"content": [{"type": "text", "text": json.dumps(probe(msg["params"]["arguments"]))}]}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": r}) + "\n")
    sys.stdout.flush()
