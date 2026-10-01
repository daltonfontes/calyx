"""W2: recovery after the process dies, with effects outside the run.

The same refund flow (get_order -> decide -> refund -> reply -> email)
against the same fake store, in Calyx and in Python. The process is killed
at three points, then resumed the way each system offers:

- `after-payment`: the payment finished and the run recorded it; the
  process dies before the next step (`save_result` in the question).
- `payment-in-flight`: the store paid, but its answer has not reached the
  caller when the process is killed (`kill -9`).
- `email-in-flight`: the same, for the e-mail.

Counted in the store afterwards: payments and e-mails (one each is right),
and model calls made in total (two are needed). Results go to
bench/results/w2.json.

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
WORK = os.path.join(W2, ".work")
STORE = os.path.join(WORK, "store.json")
LLM_LOG = os.path.join(WORK, "llm.log")

SCENARIOS = {
    "after-payment": {"request": "R1", "message": "chegou quebrado"},
    "payment-in-flight": {"request": "R1__slowpay__", "message": "chegou quebrado"},
    "email-in-flight": {"request": "R1", "message": "chegou quebrado __slowmail__"},
}


def store() -> dict:
    try:
        with open(STORE) as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def systems(req: str, msg: str) -> dict[str, dict]:
    calyx = [CALYX, "run", "refund.clyx", "--request", req, "--order", "A100", "--message", msg]
    lg = [PY, "langgraph_.py"]
    naive = [PY, "naive.py", req, "A100", msg]
    return {
        "calyx": {"run": calyx, "resume": None, "crash_env": {"CALYX_CRASH_AFTER": "3"}},
        "langgraph (padrão)": {
            "run": lg + ["run", "t1", req, "A100", msg],
            "resume": lg + ["resume", "t1", req, "A100", msg],
            "crash_env": {"BENCH_CRASH_BEFORE": "reply"},
        },
        "langgraph (padrão) + cuidado manual": {
            "run": lg + ["run", "t1", req, "A100", msg, "--careful"],
            "resume": lg + ["resume", "t1", req, "A100", msg, "--careful"],
            "crash_env": {"BENCH_CRASH_BEFORE": "reply"},
        },
        "langgraph durability=sync": {
            "run": lg + ["run", "t1", req, "A100", msg, "--sync"],
            "resume": lg + ["resume", "t1", req, "A100", msg, "--sync"],
            "crash_env": {"BENCH_CRASH_BEFORE": "reply"},
        },
        "langgraph durability=sync + cuidado manual": {
            "run": lg + ["run", "t1", req, "A100", msg, "--careful", "--sync"],
            "resume": lg + ["resume", "t1", req, "A100", msg, "--careful", "--sync"],
            "crash_env": {"BENCH_CRASH_BEFORE": "reply"},
        },
        "python sem checkpoint": {
            "run": naive,
            "resume": naive,
            "crash_env": {"BENCH_CRASH_BEFORE": "reply"},
        },
        "python sem checkpoint + cuidado manual": {
            "run": naive + ["--careful"],
            "resume": naive + ["--careful"],
            "crash_env": {"BENCH_CRASH_BEFORE": "reply"},
        },
    }


def env(extra: dict | None = None) -> dict:
    e = dict(os.environ)
    e.update(
        CALYX_FAKE_STORE=STORE,
        BENCH_LLM_LOG=LLM_LOG,
        BENCH_CHECKPOINT=os.path.join(WORK, "checkpoints.sqlite"),
        BENCH_LATENCY="1.0",
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


def one(system: str, spec: dict, scenario: str) -> dict:
    shutil.rmtree(WORK, ignore_errors=True)
    shutil.rmtree(os.path.join(W2, ".calyx"), ignore_errors=True)
    os.makedirs(WORK)
    stderr_log = os.path.join(WORK, "run.err")

    # First run, killed at the scenario's point.
    extra = spec["crash_env"] if scenario == "after-payment" else {}
    with open(stderr_log, "w") as err:
        p = subprocess.Popen(
            spec["run"], cwd=W2, env=env(extra), stdout=subprocess.DEVNULL, stderr=err,
            start_new_session=True,
        )
        if scenario == "after-payment":
            p.wait(timeout=60)
        else:
            what = "payments" if scenario == "payment-in-flight" else "outbox"
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
    out = subprocess.run(cmd, cwd=W2, env=env(), capture_output=True, text=True, timeout=120)
    resume_s = time.perf_counter() - t0

    db = store()
    calls = llm_calls(first_err + out.stderr, system)
    return {
        "system": system,
        "scenario": scenario,
        "resume_exit": out.returncode,
        "payments": len(db.get("payments", [])),
        "emails": len(db.get("outbox", [])),
        "llm_calls": calls,
        "llm_calls_redone": calls - 2,
        "resume_s": round(resume_s, 2),
        "resume_tail": (out.stdout + out.stderr).strip().splitlines()[-1:] or [""],
    }


def main() -> None:
    rows = []
    for scenario, args in SCENARIOS.items():
        for system, spec in systems(args["request"], args["message"]).items():
            r = one(system, spec, scenario)
            rows.append(r)
            print(
                f"{scenario:18} {system:44} pagamentos={r['payments']} e-mails={r['emails']} "
                f"llm={r['llm_calls']} saída={r['resume_exit']}",
                flush=True,
            )
    shutil.rmtree(WORK, ignore_errors=True)
    shutil.rmtree(os.path.join(W2, ".calyx"), ignore_errors=True)
    with open(os.path.join(HERE, "results", "w2.json"), "w") as f:
        json.dump(rows, f, indent=1, ensure_ascii=False)


if __name__ == "__main__":
    main()
