"""What MCP annotations catch when a tool is declared wrong.

The weak point of Calyx's contracts is the declaration itself: if the
programmer declares a tool's effect wrong, the compiler trusts it. This
takes every tool of programs/tools_native.clyx and declares it, in turn,
each way a programmer could:

    read | write (no key) | write with a key | write once (pause)

then runs `calyx check --tools` against a live LIMBO sandbox, whose MCP
server (adapter.py) passes on LIMBO's annotations. Each declaration is
classed with LIMBO's own contracts (services.build_tools, the truth the
agent never sees):

- dangerous: the runtime may repeat a write that is not idempotent: a
  non-idempotent write declared `read` or `write` without key, or with a
  key the service ignores;
- safe: the declaration is right, or only more careful than needed.

    python bench/limbo/declarations.py --limbo PATH [--key-hint]

Writes bench/results/limbo_declarations.json. With --key-hint, the adapter
also sends the proposed `idempotencyKeyHint` (docs/mcp/idempotency-key-hint.md)
and the results go to bench/results/limbo_declarations_keyhint.json.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
CALYX = os.path.join(ROOT, "target", "release", "calyx")
KINDS = ["read", "write", "write+key", "write once"]
# Two tasks that together have every service: hotfix_long (tickets, social,
# deploy, data, mail) and subscription_upgrade (billing, data, mail).
TASKS = ["hotfix_long", "subscription_upgrade"]
# Under the native contract, the services that honor a key.
HONORS_KEY = {"publish_keyed", "charge"}


def tools(text: str) -> list[tuple[str, list[str], str]]:
    """(name, params, return type) of each tool declaration."""
    out = []
    for m in re.finditer(r"^tool (\w+)\((.*?)\) -> (.+):$", text, re.M):
        params = [p.split(":")[0].strip() for p in m.group(2).split(",") if p.strip()]
        out.append((m.group(1), params, m.group(3)))
    return out


def program(text: str, kind: str) -> str:
    """Every tool declared as `kind`; `verify` becomes `pause`, so no
    declaration depends on another."""
    out = []
    for name, params, ret in tools(text):
        sig = re.search(rf"^tool {name}\(.*$", text, re.M).group(0)
        body = {
            "read": "    effect read",
            "write": "    effect write",
            "write+key": f"    effect write\n    idempotency_key {params[0]}",
            "write once": "    effect write once\n    on_uncertain pause",
        }[kind]
        out.append(f"{sig}\n{body}\n")
    return "\n".join(out)


def truth(contracts, wrapped: str, name: str) -> dict:
    c = contracts[wrapped].contract
    return {"write": c.write, "idempotent": bool(c.idempotent or not c.write),
            "honors_key": name in HONORS_KEY}


def dangerous(kind: str, t: dict) -> bool:
    risky = t["write"] and not t["idempotent"]
    if kind in ("read", "write"):
        return risky
    if kind == "write+key":
        return risky and not t["honors_key"]
    return False


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limbo", required=True)
    ap.add_argument("--key-hint", action="store_true")
    a = ap.parse_args()
    sys.path.insert(0, os.path.abspath(a.limbo))
    sys.path.insert(0, HERE)
    from adapter import WRAPS
    from limbo.agent import EpisodeSpec
    from limbo.sandbox_http import SandboxServer, SandboxSession
    from limbo.services import build_tools
    from limbo.tasks import make_task

    contracts = build_tools("native")
    text = open(os.path.join(HERE, "programs", "tools_native.clyx")).read()
    names = [n for n, _, _ in tools(text)]
    found: dict[tuple[str, str], set[str]] = {}
    covered: set[str] = set()
    work = tempfile.mkdtemp(prefix="limbo-decl-")
    adapter = os.path.join(HERE, "adapter.py")
    with open(os.path.join(work, "calyx.toml"), "w") as f:
        for n in names:
            f.write(f'[tools.{n}]\ncommand = ["{sys.executable}", "-I", "{adapter}"]\n\n')
    for task in TASKS:
        # With no fault, the focal write does not matter: the first one.
        spec = EpisodeSpec(template=task, index=0, focal=make_task(task, 0).focals[0].label, mode="none",
                           model="calyx")
        session = SandboxSession(spec)
        listed = {t["name"] for t in session.list_tools()}
        covered |= {n for n in names if WRAPS[n] in listed}
        with SandboxServer(session) as server:
            env = dict(os.environ, LIMBO_PORT=str(server.port), LIMBO_TOKEN=server.token,
                       LIMBO_KEY_HINT="1" if a.key_hint else "0")
            for kind in KINDS:
                path = os.path.join(work, f"{kind.replace(' ', '_').replace('+', '_')}.clyx")
                with open(path, "w") as f:
                    f.write(program(text, kind))
                out = subprocess.run([CALYX, "check", path, "--tools"], cwd=work, env=env,
                                     capture_output=True, text=True).stdout
                if "error[" in out:
                    sys.exit(f"calyx check --tools failed:\n{out}")
                for code, tool in re.findall(r"warning\[(W070\d)\]: tool `(\w+)`", out):
                    if WRAPS[tool] in listed:
                        found.setdefault((tool, kind), set()).add(code)
    rows = []
    for n in names:
        if n not in covered:
            continue
        t = truth(contracts, WRAPS[n], n)
        for kind in KINDS:
            codes = sorted(found.get((n, kind), set()))
            rows.append({"tool": n, "limbo_tool": WRAPS[n], "declared": kind, **t,
                         "dangerous": dangerous(kind, t), "warned": codes})
    danger = [r for r in rows if r["dangerous"]]
    safe = [r for r in rows if not r["dangerous"]]
    print(f"{'tool':16} {'limbo tool':22} " + " ".join(f"{k:>11}" for k in KINDS))
    for n in names:
        rs = [r for r in rows if r["tool"] == n]
        if not rs:
            continue
        cells = []
        for r in rs:
            mark = ("!" if r["dangerous"] else ".") + ("W" if r["warned"] else "-")
            cells.append(f"{mark:>11}")
        print(f"{n:16} {rs[0]['limbo_tool']:22} " + " ".join(cells))
    print("\n! dangerous declaration, . safe; W warned, - not warned")
    caught = sum(1 for r in danger if r["warned"])
    false = sum(1 for r in safe if r["warned"])
    by_kind = {k: (sum(1 for r in danger if r["declared"] == k and r["warned"]),
                   sum(1 for r in danger if r["declared"] == k)) for k in KINDS}
    print(f"dangerous declarations warned: {caught}/{len(danger)}  "
          + ", ".join(f"{k}: {c}/{n}" for k, (c, n) in by_kind.items() if n))
    print(f"safe declarations warned: {false}/{len(safe)}")
    name = "limbo_declarations_keyhint.json" if a.key_hint else "limbo_declarations.json"
    with open(os.path.join(ROOT, "bench", "results", name), "w") as f:
        json.dump({"rows": rows, "dangerous": len(danger), "dangerous_warned": caught,
                   "safe": len(safe), "safe_warned": false,
                   "by_kind": {k: list(v) for k, v in by_kind.items()}}, f, indent=1)


if __name__ == "__main__":
    main()
