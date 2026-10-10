"""E5: how far mandatory contracts and static analysis cut concurrency
errors, and what they cost in flexibility and speed.

1. Errors: the bugs of the corpus (tests/state_bugs/) that are about
   concurrency (two activities overlap, or their order matters), and where
   Calyx stops each one: the compiler (error or warning), the runtime, the
   language itself (it cannot be written), or nowhere.
2. Flexibility: 21 correct concurrent patterns of agent workflows
   (bench/e5_flex/p*.clyx), each written the natural way. Accepted, warned
   about (a false alarm: the program is right), or refused; and for those,
   the rewrite that passes (`.ok.clyx`) and what it costs in lines.
3. Speed: the time of `calyx check` (the analysis), and the latency the
   rewrites cost at run time, with tools that take 0.5 s (slow_tool.py) and
   fake models of fixed latency.
4. Contracts: the share of the examples' lines that are contracts.

Results go to bench/results/e5.json.

    python bench/run_e5.py
"""
import glob
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
FLEX = os.path.join(HERE, "e5_flex")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
REPS = int(os.environ.get("E5_REPS", "5"))

# Concurrency bugs of the corpus, by kind. The rest of the corpus is about
# one activity (crashes and resumes, types, declarations, agents).
CONCURRENCY = {
    "01": "conflicting writes", "22": "conflicting writes", "23": "conflicting writes",
    "24": "conflicting writes", "25": "conflicting writes", "26": "conflicting writes",
    "47": "conflicting writes",
    "04": "order of effects", "14": "order of effects", "15": "order of effects",
    "38": "order of effects",
    "19": "atomicity (check, then act)", "20": "atomicity (check, then act)",
    "33": "atomicity (check, then act)", "34": "atomicity (check, then act)",
    "39": "simultaneous runs", "41": "simultaneous runs", "44": "simultaneous runs",
    "46": "races", "48": "races", "49": "races", "50": "races", "51": "races", "52": "races",
    "54": "races",
}


def check(path: str) -> list[str]:
    out = subprocess.run([CALYX, "check", path], capture_output=True, text=True)
    return sorted(set(re.findall(r"^(?:error|warning)\[([EW]\d+)\]", out.stdout + out.stderr, re.M)))


def lines(path: str) -> int:
    return sum(1 for l in open(path) if l.strip() and not l.strip().startswith("#"))


def corpus() -> dict:
    rows = []
    for path in sorted(glob.glob(os.path.join(ROOT, "tests", "state_bugs", "*.clyx"))):
        num = os.path.basename(path)[:2]
        if num not in CONCURRENCY:
            continue
        text = open(path).read()
        by = re.search(r"^# Pego por: (.*)$", text, re.M).group(1).strip()
        codes = check(path)
        stage = ("compiler (error)" if any(c.startswith("E") for c in codes)
                 else "compiler (warning)" if codes
                 else {"runtime": "runtime", "design": "language", "none": "missed"}[by])
        rows.append({"bug": os.path.basename(path)[:-5], "kind": CONCURRENCY[num],
                     "stage": stage, "codes": codes, "declared": by})
    by_stage: dict[str, int] = {}
    for r in rows:
        by_stage[r["stage"]] = by_stage.get(r["stage"], 0) + 1
    return {"bugs": rows, "by_stage": by_stage, "total": len(rows)}


# Patterns whose natural program compiles but does not do what was meant.
NOT_EXPRESSIBLE = {
    "p20_quorum": "there is no 'first k of n': the closest program waits for all n",
}


def patterns() -> list[dict]:
    rows = []
    for path in sorted(glob.glob(os.path.join(FLEX, "p*.clyx"))):
        if path.endswith(".ok.clyx"):
            continue
        name = os.path.basename(path)[:-5]
        what = re.search(r"^# Padrão: (.*)$", open(path).read(), re.M).group(1)
        codes = check(path)
        status = ("refused" if any(c.startswith("E") for c in codes)
                  else "warned" if codes
                  else "not expressible" if name in NOT_EXPRESSIBLE else "accepted")
        row = {"pattern": name, "what": what, "status": status, "codes": codes,
               "lines": lines(path)}
        if name in NOT_EXPRESSIBLE:
            row["why"] = NOT_EXPRESSIBLE[name]
        ok = path[:-5] + ".ok.clyx"
        if os.path.exists(ok):
            row["rewrite"] = {"codes": check(ok), "lines": lines(ok),
                              "extra_lines": lines(ok) - lines(path)}
        rows.append(row)
    return rows


