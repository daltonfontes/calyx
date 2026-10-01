"""Checks, by running Calyx, the classifications of the real-bug study that
count in Calyx's favour (and the one gap found while checking them).

Each check writes the workflow of an issue in Calyx (bench/real_bugs/verify/)
and asserts what docs/evaluation/bugs-reais.md says about it. Results go to
bench/results/real_bugs_verify.json.

    python bench/real_bugs/run_verify.py
"""
import json
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
DIR = os.path.join(HERE, "verify")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
STORE = os.path.join(DIR, ".store.json")


def calyx(*args: str) -> subprocess.CompletedProcess:
    env = dict(os.environ, CALYX_FAKE_STORE=STORE)
    return subprocess.run([CALYX, *args], cwd=DIR, env=env, capture_output=True, text=True,
                          timeout=60)


def clean() -> None:
    shutil.rmtree(os.path.join(DIR, ".calyx"), ignore_errors=True)
    if os.path.exists(STORE):
        os.remove(STORE)


def run_id(out: subprocess.CompletedProcess) -> str:
    return next(l.split()[-1] for l in out.stderr.splitlines() if l.startswith("calyx: run "))


def payments() -> int:
    with open(STORE) as f:
        return len(json.load(f).get("payments", []))


def retry_pays_once() -> dict:
    """CrewAI 5802, with the payment keyed by the request: paid once."""
    clean()
    out = calyx("run", "retry_pays_once.clyx", "--request", "R9", "--order", "A100")
    calls = out.stderr.count("write refund")
    return {"ok": out.returncode == 0 and calls == 3 and payments() == 1,
            "refund_calls": calls, "payments": payments()}


def write_once_in_loop() -> dict:
    """The same loop with `refund` as `write once`: no warning, paid each time."""
    clean()
    src = open(os.path.join(DIR, "retry_pays_once.clyx")).read()
    src = src.replace("    effect write\n    idempotency_key request\n",
                      "    effect write once\n    on_uncertain pause\n")
    path = os.path.join(DIR, "_write_once_in_loop.clyx")
    with open(path, "w") as f:
        f.write(src)
    try:
        check = calyx("check", "_write_once_in_loop.clyx")
        out = calyx("run", "_write_once_in_loop.clyx", "--request", "R9", "--order", "A100")
    finally:
        os.remove(path)
    # What the study reports: the compiler says nothing and the store pays 3 times.
    return {"ok": check.returncode == 0 and not check.stderr.strip() and payments() == 3,
            "diagnostics": check.stderr.strip(), "payments": payments(),
            "exit": out.returncode}


def two_waits() -> dict:
    """LangGraph 6208: the second wait does not redo the first, nor the model call."""
    clean()
    first = calyx("run", "two_waits.clyx", "--request", "r", "--fake-models", "--quiet")
    rid = run_id(first)
    calyx("deliver", rid, "Approval", "Approved")
    mid = calyx("resume", rid, "--fake-models")
    calyx("deliver", rid, "Budget", '{"limit": 300}')
    last = calyx("resume", rid, "--fake-models")
    return {
        "ok": first.returncode == 4 and mid.returncode == 4 and "Budget" in mid.stderr
        and last.returncode == 0 and last.stdout.startswith("aprovado até 300")
        and "0 model call(s)" in mid.stderr and "0 model call(s)" in last.stderr,
        "after_first_answer": "waiting" if mid.returncode == 4 else mid.returncode,
        "output": last.stdout.strip(),
    }


def stuck_agent() -> dict:
    """CrewAI 737, LangGraph 5099: the same call three turns in a row stops the agent."""
    clean()
    out = calyx("run", "stuck_agent.clyx", "--question", "x")
    turns = out.stderr.count("fake-stuck(investigate)")
    return {"ok": out.returncode != 0 and "preso" in out.stderr and turns == 3,
            "turns": turns, "max_turns": 20}


def human_gate() -> dict:
    """CrewAI 960, AutoGen 6819: the next step does not start before the approval."""
    clean()
    first = calyx("run", "human_gate.clyx", "--topic", "t", "--fake-models")
    rid = run_id(first)
    before = first.stderr.count(" llm ")
    calyx("deliver", rid, "Approval", "Approved")
    last = calyx("resume", rid, "--fake-models")
    return {"ok": first.returncode == 4 and before == 1 and "fake-model(market)" not in first.stderr
            and "fake-model(market)" in last.stderr and last.returncode == 0,
            "model_calls_before_approval": before}


CHECKS = {
    "crewai-5802 pagamento com chave num laço de novas tentativas": retry_pays_once,
    "crewai-5802 pagamento `write once` num laço (lacuna)": write_once_in_loop,
    "langgraph-6208 duas esperas no mesmo passo": two_waits,
    "crewai-737 / langgraph-5099 agente repetindo a mesma chamada": stuck_agent,
    "crewai-960 / autogen-6819 próximo passo antes da aprovação": human_gate,
}


def main() -> None:
    if not os.path.exists(CALYX):
        sys.exit("build first: make build")
    results = {}
    try:
        for name, check in CHECKS.items():
            r = check()
            results[name] = r
            print(f"{'ok ' if r['ok'] else 'FALHOU'} {name}: {r}", flush=True)
    finally:
        clean()
    with open(os.path.join(ROOT, "bench", "results", "real_bugs_verify.json"), "w") as f:
        json.dump(results, f, indent=1, ensure_ascii=False)
    if not all(r["ok"] for r in results.values()):
        sys.exit(1)


if __name__ == "__main__":
    main()
