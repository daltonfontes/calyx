"""An MCP server (stdio) whose every tool takes 0.5 s and answers "ok":
the external write whose latency the E5 measures. Calls are answered in
threads, so several can be in flight at once, as MCP servers built on the
official SDKs do."""
import json
import sys
import threading
import time

out = threading.Lock()


def answer(msg_id, result):
    with out:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}) + "\n")
        sys.stdout.flush()


def call(msg_id):
    time.sleep(0.5)
    answer(msg_id, {"content": [{"type": "text", "text": "ok"}]})


for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("id") is None:
        continue
    method = msg.get("method")
    if method == "initialize":
        answer(msg["id"], {"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
                           "capabilities": {"tools": {}},
                           "serverInfo": {"name": "slow", "version": "0"}})
    elif method == "tools/list":
        answer(msg["id"], {"tools": [{"name": n, "inputSchema": {"type": "object"}}
                                     for n in ("email", "track", "cache_put")]})
    else:
        threading.Thread(target=call, args=(msg["id"],)).start()
