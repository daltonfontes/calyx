"""E2: the cost of the runtime, with models that answer at once.

- fan-out: W1 (N questions -> search + summary -> report) from 1,000 to
  100,000 items, in Calyx (with and without the journal) and the Python
  baselines. Questions go in a file (`--questions @file`): with 100,000 of
  them they do not fit in a command-line argument.
- agent: one agent whose model calls a tool every turn (`fake-busy`), from
  50 to 800 turns. Each turn sends the whole conversation, so some growth
  with the square of the turns is the protocol's; the runtime must not add
  more than that.

Wall time of the whole process (startup included), median of the
repetitions. Results go to bench/results/e2.json.

    python bench/run_e2.py
"""
import json
import os
import shutil
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
W1 = os.path.join(HERE, "w1_fanout")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
PY = os.environ.get("BENCH_PYTHON", sys.executable)
WORK = os.path.join(W1, ".work")

AGENT = """model busy = "fake-busy"

tool web_search(query: Text) -> Text:
    effect read
    max_output 4000 tokens

prompt investigate(question: Text) -> Text:
    \"\"\"Responda: {question}\"\"\"

graph ask(question: Text) -> Text:
    answer = agent busy:
        tools [web_search]
        max_turns TURNS
        task investigate(question)
        on turn_limit: final_answer
        on stuck: fail "stuck"
    return answer
"""


def timed(cmd: list[str]) -> float:
    shutil.rmtree(os.path.join(W1, ".calyx"), ignore_errors=True)
    t0 = time.perf_counter()
    out = subprocess.run(cmd, cwd=W1, env=dict(os.environ, BENCH_LATENCY="0"),
                         capture_output=True, text=True, timeout=1800)
    dt = time.perf_counter() - t0
    if out.returncode != 0:
        raise SystemExit(f"{cmd[:3]} failed:\n{out.stderr[-2000:]}")
    return dt


def row(experiment: str, system: str, n: int, times: list[float]) -> dict:
    med = statistics.median(times)
    r = {"experiment": experiment, "system": system, "n": n, "median_s": round(med, 3),
         "min_s": round(min(times), 3), "max_s": round(max(times), 3), "reps": len(times),
         "ms_per_item": round(1000 * med / n, 4)}
    print(f"{experiment:7} n={n:<7} {system:22} {r['median_s']:9.3f} s  "
          f"{r['ms_per_item']:8.4f} ms/item", flush=True)
    return r


def main() -> None:
    os.makedirs(WORK, exist_ok=True)
    rows: list[dict] = []
    save = lambda: json.dump(rows, open(os.path.join(HERE, "results", "e2.json"), "w"),  # noqa: E731
                             indent=1, ensure_ascii=False)
    try:
        clyx = os.path.join(W1, "research_0.clyx")
        src = open(os.path.join(W1, "research.clyx")).read()
        open(clyx, "w").write(src.replace('"fake-slow-1000"', '"fake-model"'))
        for n in [1000, 10000, 30000, 100000]:
            qs = os.path.join(WORK, f"q{n}.json")
            json.dump([f"q{i}" for i in range(n)], open(qs, "w"))
            systems = {
                "calyx": [CALYX, "run", clyx, "--quiet", "--questions", "@" + qs],
                "calyx --no-journal": [CALYX, "run", clyx, "--quiet", "--no-journal",
                                       "--questions", "@" + qs],
                "python asyncio": [PY, os.path.join(W1, "async_.py"), "@" + qs],
            }
            if n <= 10000:  # 8 ms per item and growing: 30,000 would take most of an hour
                systems["langgraph"] = [PY, os.path.join(W1, "langgraph_.py"), "@" + qs]
            for system, cmd in systems.items():
                reps = 1 if n >= 100000 or (system == "langgraph" and n >= 10000) else 3
                rows.append(row("fan-out", system, n, [timed(cmd) for _ in range(reps)]))
                save()
        for turns in [50, 100, 200, 400, 800]:
            path = os.path.join(W1, "_agent.clyx")
            open(path, "w").write(AGENT.replace("TURNS", str(turns)))
            cmd = [CALYX, "run", path, "--quiet", "--question", "x"]
            rows.append(row("agent", "calyx", turns, [timed(cmd) for _ in range(3)]))
            save()
    finally:
        shutil.rmtree(WORK, ignore_errors=True)
        shutil.rmtree(os.path.join(W1, ".calyx"), ignore_errors=True)
        for f in ("_agent.clyx",):
            if os.path.exists(os.path.join(W1, f)):
                os.remove(os.path.join(W1, f))


if __name__ == "__main__":
    main()