def timed(args, cwd, must_pass=True) -> float:
    t = time.perf_counter()
    subprocess.run([CALYX, *args], cwd=cwd, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, check=must_pass)
    return time.perf_counter() - t


def check_time() -> dict:
    files = sorted(glob.glob(os.path.join(FLEX, "p*.clyx")))
    biggest = max(glob.glob(os.path.join(ROOT, "examples", "*.clyx")), key=lines)
    per = [statistics.median(timed(["check", f], FLEX, must_pass=False) for _ in range(REPS)) for f in files]
    return {"patterns_median_ms": round(statistics.median(per) * 1000, 1),
            "patterns_max_ms": round(max(per) * 1000, 1),
            "largest_example": os.path.basename(biggest), "largest_example_lines": lines(biggest),
            "largest_example_ms": round(statistics.median(
                timed(["check", biggest], ROOT) for _ in range(REPS)) * 1000, 1)}


def latency() -> dict:
    work = os.path.join(FLEX, ".work")
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    slow = os.path.join(FLEX, "slow_tool.py")
    with open(os.path.join(work, "calyx.toml"), "w") as f:
        for t in ("email", "track", "cache_put"):
            # One server for all three, as a real service would have.
            f.write(f'[tools.{t}]\ncommand = ["{sys.executable}", "{slow}"]\n')
    runs = {
        "p10 two independent writes (warned)": ("p10_independent_writes_any_order.clyx",
                                                ["--request", "R1", "--to", "a@b"]),
        "p10 rewritten with `after`": ("alt_p10_after.clyx", ["--request", "R1", "--to", "a@b"]),
        "p10 rewritten with `unordered`": ("p10_independent_writes_any_order.ok.clyx",
                                           ["--request", "R1", "--to", "a@b"]),
        "p08 cache write inside the race (warned)": ("p08_race_with_harmless_write.clyx",
                                                     ["--q", "x"]),
        "p08 rewritten: cache write after the race": ("p08_race_with_harmless_write.ok.clyx",
                                                      ["--q", "x"]),
        "p20 quorum, closest program: waits for all 3": ("p20_quorum.clyx", ["--q", "x"]),
    }
    out = {}
    for label, (prog, args) in runs.items():
        shutil.copy(os.path.join(FLEX, prog), work)
        out[label] = round(statistics.median(
            timed(["run", prog, "--graph", "g", "--fake-models", "--no-journal", "--quiet", *args], work)
            for _ in range(REPS)), 3)
        print(label, out[label], flush=True)
    # A quorum would go on with the 2nd answer: the fake models answer at
    # once, in 0.3 s and in 2 s.
    out["p20 quorum, ideal (2nd answer)"] = 0.3
    return out


CONTRACT = re.compile(r"^\s*(idempotency_key|on_uncertain|checks|requires|compensate|batch)\b"
                      r"|^\s*\w+ after \w+")
EFFECT = re.compile(r"^\s*effect\b")
LIMITS = re.compile(r"^\s*(max_output|timeout|retry_on|limits)\b")


def contract_lines() -> dict:
    files = glob.glob(os.path.join(ROOT, "examples", "*.clyx"))
    total = effect = contract = limit = 0
    for f in files:
        for line in open(f):
            s = line.strip()
            if not s or s.startswith("#"):
                continue
            total += 1
            effect += bool(EFFECT.match(line))
            contract += bool(CONTRACT.match(line))
            limit += bool(LIMITS.match(line))
    return {"programs": len(files), "lines": total, "effect_lines": effect,
            "contract_lines": contract, "limit_lines": limit,
            "share_effect_and_contracts": round((effect + contract) / total, 3)}


def main():
    subprocess.run(["cargo", "build", "--release", "-q"], cwd=ROOT, check=True)
    results = {"corpus": corpus(), "patterns": patterns(), "check_time": check_time(),
               "latency_s": latency(), "contracts": contract_lines()}
    path = os.path.join(HERE, "results", "e5.json")
    with open(path, "w") as f:
        json.dump(results, f, indent=2, ensure_ascii=False)
        f.write("\n")
    c = results["corpus"]
    print("corpus:", c["total"], c["by_stage"])
    for p in results["patterns"]:
        print(p["pattern"], p["status"], p["codes"], p.get("rewrite", ""))
    print(results["check_time"])
    print(results["contracts"])
    print("wrote", path)


if __name__ == "__main__":
    main()
