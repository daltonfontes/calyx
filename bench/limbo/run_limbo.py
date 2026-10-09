"""Runs the Calyx programs on LIMBO (github.com/jaxblack/limbo-bench).

For each episode (task template, instance, focal write, fault mode) this:

1. builds LIMBO's own SandboxSession (world, fault injector, ledger) with the
   `vanilla` policy, so nothing in LIMBO's harness helps;
2. serves it on localhost with LIMBO's SandboxServer;
3. runs `calyx run` on programs/tools_<contract>.clyx + programs/tasks.clyx,
   with the tools served by adapter.py;
4. when a `write once` with `on_uncertain pause` stops the run, plays the
   person: asks LIMBO's operator (`escalate_to_human`, 15 simulated minutes,
   counted like any escalation) and resumes with `--uncertain done`,
   `retry` or `failed` from what the operator found;
5. calls LIMBO's `finish` with `completed` only if the run ended well, and
   grades with LIMBO's own grader (session.record).

    python bench/limbo/run_limbo.py --limbo PATH [--contract native|keys_everywhere]
        [--modes ...] [--templates ...] [--index 0] [--out FILE]

PATH is a clone of limbo-bench (tested at commit 2db09fb). Default: LIMBO's E2
design, expanded by LIMBO's own `runner.expand`.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
CALYX = os.path.join(ROOT, "target", "release", "calyx")
E2_MODES = ["none", "timeout_pre", "timeout_post", "timeout_late", "http500_post", "partial_timeout",
            "duplicate_delivery", "http503_transient"]


def quoted(instr: str, after: str) -> str:
    """The first double-quoted text after `after` in the instruction."""
    i = instr.index(after)
    return re.search(r'"([^"]*)"', instr[i:]).group(1)


def params(task) -> dict:
    """The graph's parameters, read from the instance (its instruction and targets)."""
    t, ins = task.template, task.instruction
    targets = task.targets
    if t == "release_announcement":
        plats = [x.match["platform"] for x in targets if x.kind == "post"]
        return {"text": quoted(ins, "Publish this exact text"), "first": plats[0], "second": plats[1],
                "title": quoted(ins, "titled"), "subject": quoted(ins, "with subject")}
    if t == "invoice_batch":
        lines = [{"customer": x.match["customer_id"], "amount": x.match["amount_cents"]}
                 for x in targets if x.kind == "charge"]
        return {"invoice": re.search(r"invoice (INV-\d+)", ins).group(1), "lines": lines}
    if t == "incident_open":
        svc = re.search(r"The (\S+) service", ins).group(1)
        title = quoted(ins, "titled")
        return {"service": svc, "title": title,
                "body": f"{svc} has had an elevated error rate for 20 minutes; on-call is investigating.",
                "note": quoted(ins, "Add this comment"), "subject": quoted(ins, "with subject")}
    if t == "migration_log":
        rows = json.loads(re.search(r"single db_insert_many call: (\[.*\])", ins).group(1))
        audit = json.loads(re.search(r"table audit_log: (\{.*\})", ins).group(1))
        return {"rows": [{k: str(v) for k, v in r.items()} for r in rows],
                "audit": {k: str(v) for k, v in audit.items()}}
    if t == "deploy_release":
        m = re.search(r"Roll out (\S+) version (\S+)\.", ins)
        return {"service": m.group(1), "version": m.group(2),
                "change": re.search(r"change ticket (\S+) with", ins).group(1)}
    if t == "refund_duplicate":
        m = re.search(r"Refund the duplicate charge (\S+) only", ins)
        return {"charge_id": m.group(1), "email": re.search(r"Email the customer at (\S+) ", ins).group(1),
                "invoice": re.search(r"invoice (INV-\d+)", ins).group(1)}
    if t == "feature_flags":
        rows = json.loads(re.search(r"key_field \"flag\": (\[.*\])", ins).group(1))
        return {"rows": [{k: str(v).lower() if isinstance(v, bool) else str(v) for k, v in r.items()} for r in rows],
                "text": quoted(ins, "Publish this exact text")}
    if t == "customer_notice":
        return {"emails": re.findall(r"Send one email to (\S+) with", ins),
                "date": re.search(r"maintenance on ([A-Z][a-z]+ \d+)\.", ins).group(1)}
    if t == "cross_post":
        return {"text": quoted(ins, "Publish this exact text"),
                "product": re.search(r"Promote the (\S+) live", ins).group(1)}
    if t == "subscription_upgrade":
        charge = next(x for x in targets if x.kind == "charge")
        plan = re.search(r"upgrade to the (\S+) plan", ins).group(1)
        email = next(x for x in targets if x.kind == "mail").match["to"]
        cid = charge.match["customer_id"]
        return {"customer": cid, "amount": charge.match["amount_cents"], "plan": plan,
                "row": {"customer_id": cid, "plan": plan}, "email": email}
    if t == "ticket_resolution":
        return {"key": re.search(r"Close out incident (\S+) ", ins).group(1),
                "cause": re.search(r"Root cause: (.*?)\. Fix deployed", ins).group(1)}
    if t == "hotfix_long":
        m = re.search(r"rollout for (\S+) version (\S+):", ins)
        audit = json.loads(re.search(r"table audit_log: (\{.*\})", ins).group(1))
        return {"service": m.group(1), "version": m.group(2), "text": quoted(ins, "Publish this exact text"),
                "audit": {k: str(v) for k, v in audit.items()}}
    raise ValueError(t)


