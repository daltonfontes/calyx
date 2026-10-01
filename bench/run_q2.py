"""Q2 against Python: where does each bug of bench/q2_bugs/ show up?

For each program: pyright and mypy (before running), then the program
itself (does LangGraph or Python stop it, and does the damage happen?).
Results go to bench/results/q2.json.

    python bench/run_q2.py
"""
import glob
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
Q2 = os.path.join(HERE, "q2_bugs")
PY = os.environ.get("BENCH_PYTHON", sys.executable)
BIN = os.path.dirname(PY)


def pyright(path: str) -> list[str]:
    out = subprocess.run(
        [os.path.join(BIN, "pyright"), "--outputjson", "--pythonpath", PY, path],
        cwd=Q2, capture_output=True, text=True,
    )
    report = json.loads(out.stdout)
    return [
        f"{d['range']['start']['line'] + 1}: {d['message'].splitlines()[0]}"
        for d in report.get("generalDiagnostics", [])
        if d["severity"] == "error"
    ]


def mypy(path: str) -> list[str]:
    out = subprocess.run(
        [os.path.join(BIN, "mypy"), "--no-error-summary", "--ignore-missing-imports", path],
        cwd=Q2, capture_output=True, text=True,
    )
    return [line.split(": error: ", 1)[1] for line in out.stdout.splitlines() if ": error: " in line]


def run(path: str) -> dict:
    try:
        out = subprocess.run([PY, path], cwd=Q2, capture_output=True, text=True, timeout=30)
    except subprocess.TimeoutExpired:
        return {"exit": None, "exception": "timeout (30 s)", "stdout": ""}
    exc = ""
    for line in reversed(out.stderr.splitlines()):
        m = re.match(r"^([\w.]+(?:Error|Exception|Interrupt)\b.*)$", line)
        if m:
            exc = m.group(1)[:200]
            break
    return {"exit": out.returncode, "exception": exc, "stdout": out.stdout.strip()[-300:]}


def main() -> None:
    rows = []
    for path in sorted(glob.glob(os.path.join(Q2, "b*.py"))):
        name = os.path.basename(path)
        header = open(path).read().splitlines()
        calyx = next(line.split(": ", 1)[1] for line in header if line.startswith("# Calyx:"))
        bug = header[0].split(": ", 1)[1]
        row = {
            "program": name,
            "bug": bug,
            "calyx": calyx,
            "pyright": pyright(name),
            "mypy": mypy(name),
            "run": run(name),
        }
        rows.append(row)
        print(
            f"{name:38} pyright={len(row['pyright'])} mypy={len(row['mypy'])} "
            f"exit={row['run']['exit']} {row['run']['exception'][:60]!r}",
            flush=True,
        )
    with open(os.path.join(HERE, "results", "q2.json"), "w") as f:
        json.dump(rows, f, indent=1, ensure_ascii=False)


if __name__ == "__main__":
    main()
