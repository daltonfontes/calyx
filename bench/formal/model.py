"""A small model of how Calyx makes calls, checked exhaustively.

docs/paper/formal.md states the rules and the theorems; this checks them on
small programs, over every combination of faults and crashes up to a bound,
and shows, for each assumption the theorems make, a counterexample when it
is dropped.

The model follows runtime/src/exec.c (run_job, resolve_uncertain) and
runtime/src/journal.c, by hand: it is not extracted from the C code.

    python3 bench/formal/model.py [--faults 2] [--crashes 2]

Writes bench/results/formal.json.
"""
from __future__ import annotations

import argparse
import json
import os
from dataclasses import dataclass, field

MAX_ATTEMPTS = 4

# ----- programs ---------------------------------------------------------------
# A call: (name, kind, extra). Kinds:
#   model      an LLM: answers differently each time it is really called
#   read       a read
#   write      a `write` with idempotency_key; extra = "const" (a key from the
#              program's inputs) or the name of the model call it comes from
#   once       a `write once`; extra = "verify" | "pause" | "accept"
PROGRAM = [
    ("m", "model", None),
    ("pay", "write", "m"),        # key from the model's answer
    ("ref", "write", "const"),
    ("mail", "once", "verify"),
    ("post", "once", "pause"),
    ("note", "once", "accept"),
    ("look", "read", None),
]

# What the theorems assume, and what drops each assumption.
@dataclass(frozen=True)
class Assume:
    begin_synced: bool = True       # the "begin" of a write once is on disk before the call
    sync_before_write: bool = True  # the journal is on disk before any write (the fix)
    machine_crash: bool = False     # crashes may lose what is not on disk
    keys_honored: bool = True       # the service applies a key once
    verify_fresh: bool = True       # a read-back sees every write already committed
    late_commit: bool = False       # a timed-out request may commit later
    redelivery: bool = False        # the transport may deliver a request twice


@dataclass
class World:
    """The service: what it applied, the truth the grader sees."""
    applied: dict[str, int] = field(default_factory=dict)
    keys: set[tuple[str, str]] = field(default_factory=set)
    in_flight: list[tuple[str, str | None]] = field(default_factory=list)

    def apply(self, call: str, key: str | None, a: Assume) -> None:
        if key is not None and a.keys_honored:
            if (call, key) in self.keys:
                return
            self.keys.add((call, key))
        self.applied[call] = self.applied.get(call, 0) + 1

    def land(self, a: Assume) -> None:
        """Requests in flight commit (late)."""
        for call, key in self.in_flight:
            self.apply(call, key, a)
        self.in_flight = []

    def count(self, call: str) -> int:
        return self.applied.get(call, 0)


class Crash(Exception):
    pass


class Paused(Exception):
    def __init__(self, call: str):
        self.call = call


class Oracle:
    """Every nondeterministic choice; the explorer enumerates all sequences."""

    def __init__(self, prefix: list[int]):
        self.prefix = prefix
        self.trace: list[tuple[int, int]] = []

    def choose(self, n: int) -> int:
        if n <= 1:
            return 0
        i = len(self.trace)
        c = self.prefix[i] if i < len(self.prefix) else 0
        self.trace.append((c, n))
        return c


@dataclass
class Journal:
    disk: list[tuple] = field(default_factory=list)   # survives any crash
    os: list[tuple] = field(default_factory=list)     # survives a process crash only

    def append(self, entry: tuple, sync: bool) -> None:
        self.os.append(entry)
        if sync:
            self.sync()

    def sync(self) -> None:
        self.disk += self.os
        self.os = []

    def entries(self) -> list[tuple]:
        return self.disk + self.os

    def crash(self, machine: bool) -> None:
        if machine:
            self.os = []
        else:
            self.sync()  # a process crash: the OS still writes it out


@dataclass
class Run:
    a: Assume
    o: Oracle
    world: World
    j: Journal
    faults: int
    crashes: int
    decisions: dict[str, str] = field(default_factory=dict)
    sent_after_done: list[str] = field(default_factory=list)

    def maybe_crash(self) -> None:
        if self.crashes and self.o.choose(2):
            self.crashes -= 1
            machine = self.a.machine_crash and self.o.choose(2) == 1
            self.j.crash(machine)
            raise Crash()

    def transport(self, call: str, key: str | None) -> str:
        """Sends one request: ok | lost (not applied) | timeout (applied, answer lost) | late."""
        kinds = ["ok"]
        if self.faults:
            kinds += ["lost", "timeout"] + (["late"] if self.a.late_commit else [])
        k = kinds[self.o.choose(len(kinds))]
        if k != "ok":
            self.faults -= 1
        if k in ("ok", "timeout"):
            self.world.apply(call, key, self.a)
            if self.a.redelivery and self.faults and self.o.choose(2):
                self.faults -= 1
                self.world.apply(call, key, self.a)
        elif k == "late":
            self.world.in_flight.append((call, key))
        return k

    def looked_up(self, call: str) -> bool:
        """`verify`: a read-back of the service."""
        if self.a.verify_fresh:
            self.world.land(self.a)  # a fresh read sees what was committed
        return self.world.count(call) > 0


