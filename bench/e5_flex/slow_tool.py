"""An MCP server (stdio) whose every tool takes 0.5 s and answers "ok":
the external write whose latency the E5 measures."""
import json
import sys
import time

for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("id") is None:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
                  "capabilities": {"tools": {}}, "serverInfo": {"name": "slow", "version": "0"}}
    elif method == "tools/list":
        result = {"tools": [{"name": n, "inputSchema": {"type": "object"}}
                            for n in ("email", "track", "cache_put")]}
    else:
        time.sleep(0.5)
        result = {"content": [{"type": "text", "text": "ok"}]}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}) + "\n")
    sys.stdout.flush()
