#!/usr/bin/env python3
"""A fake web search, served over MCP (stdio), for tests and development.

It speaks the minimum of the protocol Calyx uses: `initialize`,
`tools/list` and `tools/call`, as newline-delimited JSON-RPC 2.0. Answers
are made up from the query, so runs are repeatable and need no network.
Replace it in calyx.toml with a real search server when you have one.
"""
import json
import sys
import time

TOOL = {
    "name": "web_search",
    "description": "Busca na web (falsa): devolve resultados inventados a partir da consulta.",
    "inputSchema": {
        "type": "object",
        "properties": {"query": {"type": "string"}},
        "required": ["query"],
    },
}


def search(query):
    return "\n".join(
        f"[{i}] {query} - fonte de exemplo {i} (https://exemplo.org/{i}): "
        f"resumo inventado sobre \"{query}\", com dados fictícios para teste."
        for i in (1, 2, 3)
    )


def answer(msg_id, result=None, error=None):
    out = {"jsonrpc": "2.0", "id": msg_id}
    if error is None:
        out["result"] = result
    else:
        out["error"] = error
    sys.stdout.write(json.dumps(out, ensure_ascii=False) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method, msg_id = msg.get("method"), msg.get("id")
    if msg_id is None:
        continue  # notifications need no answer
    if method == "initialize":
        answer(msg_id, {
            "protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake-search", "version": "0.1"},
        })
    elif method == "tools/list":
        answer(msg_id, {"tools": [TOOL]})
    elif method == "tools/call":
        args = msg["params"].get("arguments", {})
        query = args.get("query", "")
        if "__release__" in query or "__nobody__" in query:
            # Shaped like GitHub's (structured tool returns): many fields the
            # program does not declare, and a long list of them.
            release = {"tag_name": "v9.9.9", "name": "v9.9.9", "body": "notas da versão",
                       "draft": False,
                       "assets": [{"id": i, "name": f"calyx-{i}.tar.gz", "size": 2000000,
                                   "uploader": {"login": "bot", "url": "https://exemplo.org/bot"}}
                                  for i in range(60)]}
            if "__nobody__" in query:
                del release["body"]
            answer(msg_id, {"content": [{"type": "text", "text": json.dumps(release)}]})
        elif "__prs__" in query:
            prs = [{"number": i, "title": f"PR {i}", "body": "x" * 500, "state": "closed",
                    "user": {"login": "ana"}} for i in range(1, 51)]
            answer(msg_id, {"content": [{"type": "text", "text": json.dumps(prs)}]})
        elif "__big__" in query:
            # Large outputs test how the journal stores big answers (by hash, D20).
            answer(msg_id, {"content": [{"type": "text", "text": "x" * 10000}]})
        elif "__slow__" in query:
            # A slow source: races test that the other branch wins (D12).
            time.sleep(1.5)
            answer(msg_id, {"content": [{"type": "text", "text": search(query)}]})
        elif "__fail__" in query:
            answer(msg_id, {"content": [{"type": "text", "text": "falha simulada"}], "isError": True})
        else:
            answer(msg_id, {"content": [{"type": "text", "text": search(query)}]})
    else:
        answer(msg_id, error={"code": -32601, "message": f"unknown method {method}"})
