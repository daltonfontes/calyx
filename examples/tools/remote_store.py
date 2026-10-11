#!/usr/bin/env python3
"""A remote MCP server (Streamable HTTP) for tests and examples: orders and
refunds, over HTTP instead of stdio.

    python3 remote_store.py [--port 0] [--token T] [--sse] [--forget-after N]

- Prints `listening on <port>` when ready (`--port 0`: any free port).
- `--token T`: every request needs `Authorization: Bearer T` (else 401).
- `--sse`: answers come as a stream of server-sent events, each preceded
  by a notification the client must skip; else as one JSON body.
- `--forget-after N`: the session is forgotten after N requests (404 for
  its id), as a server that restarts would; the client starts a new one.

Tools: `get_order(id)` (read), `refund(request, order, amount)` (a write
whose idempotency key, in `_meta`, is honoured) and `slow(ms)` (sleeps,
then answers: for concurrency tests). Every request is appended to
$CALYX_REMOTE_LOG as a JSON line (method, tool, session, headers seen,
time), and the end of each `slow` call too.
"""
import argparse
import json
import os
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ORDERS = {
    "A100": {"id": "A100", "customer": "Ana", "email": "ana@exemplo.org", "total": 300.0},
    "A200": {"id": "A200", "customer": "Bruno", "email": "bruno@exemplo.org", "total": 120.0},
}

TOOLS = [
    {"name": "get_order", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}},
                                          "required": ["id"]},
     "annotations": {"readOnlyHint": True}},
    {"name": "refund", "inputSchema": {"type": "object", "properties": {
        "request": {"type": "string"}, "order": {"type": "string"}, "amount": {"type": "number"}},
        "required": ["request", "order", "amount"]},
     "annotations": {"readOnlyHint": False, "idempotentHint": False}},
    {"name": "slow", "inputSchema": {"type": "object", "properties": {"ms": {"type": "number"}},
                                     "required": ["ms"]},
     "annotations": {"readOnlyHint": True}},
]

args = None
lock = threading.Lock()
sessions = {}        # id -> requests seen
paid = {}            # idempotency key -> refund


def log(entry):
    path = os.environ.get("CALYX_REMOTE_LOG")
    if path:
        with lock, open(path, "a") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")


def text(s, error=False):
    return {"content": [{"type": "text", "text": s}], "isError": error}


def call(name, a, meta):
    if name == "get_order":
        o = ORDERS.get(a.get("id"))
        return text(json.dumps(o)) if o else text(f"NotFound: no order {a.get('id')}", True)
    if name == "refund":
        key = meta.get("calyx/idempotency_key")
        with lock:
            if key is None or key not in paid:
                paid[key or str(uuid.uuid4())] = a
        return text("null")
    if name == "slow":
        time.sleep(float(a.get("ms", 0)) / 1000)
        log({"method": "done", "tool": "slow", "t": time.time()})
        return text(json.dumps("ok"))
    return text(f"unknown tool {name}", True)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def reply(self, status, body=b"", headers=()):
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_DELETE(self):
        sid = self.headers.get("Mcp-Session-Id")
        with lock:
            sessions.pop(sid, None)
        log({"method": "DELETE", "session": sid})
        self.reply(200)

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if args.token and self.headers.get("Authorization") != f"Bearer {args.token}":
            return self.reply(401, b'{"error":"unauthorized"}', [("Content-Type", "application/json")])
        msg = json.loads(body)
        method = msg.get("method")
        sid = self.headers.get("Mcp-Session-Id")
        entry = {"method": method, "session": sid, "t": time.time(),
                 "protocol": self.headers.get("MCP-Protocol-Version"),
                 "accept": self.headers.get("Accept"),
                 "custom": self.headers.get("X-Tenant")}
        if method == "tools/call":
            entry["tool"] = msg["params"]["name"]
        log(entry)
        extra = []
        if method == "initialize":
            sid = uuid.uuid4().hex
            with lock:
                sessions[sid] = 0
            extra.append(("Mcp-Session-Id", sid))
        else:
            with lock:
                known = sid in sessions
                if known:
                    sessions[sid] += 1
                    if args.forget_after and sessions[sid] >= args.forget_after:
                        sessions.pop(sid)
            if not known:
                return self.reply(404, b'{"error":"unknown session"}', [("Content-Type", "application/json")])
        if "id" not in msg:
            return self.reply(202)  # a notification
        if method == "initialize":
            result = {"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
                      "capabilities": {"tools": {}},
                      "serverInfo": {"name": "remote-store", "version": "0.1"}}
        elif method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            p = msg["params"]
            result = call(p["name"], p.get("arguments", {}), p.get("_meta", {}))
        else:
            out = {"jsonrpc": "2.0", "id": msg["id"],
                   "error": {"code": -32601, "message": f"unknown method {method}"}}
            return self.reply(200, json.dumps(out).encode(), [("Content-Type", "application/json")] + extra)
        out = {"jsonrpc": "2.0", "id": msg["id"], "result": result}
        if args.sse:
            note = {"jsonrpc": "2.0", "method": "notifications/message",
                    "params": {"level": "info", "data": "working"}}
            stream = (f"event: message\ndata: {json.dumps(note)}\n\n"
                      f"id: 1\nevent: message\ndata: {json.dumps(out)}\n\n").encode()
            return self.reply(200, stream, [("Content-Type", "text/event-stream")] + extra)
        self.reply(200, json.dumps(out).encode(), [("Content-Type", "application/json")] + extra)


def main():
    global args
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=0)
    p.add_argument("--token")
    p.add_argument("--sse", action="store_true")
    p.add_argument("--forget-after", type=int, default=0)
    args = p.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"listening on {server.server_address[1]}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    sys.exit(main())
