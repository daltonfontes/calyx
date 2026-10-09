"""W1: the same fan-out (N questions -> search + summary -> report) in
Calyx and in three Python baselines. Two experiments:

- latency: every model call takes 1 s; N = 5, 20, 50; at most 8 calls at once.
  What each runtime gets from the same graph.
- scale: model calls take 0 s; N = 10 ... 10000. What each runtime costs per
  item when the model is not the bottleneck (scheduler, journal, startup).

Times are wall time of the whole process (startup included), median of the
repetitions. Results go to bench/results/w1.json.

    python bench/run_w1.py [--quick]
"""
import json
import os
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
W1 = os.path.join(HERE, "w1_fanout")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
PY = os.environ.get("BENCH_PYTHON", sys.executable)


def program(latency_ms: int) -> str:
    """research.clyx with the model's latency (fake-slow-<ms>, or 0)."""
    src = open(os.path.join(W1, "research.clyx")).read()
    model = f"fake-slow-{latency_ms}" if latency_ms else "fake-model"
    path = os.path.join(W1, f"research_{latency_ms}.clyx")
    with open(path, "w") as f:
        f.write(src.replace('"fake-slow-1000"', f'"{model}"'))
    return path


def commands(n: int, latency_ms: int) -> dict[str, list[str]]:
    # Short questions: with 10000 of them the argument must stay under 128 KB.
    qs = json.dumps([f"q{i}" for i in range(n)])
    clyx = program(latency_ms)
    calyx = [CALYX, "run", clyx, "--quiet", "--questions", qs]
    return {
        "calyx": calyx,
        "calyx --no-journal": calyx + ["--no-journal"],
        "calyx --deterministic": calyx + ["--deterministic"],
        "python sequencial": [PY, os.path.join(W1, "seq.py"), qs],
        "python asyncio": [PY, os.path.join(W1, "async_.py"), qs],
        "langgraph": [PY, os.path.join(W1, "langgraph_.py"), qs],
    }


def run(cmd: list[str], latency_ms: int) -> tuple[float, int]:
    env = dict(os.environ, BENCH_LATENCY=str(latency_ms / 1000))
    t0 = time.perf_counter()
    out = subprocess.run(cmd, cwd=W1, env=env, capture_output=True, text=True)
    dt = time.perf_counter() - t0
    if out.returncode != 0:
        raise SystemExit(f"{cmd[:3]} failed:\n{out.stderr[-2000:]}")
    calls = 0
    for line in out.stderr.splitlines():
        if line.startswith("llm_calls="):
            calls = int(line.split()[0].split("=")[1])
    return dt, calls


RESULTS = os.path.join(HERE, "results", "w1.json")
DONE: list[dict] = []


def save(rows: list[dict]) -> None:
    with open(RESULTS, "w") as f:
        json.dump(DONE + rows, f, indent=1, ensure_ascii=False)


def experiment(name: str, latency_ms: int, sizes: list[int], reps: int, skip) -> list[dict]:
    rows = []
    for n in sizes:
        for system, cmd in commands(n, latency_ms).items():
            if skip(system, n):
                continue
            times = [run(cmd, latency_ms)[0] for _ in range(reps)]
            row = {
                "experiment": name,
                "system": system,
                "n": n,
                "latency_ms": latency_ms,
                "median_s": round(statistics.median(times), 3),
                "min_s": round(min(times), 3),
                "max_s": round(max(times), 3),
                "reps": reps,
            }
            rows.append(row)
            save(rows)
            print(f"{name:8} n={n:<6} {system:24} {row['median_s']:8.3f} s", flush=True)
    return rows


def main() -> None:
    quick = "--quick" in sys.argv
    subprocess.run(["make", "-s", "-C", ROOT, "build"], check=False)
    slow = lambda s, n: s in ("python sequencial", "calyx --deterministic") and n > 20  # noqa: E731
    DONE.extend(
        experiment("latency", 1000, [5, 20] if quick else [5, 20, 50], 1 if quick else 3, slow)
    )
    big = lambda s, n: s in ("python sequencial", "calyx --deterministic")  # noqa: E731
    DONE.extend(
        experiment(
            "scale", 0, [10, 100, 1000] if quick else [10, 100, 1000, 10000], 1 if quick else 3, big
        )
    )
    save([])


if __name__ == "__main__":
    main()
