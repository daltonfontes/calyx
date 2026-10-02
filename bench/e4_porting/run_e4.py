"""E4: where does each bug written by another person show up in Python?

For each program in programas/ (bNN_*.py): pyright and mypy, both in strict
mode, then the program itself. Prints a line per program and writes
bench/results/e4.json. Programs marked `# Não se aplica:` are listed as such.

    python bench/e4_porting/run_e4.py [bNN ...]

Use the Python of the virtualenv that has langgraph, pyright and mypy:
pyright and mypy are looked for next to it.
"""
import glob
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PROGRAMS = os.path.join(HERE, "programas")
RESULTS = os.path.join(os.path.dirname(HERE), "results", "e4.json")
PY = sys.executable
BIN = os.path.dirname(PY)


def env() -> dict:
    # The programs import world.py from the folder above them.
    return dict(os.environ, PYTHONPATH=HERE + os.pathsep + os.environ.get("PYTHONPATH", ""))


def pyright(path: str) -> list[str]:
    cfg = os.path.join(HERE, "pyrightconfig.json")
    out = subprocess.run(
        [os.path.join(BIN, "pyright"), "--outputjson", "--pythonpath", PY, "--project", cfg, path],
        cwd=HERE, capture_output=True, text=True, env=env(),
    )
    try:
        report = json.loads(out.stdout)
    except json.JSONDecodeError:
        return [f"pyright did not run: {out.stderr.strip()[:200]}"]
    return [
        f"{d['range']['start']['line'] + 1}: {d['message'].splitlines()[0]}"
        for d in report.get("generalDiagnostics", [])
        if d["severity"] == "error"
    ]


def mypy(path: str) -> list[str]:
    out = subprocess.run(
        [os.path.join(BIN, "mypy"), "--strict", "--no-error-summary", "--ignore-missing-imports", path],
        cwd=HERE, capture_output=True, text=True, env=env(),
    )
    return [
        line.split(":", 1)[1].strip()
        for line in out.stdout.splitlines()
        if ": error: " in line and os.path.basename(path) in line
    ]


def run(path: str) -> dict:
    try:
        out = subprocess.run([PY, path], cwd=HERE, capture_output=True, text=True, timeout=60,
                             env=env())
    except subprocess.TimeoutExpired:
        return {"exit": None, "exception": "timeout (60 s)", "stdout": ""}
    exc = ""
    for line in reversed(out.stderr.splitlines()):
        m = re.match(r"^([\w.]+(?:Error|Exception|Interrupt|Exit)\b.*)$", line)
        if m:
            exc = m.group(1)[:200]
            break
    return {"exit": out.returncode, "exception": exc, "stdout": out.stdout.strip()[-300:]}


def header(path: str, key: str) -> str:
    for line in open(path).read().splitlines()[:10]:
        if line.startswith(f"# {key}"):
            return line.split(":", 1)[1].strip()
    return ""


def main() -> None:
    only = sys.argv[1:]
    paths = sorted(glob.glob(os.path.join(PROGRAMS, "b[0-9][0-9]_*.py")))
    if only:
        paths = [p for p in paths if any(os.path.basename(p).startswith(o) for o in only)]
    else:  # b00 is the example of the format, not one of the 54
        paths = [p for p in paths if not os.path.basename(p).startswith("b00_")]
    if not paths:
        print("nenhum programa em programas/ (bNN_nome.py)")
        return
    rows = []
    for path in paths:
        name = os.path.basename(path)
        row: dict = {"program": name, "bug": header(path, "Bug"), "damage_at": header(path, "Dano")}
        skip = header(path, "Não se aplica")
        if skip:
            row["not_applicable"] = skip
            print(f"{name:42} não se aplica: {skip}", flush=True)
        else:
            row.update(pyright=pyright(path), mypy=mypy(path), run=run(path))
            print(
                f"{name:42} pyright={len(row['pyright'])} mypy={len(row['mypy'])} "
                f"saída={row['run']['exit']} {row['run']['exception'][:60]!r}",
                flush=True,
            )
            for e in row["pyright"][:3]:
                print(f"    pyright: {e}")
            for e in row["mypy"][:3]:
                print(f"    mypy:    {e}")
        rows.append(row)
    if not only:
        with open(RESULTS, "w") as f:
            json.dump(rows, f, indent=1, ensure_ascii=False)
        print(f"\n{len(rows)} programa(s); resultados em {os.path.relpath(RESULTS)}")


if __name__ == "__main__":
    main()
