"""W1 with a real model: the same fan-out as run_w1.py, with Gemini
(gemini-3.5-flash-lite) instead of the fixed-latency fake, in Calyx and in
the three Python baselines.

The free tier limits requests per minute, so N is small (5 and 10) and the
runs are spaced (BENCH_PAUSE seconds, default 65) for the limit's window to
clear; the order of the systems rotates every repetition. Every system
retries the same way (4 attempts, 1-2-4 s, or what the provider asks).

    GEMINI_API_KEY=... python bench/run_w1_real.py [--reps 3] [--sizes 5 10]

Results go to bench/results/w1_real.json.
"""
import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
W1 = os.path.join(HERE, "w1_fanout")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
PY = os.environ.get("BENCH_PYTHON", sys.executable)
MODEL = "gemini-3.5-flash-lite"
TOPICS = [
    "energia solar em telhados residenciais", "baterias de sódio", "hidrogênio verde",
    "energia eólica offshore", "biogás em fazendas", "redes elétricas inteligentes",
    "pequenas centrais hidrelétricas", "carros elétricos no transporte público",
    "armazenamento térmico de energia", "eficiência energética em prédios",
]


def program() -> str:
    src = open(os.path.join(W1, "research.clyx")).read()
    path = os.path.join(W1, "research_real.clyx")
    with open(path, "w") as f:
        f.write(src.replace('"fake-slow-1000"', f'"{MODEL}"'))
    return path


def commands(n: int) -> dict[str, list[str]]:
    qs = json.dumps(TOPICS[:n], ensure_ascii=False)
    return {
        "calyx": [CALYX, "run", program(), "--no-journal", "--questions", qs],
        "python sequencial": [PY, os.path.join(W1, "seq.py"), qs],
        "python asyncio": [PY, os.path.join(W1, "async_.py"), qs],
        "langgraph": [PY, os.path.join(W1, "langgraph_.py"), qs],
    }


def run(cmd: list[str]) -> dict:
    env = dict(os.environ, BENCH_MODEL=MODEL)
    t0 = time.perf_counter()
    out = subprocess.run(cmd, cwd=W1, env=env, capture_output=True, text=True)
    dt = time.perf_counter() - t0
    if out.returncode != 0:
        return {"ok": False, "s": round(dt, 2), "error": out.stderr[-500:]}
    calls = retries = None
    m = re.search(r"llm_calls=(\d+) llm_retries=(\d+)", out.stderr)
    if m:
        calls, retries = int(m.group(1)), int(m.group(2))
    m = re.search(r"finished: (\d+) model call\(s\).*?(\d+) retry\(ies\)", out.stderr)
    if m:
        calls, retries = int(m.group(1)), int(m.group(2))
    return {"ok": bool(out.stdout.strip()), "s": round(dt, 2), "calls": calls, "retries": retries,
            "report_chars": len(out.stdout)}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--sizes", type=int, nargs="*", default=[5, 10])
    a = ap.parse_args()
    if "GEMINI_API_KEY" not in os.environ:
        raise SystemExit("GEMINI_API_KEY is not set")
    pause = float(os.environ.get("BENCH_PAUSE", "65"))
    path = os.path.join(HERE, "results", "w1_real.json")
    raw = []
    for rep in range(a.reps):
        for n in a.sizes:
            cmds = list(commands(n).items())
            k = rep % len(cmds)
            for system, cmd in cmds[k:] + cmds[:k]:
                r = run(cmd)
                r.update(system=system, n=n, rep=rep)
                raw.append(r)
                print(f"rep {rep} n={n:<3} {system:18} {r['s']:7.2f} s  calls={r.get('calls')} "
                      f"retries={r.get('retries')} ok={r['ok']}", flush=True)
                with open(path, "w") as f:
                    json.dump({"model": MODEL, "pause_s": pause, "raw": raw}, f, indent=1, ensure_ascii=False)
                time.sleep(pause)
    summary = []
    for n in a.sizes:
        for system in commands(n):
            rs = [r for r in raw if r["n"] == n and r["system"] == system and r["ok"]]
            if rs:
                ts = [r["s"] for r in rs]
                summary.append({"system": system, "n": n, "median_s": statistics.median(ts),
                                "min_s": min(ts), "max_s": max(ts), "ok": len(rs),
                                "runs": sum(1 for r in raw if r["n"] == n and r["system"] == system),
                                "retries": sum(r.get("retries") or 0 for r in rs)})
    for s in summary:
        print(f"n={s['n']:<3} {s['system']:18} median {s['median_s']:7.2f} s  "
              f"[{s['min_s']:.2f}, {s['max_s']:.2f}]  ok {s['ok']}/{s['runs']}  retries {s['retries']}")
    with open(path, "w") as f:
        json.dump({"model": MODEL, "pause_s": pause, "raw": raw, "summary": summary}, f, indent=1,
                  ensure_ascii=False)


if __name__ == "__main__":
    main()