def run_calyx(argv: list[str], cwd: str, env: dict) -> tuple[int, str]:
    out = subprocess.run([CALYX, *argv], cwd=cwd, env=env, capture_output=True, text=True, timeout=600)
    return out.returncode, out.stderr + out.stdout


def operator_decision(obs: dict, tool_names: set[str]) -> str:
    """`done`, `retry` or `failed`, from what LIMBO's operator found."""
    findings = ((obs.get("result") or {}).get("findings")) or []
    if not obs.get("ok") or not findings:
        return "failed"
    f = findings[-1]["operator_finding"]
    if f == "DID take effect":
        return "done"
    if f == "did NOT take effect":
        return "retry"
    return "failed"  # partly, or could not be determined


def episode(spec_cls, session_cls, server_cls, template, index, focal, mode, contract, workdir) -> dict:
    spec = spec_cls(template=template, index=index, focal=focal, mode=mode, model="calyx",
                    policy="vanilla", experiment="calyx", contract=contract)
    session = session_cls(spec)
    task = session.task
    prog = os.path.join(workdir, "prog.clyx")
    tools = "tools_keys.clyx" if contract == "keys_everywhere" else "tools_native.clyx"
    with open(prog, "w") as f:
        for part in (tools, "tasks.clyx"):
            f.write(open(os.path.join(HERE, "programs", part)).read())
            f.write("\n")
    names = re.findall(r"^tool (\w+)\(", open(prog).read(), re.M)
    adapter = os.path.join(HERE, "adapter.py")
    with open(os.path.join(workdir, "calyx.toml"), "w") as f:
        for n in names:
            f.write(f'[tools.{n}]\ncommand = ["{sys.executable}", "-I", "{adapter}"]\n\n')
    argv = ["run", prog, "--graph", template, "--deterministic", "--config", os.path.join(workdir, "calyx.toml"),
            "--run", f"{spec.episode_id}"]
    for k, v in params(task).items():
        argv += [f"--{k}", v if isinstance(v, str) else json.dumps(v)]
    log = []
    pauses = 0
    with server_cls(session) as server:
        env = dict(os.environ, LIMBO_PORT=str(server.port), LIMBO_TOKEN=server.token)
        t0 = time.time()
        code, out = run_calyx(argv, workdir, env)
        log.append(out)
        run_id = None
        while code != 0 and "--uncertain" in out and pauses < 5:
            pauses += 1
            run_id = run_id or latest_run(workdir)
            obs = session.call("escalate_to_human", {"question": "Did the last write take effect?"})
            decision = operator_decision(obs, set(names))
            log.append(f"[operator] {json.dumps(obs)[:400]} -> --uncertain {decision}")
            code, out = run_calyx(["resume", run_id, "--deterministic", "--config", os.path.join(workdir, "calyx.toml"),
                                   "--uncertain", decision], workdir, env)
            log.append(out)
        wall = time.time() - t0
        session.call("finish", {"status": "completed" if code == 0 else "failed",
                                "summary": "calyx run " + ("ended" if code == 0 else "failed")})
        rec = session.record(final=True)
    g = rec["grade"]
    return {
        "episode_id": rec["episode_id"], "template": template, "index": index, "focal": focal, "mode": mode,
        "contract": contract, "fault_truth": rec["fault_truth"], "fault_triggered": rec["fault_triggered"],
        "TS": g["TS"], "EOS": g["EOS"], "dup_executed": g["dup_executed"], "dup_live": g["dup_live"],
        "goal_met": g["goal_met"], "collateral": len(g["collateral"]), "overclaim": rec["overclaim"],
        "calyx_exit": code, "pauses": pauses, "escalations": rec["escalations"],
        "human_minutes": rec["human_minutes"], "virtual_seconds": rec["virtual_seconds"],
        "n_calls": rec["n_agent_calls"], "wall_s": round(wall, 2), "targets": g["targets"],
        "log": short("\n".join(log)),
    }


