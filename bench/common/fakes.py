"""What the Python baselines share with Calyx's fakes, so every version
sees the same world: a model that answers after a fixed latency, the same
fake web search, and an MCP client for the same fake store
(examples/tools/fake_store.py).

The model counts its calls; each program prints `llm_calls=N` on stderr at
the end, which the harness reads (Calyx prints the same count in its trace).
"""
import asyncio
import atexit
import json
import os
import subprocess
import sys
import threading
import time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LATENCY = float(os.environ.get("BENCH_LATENCY", "1.0"))

_calls = 0
_calls_lock = threading.Lock()


def _count():
    global _calls
    with _calls_lock:
        _calls += 1
        # Survives `kill -9`: one line per call, for the recovery benchmark.
        log = os.environ.get("BENCH_LLM_LOG")
        if log:
            with open(log, "a") as f:
                f.write("call\n")


_retries = 0
atexit.register(lambda: print(f"llm_calls={_calls} llm_retries={_retries}", file=sys.stderr))


def _answer(prompt: str) -> str:
    first = next((line for line in prompt.splitlines() if line.strip()), "")
    return f"[resposta falsa para: {first[:60]}]"


# With BENCH_MODEL set (e.g. gemini-3.5-flash-lite), the model is real: the
# same OpenAI-compatible endpoint Calyx's runtime calls, the key from
# GEMINI_API_KEY, and the same retries (4 attempts, 1-2-4 s, longer when
# the provider asks, up to 60 s).
MODEL = os.environ.get("BENCH_MODEL")
URL = "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"


def _real(prompt: str) -> str:
    import re
    import urllib.error
    import urllib.request

    body = json.dumps({"model": MODEL, "messages": [{"role": "user", "content": prompt}]}).encode()
    for attempt in range(1, 5):
        req = urllib.request.Request(URL, data=body, headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer " + os.environ["GEMINI_API_KEY"],
        })
        try:
            with urllib.request.urlopen(req, timeout=300) as r:
                return json.loads(r.read())["choices"][0]["message"]["content"]
        except urllib.error.HTTPError as e:
            if e.code not in (429, 500, 502, 503, 504) or attempt == 4:
                raise
            global _retries
            with _calls_lock:
                _retries += 1
            wait = (2 if e.code == 429 else 1) * 2 ** (attempt - 1)
            text = e.read().decode(errors="replace")
            m = re.search(r"retry in ([0-9.]+)\s*s", text) or re.search(r"([0-9.]+)", e.headers.get("Retry-After") or "")
            if m:
                wait = max(wait, min(float(m.group(1)), 60.0))
            time.sleep(wait)
    raise RuntimeError("unreachable")


def llm(prompt: str) -> str:
    """A model call: `LATENCY` seconds, then a deterministic answer (or the
    real model, with BENCH_MODEL)."""
    if MODEL:
        out = _real(prompt)
        _count()
        return out
    time.sleep(LATENCY)
    _count()
    return _answer(prompt)


async def allm(prompt: str) -> str:
    if MODEL:
        out = await asyncio.to_thread(_real, prompt)
        _count()
        return out
    await asyncio.sleep(LATENCY)
    _count()
    return _answer(prompt)


def search(query: str) -> str:
    """Same text as examples/tools/fake_search.py, without the process."""
    return "\n".join(
        f"[{i}] {query} - fonte de exemplo {i} (https://exemplo.org/{i}): "
        f'resumo inventado sobre "{query}", com dados fictícios para teste.'
        for i in (1, 2, 3)
    )


class Mcp:
    """A minimal MCP client over stdio (initialize + tools/call)."""

    def __init__(self, script: str):
        self.proc = subprocess.Popen(
            [sys.executable, os.path.join(ROOT, script)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        self.next_id = 0
        self.lock = threading.Lock()
        self._request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}})

    def _request(self, method: str, params: dict) -> dict:
        with self.lock:
            self.next_id += 1
            msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
            assert self.proc.stdin and self.proc.stdout
            self.proc.stdin.write(json.dumps(msg) + "\n")
            self.proc.stdin.flush()
            return json.loads(self.proc.stdout.readline())

    def call(self, tool: str, args: dict, meta: dict | None = None) -> str:
        params: dict = {"name": tool, "arguments": args}
        if meta:
            params["_meta"] = meta
        result = self._request("tools/call", params)["result"]
        text = "\n".join(c["text"] for c in result["content"] if c["type"] == "text")
        if result.get("isError"):
            raise RuntimeError(f"{tool} failed: {text}")
        return text
