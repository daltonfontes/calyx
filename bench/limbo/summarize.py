"""Summarizes bench/results/limbo_*.jsonl the way LIMBO's report does.

E2 table: episodes whose fault fired (mode != none): EOS, duplicate rate
(dup_executed > 0) and TS. E2k table: duplicate rate per fault mode, on the
modes whose write happened (timeout_post, http500_post, timeout_late,
partial_timeout) and on redelivery. Writes bench/results/limbo.json.

    python bench/limbo/summarize.py
"""
import json
import os

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
RES = os.path.join(ROOT, "bench", "results")
FAMILY = ["timeout_post", "http500_post", "timeout_late", "partial_timeout", "duplicate_delivery"]


def load(contract):
    path = os.path.join(RES, f"limbo_{contract}.jsonl")
    if not os.path.exists(path):
        return []
    return [json.loads(l) for l in open(path)]


def pct(rows, f):
    return round(100 * sum(1 for r in rows if f(r)) / len(rows), 1) if rows else None


def main():
    out = {}
    for contract in ("native", "keys_everywhere"):
        rows = load(contract)
        if not rows:
            continue
        faulted = [r for r in rows if r["mode"] != "none" and r["fault_triggered"]]
        clean = [r for r in rows if r["mode"] == "none"]
        s = {
            "episodes": len(rows),
            "faulted": len(faulted),
            "EOS": pct(faulted, lambda r: r["EOS"]),
            "dup": pct(faulted, lambda r: r["dup_executed"] > 0),
            "TS": pct(faulted, lambda r: r["TS"]),
            "overclaim": pct(faulted, lambda r: r["overclaim"]),
            "clean_EOS": pct(clean, lambda r: r["EOS"]),
            "paused": sum(1 for r in faulted if r["pauses"]),
            "human_minutes_mean": round(sum(r["human_minutes"] for r in faulted) / len(faulted), 2),
            "calls_mean": round(sum(r["n_calls"] for r in faulted) / len(faulted), 1),
            "by_mode": {},
        }
        for m in sorted({r["mode"] for r in rows}):
            g = [r for r in rows if r["mode"] == m and (m == "none" or r["fault_triggered"])]
            s["by_mode"][m] = {"n": len(g), "EOS": pct(g, lambda r: r["EOS"]),
                               "dup": pct(g, lambda r: r["dup_executed"] > 0), "TS": pct(g, lambda r: r["TS"])}
        out[contract] = s
        print(f"\n== {contract}: {len(rows)} episodes, {len(faulted)} with the fault fired")
        print(f"   EOS {s['EOS']}%  dup {s['dup']}%  TS {s['TS']}%  overclaim {s['overclaim']}%  "
              f"paused {s['paused']}  human {s['human_minutes_mean']} min  calls {s['calls_mean']}")
        print(f"   {'mode':20} {'n':>3} {'EOS':>6} {'dup':>6} {'TS':>6}")
        for m, v in s["by_mode"].items():
            print(f"   {m:20} {v['n']:>3} {v['EOS']:>6} {v['dup']:>6} {v['TS']:>6}")
        dups = [r for r in faulted if r["dup_executed"] > 0]
        by = {}
        for r in dups:
            by.setdefault(r["mode"], []).append(f"{r['template']}/{r['focal']}")
        for m, items in by.items():
            print(f"   dup in {m}: {', '.join(items)}")
    with open(os.path.join(RES, "limbo.json"), "w") as f:
        json.dump(out, f, indent=1)


if __name__ == "__main__":
    main()
