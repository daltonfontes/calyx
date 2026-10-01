"""W2: recovery after the process dies, with effects outside the run.

The same refund flow (get_order -> decide -> refund -> reply -> email)
against the same fake store, in Calyx, LangGraph, Temporal and plain
Python. The process is killed at every point of the crash matrix, then
resumed the way each system offers:

- after each step was recorded (`after-<step>`): the step finished and the
  run recorded it; the process dies before the next one starts. In Calyx,
  `CALYX_CRASH_AFTER=k`; in Python, the process exits when the next step
  starts (`BENCH_CRASH_BEFORE`);
- with an effect in flight (`<effect>-in-flight`): the store paid or sent,
  but its answer has not reached the caller when the process gets
  `kill -9`.

Counted in the store afterwards: payments and e-mails (one each is right),
and model calls made in total (two are needed). Results go to
bench/results/w2.json.

Temporal runs when TEMPORAL_BIN points to the `temporal` CLI; the harness
starts its dev server (state in a file, so it outlives the workers).

    python bench/run_w2.py
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
W2 = os.path.join(HERE, "w2_recovery")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
PY = os.environ.get("BENCH_PYTHON", sys.executable)
TEMPORAL_BIN = os.environ.get("TEMPORAL_BIN")
TEMPORAL_PORT = "7299"
WORK = os.path.join(W2, ".work")
STORE = os.path.join(WORK, "store.json")
LLM_LOG = os.path.join(WORK, "llm.log")

STEPS = ["get_order", "decide", "refund", "reply", "email"]
BASE = {"request": "R1", "message": "chegou quebrado"}

# name -> (how the process dies, arguments)
SCENARIOS: dict[str, dict] = {}
for k, step in enumerate(STEPS[:-1], start=1):
    SCENARIOS[f"after-{step}"] = {"after": k, **BASE}
SCENARIOS["refund-in-flight"] = {"in_flight": "payments", "request": "R1__slowpay__",
                                 "message": "chegou quebrado"}
SCENARIOS["email-in-flight"] = {"in_flight": "outbox", "request": "R1",
                                "message": "chegou quebrado __slowmail__"}


def store() -> dict:
    try:
        with open(STORE) as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def systems(req: str, msg: str, case: str) -> dict[str, dict]:
    calyx = [CALYX, "run", "refund.clyx", "--request", req, "--order", "A100", "--message", msg]
    args = [req, "A100", msg]

    def py(script: str, *flags: str, with_id: bool = True) -> dict:
        tag = "-".join(f.strip("-") for f in flags) or "plain"
        ident = [f"{case}-{script.split('_')[0]}-{tag}"] if with_id else []
        return {
            "run": [PY, script] + (["run"] if with_id else []) + ident + args + list(flags),
            "resume": [PY, script] + (["resume"] if with_id else []) + ident + args + list(flags),
        }

    out = {
        "calyx": {"run": calyx, "resume": None},
        "langgraph (padrão)": py("langgraph_.py"),
        "langgraph (padrão) + cuidado manual": py("langgraph_.py", "--careful"),
        "langgraph durability=sync": py("langgraph_.py", "--sync"),
        "langgraph durability=sync + cuidado manual": py("langgraph_.py", "--careful", "--sync"),
    }
    if TEMPORAL_BIN:
        out["temporal"] = py("temporal_.py")
        out["temporal + cuidado manual"] = py("temporal_.py", "--careful")
    out["python sem checkpoint"] = py("naive.py", with_id=False)
    out["python sem checkpoint + cuidado manual"] = py("naive.py", "--careful", with_id=False)
    return out


def env(extra: dict | None = None) -> dict:
    e = dict(os.environ)
    e.update(
        CALYX_FAKE_STORE=STORE,
        BENCH_LLM_LOG=LLM_LOG,
        BENCH_CHECKPOINT=os.path.join(WORK, "checkpoints.sqlite"),
        BENCH_LATENCY="1.0",
        TEMPORAL_ADDRESS=f"127.0.0.1:{TEMPORAL_PORT}",
    )
    e.update(extra or {})
    return e


def llm_calls(calyx_stderr: str, system: str) -> int:
    if system == "calyx":
        # One trace line per model call that answered.
        return sum(1 for line in calyx_stderr.splitlines() if " llm " in line and "tokens" in line)
    try:
        with open(LLM_LOG) as f:
            return len(f.read().splitlines())
    except FileNotFoundError:
        return 0


def one(system: str, spec: dict, scenario: dict) -> dict:
    shutil.rmtree(WORK, ignore_errors=True)
    shutil.rmtree(os.path.join(W2, ".calyx"), ignore_errors=True)
    os.makedirs(WORK)
    stderr_log = os.path.join(WORK, "run.err")

    # First run, killed at the scenario's point.
    extra = {}
    if "after" in scenario:
        k = scenario["after"]
        extra = (
            {"CALYX_CRASH_AFTER": str(k)}
            if system == "calyx"
            else {"BENCH_CRASH_BEFORE": STEPS[k]}
        )
    with open(stderr_log, "w") as err:
        p = subprocess.Popen(
            spec["run"], cwd=W2, env=env(extra), stdout=subprocess.DEVNULL, stderr=err,
            start_new_session=True,
        )
        if "after" in scenario:
            p.wait(timeout=60)
        else:
            what = scenario["in_flight"]
            deadline = time.time() + 60
            while not store().get(what) and time.time() < deadline:
                time.sleep(0.05)
            os.killpg(p.pid, signal.SIGKILL)  # the runtime and the tool server with it
            p.wait()
    first_err = open(stderr_log).read()

    # Resume.
    cmd = spec["resume"]
    if cmd is None:  # calyx: the run id is in its output
        run_id = next(
            line.split()[-1] for line in first_err.splitlines() if line.startswith("calyx: run ")
        )
        cmd = [CALYX, "resume", run_id]
    t0 = time.perf_counter()
    out = subprocess.run(cmd, cwd=W2, env=env(), capture_output=True, text=True, timeout=180)
    resume_s = time.perf_counter() - t0

    db = store()
    calls = llm_calls(first_err + out.stderr, system)
    payments, emails = len(db.get("payments", [])), len(db.get("outbox", []))
    return {
        "system": system,
        "resume_exit": out.returncode,
        "payments": payments,
        "emails": emails,
        "llm_calls": calls,
        "llm_calls_redone": calls - 2,
        "correct_effects": out.returncode == 0 and payments == 1 and emails == 1,
        "resume_s": round(resume_s, 2),
        "resume_tail": (out.stdout + out.stderr).strip().splitlines()[-1:] or [""],
    }


def start_temporal() -> subprocess.Popen | None:
    if not TEMPORAL_BIN:
        print("TEMPORAL_BIN not set: Temporal is left out", flush=True)
        return None
    db = os.path.join(W2, ".temporal.db")
    if os.path.exists(db):
        os.remove(db)
    server = subprocess.Popen(
        [TEMPORAL_BIN, "server", "start-dev", "--headless", "--db-filename", db,
         "--port", TEMPORAL_PORT, "--log-level", "error"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
    )
    for _ in range(100):
        ok = subprocess.run(
            [TEMPORAL_BIN, "operator", "namespace", "describe", "default",
             "--address", f"127.0.0.1:{TEMPORAL_PORT}"],
            capture_output=True,
        )
        if ok.returncode == 0:
            return server
        time.sleep(0.2)
    raise SystemExit("the Temporal dev server did not start")


def main() -> None:
    server = start_temporal()
    rows = []
    try:
        for name, scenario in SCENARIOS.items():
            case = f"{name}-{time.time_ns()}"
            for system, spec in systems(scenario["request"], scenario["message"], case).items():
                r = {"scenario": name, **one(system, spec, scenario)}
                rows.append(r)
                print(
                    f"{name:18} {system:44} pagamentos={r['payments']} e-mails={r['emails']} "
                    f"llm={r['llm_calls']} saída={r['resume_exit']} retomada={r['resume_s']}s",
                    flush=True,
                )
                with open(os.path.join(HERE, "results", "w2.json"), "w") as f:
                    json.dump(rows, f, indent=1, ensure_ascii=False)
    finally:
        if server:
            os.killpg(server.pid, signal.SIGKILL)
        shutil.rmtree(WORK, ignore_errors=True)
        shutil.rmtree(os.path.join(W2, ".calyx"), ignore_errors=True)
        for leftover in (".temporal.db", ".temporal.db-shm", ".temporal.db-wal"):
            if os.path.exists(os.path.join(W2, leftover)):
                os.remove(os.path.join(W2, leftover))


if __name__ == "__main__":
    main()
