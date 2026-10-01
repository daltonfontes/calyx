#!/usr/bin/env python3
"""Tools that work inside a sandbox, served over MCP (stdio).

Every tool receives `box`: the path of the sandbox, lent by the program
(`reads repo` or `edits repo`); the model never chooses it. Paths given by
the model are relative to the box and may not leave it.

- `list_files(box)`, `read_file(box, path)`: read.
- `edit_file(box, path, old, new)`: replaces `old`, which must appear
  exactly once. A `new` containing `__crash_once__` writes the change and
  then crashes, the first time only: the runtime undoes the half-done call
  and repeats it.
- `run_tests(box)`: `python3 -m unittest` in the box (no `.pyc` files, so
  reading leaves the box as it was).
- `diff(box)`: what changed since the run started, against the sandbox's
  first snapshot (`<box>.snapshots/base`, kept by the runtime).
"""
import difflib
import json
import os
import subprocess
import sys


def schema(**props):
    return {"type": "object", "properties": {k: {"type": "string"} for k in props},
            "required": list(props)}


TOOLS = [
    {"name": "list_files", "description": "Lista os arquivos.", "inputSchema": schema(box=1)},
    {"name": "read_file", "description": "Lê um arquivo.", "inputSchema": schema(box=1, path=1)},
    {"name": "edit_file", "description": "Troca um trecho de um arquivo.",
     "inputSchema": schema(box=1, path=1, old=1, new=1)},
    {"name": "run_tests", "description": "Roda os testes.", "inputSchema": schema(box=1)},
    {"name": "diff", "description": "O que mudou.", "inputSchema": schema(box=1)},
]


class Refused(Exception):
    pass


def inside(box, path):
    full = os.path.realpath(os.path.join(box, path))
    if not full.startswith(os.path.realpath(box) + os.sep):
        raise Refused(f"`{path}` is outside the repository")
    return full


def files(box):
    out = []
    for root, dirs, names in os.walk(box):
        dirs[:] = sorted(d for d in dirs if d != "__pycache__")
        for n in sorted(names):
            out.append(os.path.relpath(os.path.join(root, n), box))
    return out


def base_files(box):
    store = box + ".snapshots"
    with open(os.path.join(store, "base")) as f:
        manifest_hash = f.read().strip()
    with open(os.path.join(store, "manifests", manifest_hash + ".json")) as f:
        manifest = json.load(f)
    out = {}
    for path, entry in manifest.items():
        with open(os.path.join(store, "blobs", entry["hash"]), encoding="utf-8",
                  errors="replace") as f:
            out[path] = f.read()
    return out


def call(name, args):
    box = args.get("box", "")
    if not os.path.isdir(box):
        raise Refused(f"no sandbox at `{box}`")
    if name == "list_files":
        return "\n".join(files(box))
    if name == "read_file":
        with open(inside(box, args.get("path", "")), encoding="utf-8") as f:
            return f.read()
    if name == "edit_file":
        path = inside(box, args.get("path", ""))
        with open(path, encoding="utf-8") as f:
            text = f.read()
        old, new = args.get("old", ""), args.get("new", "")
        count = text.count(old) if old else 0
        if count != 1:
            raise Refused(f"`old` appears {count} times in {args.get('path')}; it must appear once")
        with open(path, "w", encoding="utf-8") as f:
            f.write(text.replace(old, new))
        marker = box + ".crashed"
        if "__crash_once__" in new and not os.path.exists(marker):
            open(marker, "w").close()
            sys.exit(3)  # half done: written, never answered
        return f"edited {args.get('path')}"
    if name == "run_tests":
        env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
        p = subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", ".", "-t", "."],
                           cwd=box, env=env, capture_output=True, text=True, timeout=50)
        return (p.stdout + p.stderr).strip() or "no output"
    if name == "diff":
        before = base_files(box)
        out = []
        for path in sorted(set(before) | set(files(box))):
            old = before.get(path, "")
            try:
                with open(os.path.join(box, path), encoding="utf-8", errors="replace") as f:
                    new = f.read()
            except FileNotFoundError:
                new = ""
            if old != new:
                out.extend(difflib.unified_diff(old.splitlines(True), new.splitlines(True),
                                                f"a/{path}", f"b/{path}"))
        return "".join(out) or "no changes"
    raise Refused(f"unknown tool {name}")


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
        continue
    if method == "initialize":
        answer(msg_id, {
            "protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "sandbox-tools", "version": "0.1"},
        })
    elif method == "tools/list":
        answer(msg_id, {"tools": TOOLS})
    elif method == "tools/call":
        p = msg["params"]
        try:
            text = call(p.get("name"), p.get("arguments", {}))
            answer(msg_id, {"content": [{"type": "text", "text": text}]})
        except (Refused, OSError, subprocess.TimeoutExpired) as e:
            answer(msg_id, {"content": [{"type": "text", "text": str(e)}], "isError": True})
    else:
        answer(msg_id, error={"code": -32601, "message": f"unknown method {method}"})
