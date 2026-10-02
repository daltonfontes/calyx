"""W7: one user's memory, written by many runs at the same time.

The same chat turn (recall the user's memory -> answer (model) -> extract
three facts (model) -> add them to the memory and count the conversation)
in Calyx (an entity) and LangGraph (its Store, on SQLite). Every run is its
own process, as when several requests from the same user arrive together:

- `N juntas`: N runs start at once;
- `queda depois de gravar`: the process dies right after the memory was
  written, before the run recorded it (Calyx: CALYX_CRASH_IN_SEND; LangGraph:
  before the node's checkpoint), then the run is resumed;
- both: N runs at once, all die there, all are resumed at once.

Right is N conversations and 3N facts, none twice. Results go to
bench/results/w7.json.

    python bench/run_w7.py
"""
import json
import os
import shutil
import subprocess
import sys
import time
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
W7 = os.path.join(HERE, "w7_memory")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
PY = os.environ.get("BENCH_PYTHON", sys.executable)
WORK = os.path.join(W7, ".work")
USER = "ana"

SCENARIOS = {
    "20 execuções juntas": (20, False),
    "queda depois de gravar, e retomada": (1, True),
    "10 juntas, todas caem e são retomadas": (10, True),
}


def env(crash: bool) -> dict:
    e = dict(os.environ)
    e.update(BENCH_LATENCY="0.3", BENCH_STORE=os.path.join(WORK, "store.sqlite"),
             BENCH_CHECKPOINT=os.path.join(WORK, "checkpoints.sqlite"))
    if crash:
        e.update(CALYX_CRASH_IN_SEND="1", BENCH_CRASH_IN_SEND="1")
    return e


def together(cmds: list[list[str]], crash: bool) -> list[subprocess.CompletedProcess]:
    procs = [subprocess.Popen(c, cwd=W7, env=env(crash), stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True) for c in cmds]
    outs = []
    for p in procs:
        out, err = p.communicate(timeout=120)
        outs.append(subprocess.CompletedProcess(p.args, p.returncode, out, err))
    return outs


class Calyx:
    def run(self, n: int, crash: bool) -> int:
        outs = together([[CALYX, "run", "memory.clyx", "--fake-models", "--quiet",
                          "--user", USER, "--text", f"mensagem {i}"] for i in range(n)], crash)
        failed = sum(o.returncode != 0 for o in outs)
        if crash:
            ids = [next(l.split()[-1] for l in o.stderr.splitlines() if l.startswith("calyx: run "))
                   for o in outs]
            outs = together([[CALYX, "resume", i, "--fake-models", "--quiet"] for i in ids], False)
            failed = sum(o.returncode != 0 for o in outs)
        return failed

    def memory(self) -> dict:
        d = os.path.join(W7, ".calyx", "entities", "UserMemory")
        for k in os.listdir(d):
            with open(os.path.join(d, k, "entity.json")) as f:
                return json.load(f)["state"]
        return {"facts": [], "conversations": 0}


class LangGraph:
    def __init__(self, careful: bool):
        self.flags = ["--careful"] if careful else []

    def run(self, n: int, crash: bool) -> int:
        subprocess.run([PY, "langgraph_.py", "setup", "-", "-", "-"], cwd=W7, env=env(False),
                       check=True, capture_output=True, timeout=60)
        cmd = lambda mode, i: [PY, "langgraph_.py", mode, f"m{i}", USER, f"mensagem {i}", *self.flags]
        outs = together([cmd("run", i) for i in range(n)], crash)
        if crash:
            outs = together([cmd("resume", i) for i in range(n)], False)
        return sum(o.returncode != 0 for o in outs)

    def memory(self) -> dict:
        out = subprocess.run([PY, "langgraph_.py", "dump", "-", USER, "-", *self.flags], cwd=W7,
                             env=env(False), capture_output=True, text=True, timeout=60)
        return json.loads(out.stdout)


SYSTEMS = {
    "calyx": Calyx,
    "langgraph (Store)": lambda: LangGraph(False),
    "langgraph (Store) + cuidado manual": lambda: LangGraph(True),
}


def main() -> None:
    rows = []
    try:
        for name, (n, crash) in SCENARIOS.items():
            for system, make in SYSTEMS.items():
                shutil.rmtree(WORK, ignore_errors=True)
                shutil.rmtree(os.path.join(W7, ".calyx"), ignore_errors=True)
                os.makedirs(WORK)
                s = make()
                t0 = time.perf_counter()
                failed = s.run(n, crash)
                secs = time.perf_counter() - t0
                mem = s.memory()
                facts = [f["content"] for f in mem["facts"]]
                dup = sum(c - 1 for c in Counter(facts).values())
                r = {"scenario": name, "system": system, "runs": n, "failed_runs": failed,
                     "conversations": mem["conversations"], "facts": len(facts),
                     "facts_expected": 3 * n, "facts_lost": max(0, 3 * n - len(set(facts))),
                     "facts_twice": dup, "seconds": round(secs, 2)}
                r["correct"] = (failed == 0 and r["conversations"] == n and len(facts) == 3 * n
                                and dup == 0)
                rows.append(r)
                print(f"{name:40} {system:36} {'ok ' if r['correct'] else 'ERRO'} "
                      f"conversas={r['conversations']}/{n} fatos={len(facts)}/{3 * n} "
                      f"perdidos={r['facts_lost']} repetidos={dup} falhas={failed} {r['seconds']}s",
                      flush=True)
                with open(os.path.join(HERE, "results", "w7.json"), "w") as f:
                    json.dump(rows, f, indent=1, ensure_ascii=False)
    finally:
        shutil.rmtree(WORK, ignore_errors=True)
        shutil.rmtree(os.path.join(W7, ".calyx"), ignore_errors=True)


if __name__ == "__main__":
    main()