KEEP = re.compile(r"failed|verify|repeated|taken as done|\[operator\]|finished|error|warning|uncertain")


def short(log: str) -> str:
    """The lines of the trace that tell what happened to the faulty call."""
    return "\n".join(line for line in log.splitlines() if KEEP.search(line))[-1500:]


def latest_run(workdir: str) -> str:
    runs = os.path.join(workdir, ".calyx", "runs")
    return max(os.listdir(runs), key=lambda d: os.path.getmtime(os.path.join(runs, d)))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limbo", required=True)
    ap.add_argument("--contract", default="native", choices=["native", "keys_everywhere"])
    ap.add_argument("--modes", nargs="*", default=E2_MODES)
    ap.add_argument("--templates", nargs="*", default=None)
    ap.add_argument("--index", type=int, default=0)
    ap.add_argument("--out", default=None)
    a = ap.parse_args()
    sys.path.insert(0, os.path.abspath(a.limbo))
    from limbo.agent import EpisodeSpec
    from limbo.runner import expand
    from limbo.sandbox_http import SandboxServer, SandboxSession
    from limbo.tasks import TEMPLATES

    out = a.out or os.path.join(ROOT, "bench", "results", f"limbo_{a.contract}.jsonl")
    rows = []
    # LIMBO's own expansion of a design: `none` on the first focal write only,
    # `partial_timeout` on batch writes only.
    specs = expand("calyx", a.templates or list(TEMPLATES), [a.index], a.modes, ["calyx"], ["vanilla"])
    for spec in specs:
        template, focal, mode = spec.template, spec.focal, spec.mode
        work = tempfile.mkdtemp(prefix="limbo-")
        try:
            r = episode(EpisodeSpec, SandboxSession, SandboxServer, template, a.index, focal, mode, a.contract, work)
        finally:
            shutil.rmtree(work, ignore_errors=True)
        rows.append(r)
        print(f"{template:22} {focal:20} {mode:19} TS={int(r['TS'])} EOS={int(r['EOS'])} "
              f"dup={r['dup_executed']} exit={r['calyx_exit']} pauses={r['pauses']} "
              f"overclaim={int(r['overclaim'])}", flush=True)
    with open(out, "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    n = len(rows)
    eos = sum(r["EOS"] for r in rows)
    print(f"\n{n} episodes: EOS {eos}/{n}, TS {sum(r['TS'] for r in rows)}/{n}, "
          f"dup {sum(1 for r in rows if r['dup_executed'] > 0)}/{n}, "
          f"overclaim {sum(r['overclaim'] for r in rows)}/{n}; {os.path.relpath(out)}")


if __name__ == "__main__":
    main()
