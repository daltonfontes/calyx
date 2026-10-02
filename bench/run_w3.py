"""W3: human approval with a deadline, with the process stopped while it waits.

The same flow (get_order -> decide -> wait for a person, 3 s deadline ->
pay and e-mail, or e-mail the refusal) in Calyx (`receive`), LangGraph
(`interrupt()`) and Temporal (signal + `wait_condition` with a timeout). In
every scenario the process ends while the run waits; then each step of the
scenario is done the way each system offers:

- `deliver`: the person answers `Approved` (Calyx: `calyx deliver`;
  LangGraph: `Command(resume=...)`, which runs the rest; Temporal: a signal);
- `continue`: what runs on a schedule (Calyx: `calyx tick`; LangGraph:
  nothing, or the careful version's own check of the deadline; Temporal: a
  worker until the workflow ends);
- `wait`: 3.5 s go by, past the deadline.

Counted in the store afterwards: payments and e-mails, and which e-mail went
out; and model calls (one is needed). Results go to bench/results/w3.json.

    python bench/run_w3.py
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
W3 = os.path.join(HERE, "w3_approval")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
PY = os.environ.get("BENCH_PYTHON", sys.executable)
TEMPORAL_BIN = os.environ.get("TEMPORAL_BIN")
TEMPORAL_PORT = "7298"
WORK = os.path.join(W3, ".work")
STORE = os.path.join(WORK, "store.json")
LLM_LOG = os.path.join(WORK, "llm.log")

APPROVED, REFUSED = "aprovado", "recusado"
SCENARIOS = {
    "aprovada": (["deliver", "continue"], APPROVED),
    "prazo vence com tudo parado": (["wait", "continue"], REFUSED),
    "resposta em dobro": (["deliver", "continue", "deliver", "continue"], APPROVED),
    "resposta atrasada, depois da recusa": (["wait", "continue", "deliver", "continue"], REFUSED),
    "resposta atrasada, antes da retomada": (["wait", "deliver", "continue"], REFUSED),
    "resposta no prazo, retomada depois": (["deliver", "wait", "continue"], APPROVED),
}


def env() -> dict:
    e = dict(os.environ)
    e.update(
        CALYX_FAKE_STORE=STORE,
        BENCH_LLM_LOG=LLM_LOG,
        BENCH_LATENCY="0",
        BENCH_CHECKPOINT=os.path.join(WORK, "checkpoints.sqlite"),
        TEMPORAL_ADDRESS=f"127.0.0.1:{TEMPORAL_PORT}",
    )
    return e


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=W3, env=env(), capture_output=True, text=True, timeout=60)


class Calyx:
    def __init__(self, case: str):
        self.id = ""
        self.stderr = ""

    def step(self, what: str) -> None:
        if what == "start":
            out = run([CALYX, "run", "approval.clyx", "--graph", "approve", "--fake-models",
                       "--request", "R1", "--order", "A100", "--message", "chegou quebrado"])
            self.id = next(l.split()[-1] for l in out.stderr.splitlines()
                           if l.startswith("calyx: run "))
        elif what == "deliver":
            out = run([CALYX, "deliver", self.id, "Approval", "Approved"])
        else:
            out = run([CALYX, "tick", "--fake-models"])
        self.stderr += out.stderr

    def llm_calls(self) -> int:
        return sum(1 for l in self.stderr.splitlines() if " llm " in l and "tokens" in l)


class Python:
    def __init__(self, script: str, case: str, flags: list[str]):
        self.script, self.flags = script, flags
        self.id = f"{case}-{script.split('_')[0]}-{'-'.join(f.strip('-') for f in flags) or 'plain'}"

    def step(self, what: str) -> None:
        mode = {"start": ["start"], "deliver": ["deliver"], "continue": ["tick"]}[what]
        extra = ["Approved"] if what == "deliver" else []
        run([PY, self.script, mode[0], self.id, *extra, *self.flags])

    def llm_calls(self) -> int:
        try:
            with open(LLM_LOG) as f:
                return len(f.read().splitlines())
        except FileNotFoundError:
            return 0


def systems(case: str) -> dict:
    out = {
        "calyx": lambda: Calyx(case),
        "langgraph": lambda: Python("langgraph_.py", case, []),
        "langgraph + cuidado manual": lambda: Python("langgraph_.py", case, ["--careful"]),
    }
    if TEMPORAL_BIN:
        out["temporal"] = lambda: Python("temporal_.py", case, [])
        out["temporal + cuidado manual"] = lambda: Python("temporal_.py", case, ["--careful"])
    return out


def one(make, steps: list[str], expected: str) -> dict:
    shutil.rmtree(WORK, ignore_errors=True)
    shutil.rmtree(os.path.join(W3, ".calyx"), ignore_errors=True)
    os.makedirs(WORK)
    s = make()
    s.step("start")
    for what in steps:
        if what == "wait":
            time.sleep(3.5)
        else:
            s.step(what)
    try:
        with open(STORE) as f:
            db = json.load(f)
    except FileNotFoundError:
        db = {}
    payments, outbox = len(db.get("payments", [])), db.get("outbox", [])
    bodies = [m["body"] for m in outbox]
    got = (APPROVED if payments and bodies and bodies[0].startswith("Reembolsamos")
           else REFUSED if not payments and bodies and bodies[0].startswith("Não aprovado")
           else "nenhum" if not payments and not bodies else "inconsistente")
    calls = s.llm_calls()
    return {
        "payments": payments, "emails": len(outbox), "outcome": got, "expected": expected,
        "llm_calls": calls,
        "correct": got == expected and len(outbox) == 1 and payments <= 1 and calls == 1,
    }


def start_temporal() -> subprocess.Popen | None:
    if not TEMPORAL_BIN:
        print("TEMPORAL_BIN not set: Temporal is left out", flush=True)
        return None
    server = subprocess.Popen(
        [TEMPORAL_BIN, "server", "start-dev", "--headless", "--port", TEMPORAL_PORT,
         "--log-level", "error"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
    )
    for _ in range(150):
        ok = subprocess.run([TEMPORAL_BIN, "operator", "namespace", "describe", "default",
                             "--address", f"127.0.0.1:{TEMPORAL_PORT}"], capture_output=True)
        if ok.returncode == 0:
            return server
        time.sleep(0.2)
    raise SystemExit("the Temporal dev server did not start")


def main() -> None:
    server = start_temporal()
    rows = []
    try:
        for name, (steps, expected) in SCENARIOS.items():
            case = f"c{time.time_ns()}"
            for system, make in systems(case).items():
                r = {"scenario": name, "system": system, **one(make, steps, expected)}
                rows.append(r)
                print(f"{name:38} {system:28} {'ok ' if r['correct'] else 'ERRO'} "
                      f"resultado={r['outcome']:13} pagamentos={r['payments']} "
                      f"e-mails={r['emails']} llm={r['llm_calls']}", flush=True)
                with open(os.path.join(HERE, "results", "w3.json"), "w") as f:
                    json.dump(rows, f, indent=1, ensure_ascii=False)
    finally:
        if server:
            os.killpg(server.pid, signal.SIGKILL)
        shutil.rmtree(WORK, ignore_errors=True)
        shutil.rmtree(os.path.join(W3, ".calyx"), ignore_errors=True)


if __name__ == "__main__":
    main()