def execute(r: Run, values: dict[str, str]) -> None:
    done = {e[1]: e[2] for e in r.j.entries() if e[0] == "done"}
    begun = {e[1] for e in r.j.entries() if e[0] == "begin"}
    for name, kind, extra in PROGRAM:
        if name in done:
            values[name] = done[name]
            continue
        r.maybe_crash()
        if kind in ("model", "read"):
            for _ in range(MAX_ATTEMPTS):
                if r.faults and r.o.choose(2):
                    r.faults -= 1
                    continue
                v = f"{name}{r.o.choose(2)}" if kind == "model" else "v"
                break
            else:
                raise Crash()  # the run fails; resuming tries again
            r.maybe_crash()
            r.j.append(("done", name, v), sync=False)
            values[name] = v
            continue
        if kind == "write":
            key = "k" if extra == "const" else values[extra]
            if r.a.sync_before_write:
                r.j.sync()
            for _ in range(MAX_ATTEMPTS):
                r.maybe_crash()
                k = r.transport(name, key)
                if k == "ok":
                    break
            else:
                raise Crash()
            r.maybe_crash()
            r.j.append(("done", name, "ok"), sync=True)
            values[name] = "ok"
            continue
        # write once
        uncertain = name in begun
        if not uncertain:
            r.j.append(("begin", name), sync=r.a.begin_synced)
        for attempt in range(MAX_ATTEMPTS):
            if uncertain:
                decision = resolve(r, name, extra)
                uncertain = False
                if decision == "done":
                    break
                # redo: make it again
            r.maybe_crash()
            k = r.transport(name, None)
            if k == "ok":
                break
            if k == "lost" and not r.a.late_commit:
                # the runtime cannot tell `lost` from `timeout`: both are Timeout
                pass
            uncertain = True
        else:
            raise Crash()
        r.maybe_crash()
        r.j.append(("done", name, "ok"), sync=True)
        values[name] = "ok"


def resolve(r: Run, name: str, policy: str) -> str:
    """resolve_uncertain: done or redo, or the run pauses for a person."""
    if name in r.decisions:
        return r.decisions.pop(name)
    if policy == "accept":
        return "done"
    if policy == "verify":
        return "done" if r.looked_up(name) else "redo"
    raise Paused(name)


def run_all(a: Assume, faults: int, crashes: int, prefix: list[int]) -> tuple[Oracle, dict]:
    o = Oracle(prefix)
    r = Run(a, o, World(), Journal(), faults, crashes)
    finished = False
    for _ in range(crashes + 6):  # the run, then each resume
        try:
            execute(r, {})
            finished = True
            break
        except Crash:
            continue
        except Paused as p:
            # A person checks the service, as LIMBO's operator does.
            r.world.land(a)
            r.decisions[p.call] = "done" if r.world.count(p.call) else "redo"
    r.world.land(a)  # in-flight requests commit eventually
    return o, {"finished": finished, "applied": dict(r.world.applied)}


def violations(out: dict) -> list[str]:
    bad = []
    for name, kind, extra in PROGRAM:
        n = out["applied"].get(name, 0)
        if kind in ("write", "once") and n > 1:
            bad.append(f"{name} applied {n} times")
        must = kind == "write" or (kind == "once" and extra != "accept")
        if out["finished"] and must and n == 0:
            bad.append(f"{name} never applied, run finished")
    return bad


def explore(a: Assume, faults: int, crashes: int) -> dict:
    prefix: list[int] = []
    runs = 0
    first_bad = None
    bad_runs = 0
    while True:
        o, out = run_all(a, faults, crashes, prefix)
        runs += 1
        bad = violations(out)
        if bad:
            bad_runs += 1
            if first_bad is None:
                first_bad = {"violations": bad, "choices": [c for c, _ in o.trace]}
        t = o.trace
        while t and t[-1][0] + 1 >= t[-1][1]:
            t.pop()
        if not t:
            break
        prefix = [c for c, _ in t[:-1]] + [t[-1][0] + 1]
    return {"executions": runs, "violating": bad_runs, "example": first_bad}


CASES = [
    ("assumptions hold (process crashes)", Assume()),
    ("assumptions hold (machine crashes too)", Assume(machine_crash=True)),
    ("no sync before a keyed write, machine crashes (the bug fixed)",
     Assume(machine_crash=True, sync_before_write=False)),
    ("begin of write once not on disk, machine crashes",
     Assume(machine_crash=True, begin_synced=False, sync_before_write=False)),
    ("the service ignores keys", Assume(keys_honored=False)),
    ("the read-back is stale and commits arrive late (LIMBO timeout_late)",
     Assume(verify_fresh=False, late_commit=True)),
    ("the transport delivers twice (LIMBO duplicate_delivery)", Assume(redelivery=True)),
]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--faults", type=int, default=2)
    ap.add_argument("--crashes", type=int, default=2)
    args = ap.parse_args()
    results = []
    for label, a in CASES:
        r = explore(a, args.faults, args.crashes)
        r["case"] = label
        r["assume"] = a.__dict__
        results.append(r)
        ex = r["example"]["violations"] if r["example"] else ""
        print(f"{label:70} {r['executions']:>7} executions, {r['violating']:>6} violating  {ex}")
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    with open(os.path.join(root, "bench", "results", "formal.json"), "w") as f:
        json.dump({"faults": args.faults, "crashes": args.crashes, "program": PROGRAM,
                   "results": results}, f, indent=1)


if __name__ == "__main__":
    main()
