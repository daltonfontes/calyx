"""W8: several machines, with the journal in PostgreSQL.

What sharing runs through a database costs, and how fast another machine
takes a run over. Three parts:

- journal: the wall time of a run with the journal in a file and in
  PostgreSQL, with the database on this machine and a few milliseconds
  away (a TCP proxy that delays every byte, delay_proxy.py). Two
  workloads: W1's fan-out (500 questions, models that answer at once:
  1,003 journal lines from 8 threads) and an agent of 100 turns (one
  thread, a model and a tool call per turn). With W8_OLD_CALYX pointing to
  a v0.3.5 binary, also the journal before group commit (one transaction
  per line).
- takeover: machine A dies in the middle of W2's refund (models of 1 s),
  and a `calyx worker --every 1` on machine B finishes it. Two ways to
  die: the process is killed (its OS closes the connection), or the
  machine goes silent (iptables drops every packet of A's connection, then
  the process is killed: nothing reaches the server, as with a power cut).
  Measured: from the death to the run finished on B. The silent case needs
  root and the server listening on 127.0.0.2 (A's address); without them
  it is skipped.
- entities: P processes, half on each machine, each sends 50 messages to
  the same entity. Messages per second, and the final count (none lost).
  In files (one machine, flock) and in PostgreSQL (two machines).

The database: W8_DATABASE (default calyx@127.0.0.1:55432/postgres). Results
go to bench/results/w8.json.

    python bench/run_w8.py
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
W8 = os.path.join(HERE, "w8_machines")
W1 = os.path.join(HERE, "w1_fanout")
W2 = os.path.join(HERE, "w2_recovery")
WORK = os.path.join(W8, ".work")
CALYX = os.path.join(ROOT, "target", "release", "calyx")
OLD = os.environ.get("W8_OLD_CALYX")
DB = os.environ.get("W8_DATABASE", "calyx@127.0.0.1:55432/postgres")
USER_HOST, _, DBNAME = DB.partition("/")
USER, _, HOSTPORT = USER_HOST.partition("@")
HOST, _, PORT = HOSTPORT.partition(":")
REPS = int(os.environ.get("W8_REPS", "5"))
# One-way delays added by the proxy, in ms (0: straight to the server).
DELAYS = [0, 1, 2.5]
SILENT_ADDR = "127.0.0.2"
SILENT_CAP = 60.0

AGENT = """model busy = "fake-busy"

tool web_search(query: Text) -> Text:
    effect read
    max_output 4000 tokens

prompt investigate(question: Text) -> Text:
    \"\"\"Responda: {question}\"\"\"

graph ask(question: Text) -> Text:
    answer = agent busy:
        tools [web_search]
        max_turns 100
        task investigate(question)
        on turn_limit: final_answer
        on stuck: fail "stuck"
    return answer
"""

COUNTER = """entity Counter(key name: Text):
    state count: Nat = 0

    on Get() -> Nat:
        return count

    on Add():
        next count = count + 1

graph bumps(name: Text, items: List[Text]) -> Nat:
    sent = for each i in items:
        send Counter(name).Add()
    return 0

graph read(name: Text) -> Nat:
    return ask Counter(name).Get()
"""


def url(port=PORT, host=HOST) -> str:
    return f"postgresql://{USER}@{host}:{port}/{DBNAME}"


def psql(sql: str, u: str | None = None) -> str:
    return subprocess.run(["psql", u or url(), "-Atc", sql], capture_output=True,
                          text=True, check=True).stdout.strip()


def rtt_ms(u: str) -> float:
    """The median of 20 `SELECT 1` round trips on one connection."""
    script = "\\timing on\n" + "SELECT 1;\n" * 20
    out = subprocess.run(["psql", u, "-At"], input=script, capture_output=True, text=True,
                         check=True).stdout
    times = [float(line.split()[1]) for line in out.splitlines() if line.startswith("Time:")]
    return round(statistics.median(times), 2)


def timed(cmd, cwd, env) -> float:
    t = time.perf_counter()
    subprocess.run(cmd, cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                   check=True)
    return time.perf_counter() - t


def proxies():
    """One proxy per delay: {delay: port}."""
    procs, ports = [], {0: PORT}
    for i, d in enumerate(DELAYS):
        if d == 0:
            continue
        port = str(55460 + i)
        p = subprocess.Popen([sys.executable, os.path.join(W8, "delay_proxy.py"), port, HOST, PORT,
                              str(d)], stdout=subprocess.PIPE, text=True)
        p.stdout.readline()
        procs.append(p)
        ports[d] = port
    return procs, ports


def journal_cost(ports) -> dict:
    os.makedirs(WORK, exist_ok=True)
    questions = os.path.join(WORK, "q500.json")
    with open(questions, "w") as f:
        json.dump([f"pergunta {i}" for i in range(500)], f)
    agent_dir = os.path.join(WORK, "agent")
    os.makedirs(agent_dir, exist_ok=True)
    with open(os.path.join(agent_dir, "a.clyx"), "w") as f:
        f.write(AGENT)
    with open(os.path.join(agent_dir, "calyx.toml"), "w") as f:
        f.write('[tools.web_search]\ncommand = ["python3", "%s"]\n'
                % os.path.join(ROOT, "examples", "tools", "fake_search.py"))
    workloads = {
        "fanout_500": (W1, ["run", "research_0.clyx", "--fake-models", "--quiet",
                            "--questions", "@" + questions]),
        "agent_100_turns": (agent_dir, ["run", "a.clyx", "--fake-models", "--quiet",
                                        "--question", "baterias"]),
    }
    binaries = {"group_commit": CALYX}
    if OLD:
        binaries["per_line"] = OLD
    out = {"rtt_ms": {str(d): rtt_ms(url(p)) for d, p in ports.items()}, "workloads": {}}
    for name, (cwd, args) in workloads.items():
        env = {k: v for k, v in os.environ.items() if k != "CALYX_DATABASE_URL"}
        rows = {"file": round(statistics.median(timed([CALYX, *args], cwd, env)
                                                for _ in range(REPS)), 3)}
        for bname, binary in binaries.items():
            for d, port in ports.items():
                e = dict(env, CALYX_DATABASE_URL=url(port))
                rows[f"pg_{bname}_delay_{d}"] = round(statistics.median(
                    timed([binary, *args], cwd, e) for _ in range(REPS)), 3)
        out["workloads"][name] = rows
        print(name, rows, flush=True)
    return out


def status(run: str) -> str:
    return psql(f"SELECT status FROM calyx_runs WHERE id = '{run}'")


def takeover(binary: str, silent: bool) -> float | None:
    """Seconds from A's death to the run finished by B's worker; None if not
    within SILENT_CAP."""
    work = os.path.join(WORK, "takeover")
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(os.path.join(work, "a"))
    os.makedirs(os.path.join(work, "b"))
    env = dict(os.environ, CALYX_FAKE_STORE=os.path.join(work, "store.json"))
    a_url = url(host=SILENT_ADDR) if silent else url()
    a = subprocess.Popen([binary, "run", os.path.join(W2, "refund.clyx"), "--request", "R1",
                          "--order", "A100", "--message", "chegou quebrado",
                          "--config", os.path.join(W2, "calyx.toml")],
                         cwd=os.path.join(work, "a"), env=dict(env, CALYX_DATABASE_URL=a_url),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    run = a.stderr.readline().split()[-1]
    time.sleep(1.5)  # the first model call is under way
    rules = [["INPUT", "-d", SILENT_ADDR, "-p", "tcp", "--dport", PORT, "-j", "DROP"],
             ["INPUT", "-s", SILENT_ADDR, "-p", "tcp", "--sport", PORT, "-j", "DROP"]]
    if silent:
        for r in rules:
            subprocess.run(["iptables", "-I", *r], check=True)
    a.kill()
    a.wait()
    died = time.perf_counter()
    b = subprocess.Popen([binary, "worker", "--every", "1", "--quiet"],
                         cwd=os.path.join(work, "b"), env=dict(env, CALYX_DATABASE_URL=url()),
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    took = None
    try:
        while time.perf_counter() - died < SILENT_CAP:
            if status(run) == "finished":
                took = round(time.perf_counter() - died, 2)
                break
            time.sleep(0.05)
    finally:
        b.kill()
        b.wait()
        if silent:
            for r in rules:
                subprocess.run(["iptables", "-D", *r], check=False)
            # The dead connection the server still keeps, if any.
            psql("SELECT pg_terminate_backend(pid) FROM pg_stat_activity "
                 "WHERE usename = current_user AND pid <> pg_backend_pid()")
    with open(env["CALYX_FAKE_STORE"]) as f:
        db = json.load(f)
    assert took is None or (len(db.get("payments", [])), len(db.get("outbox", []))) == (1, 1), db
    return took


def can_go_silent() -> bool:
    if os.geteuid() != 0 or not shutil.which("iptables"):
        return False
    try:
        psql("SELECT 1", url(host=SILENT_ADDR))
        return True
    except subprocess.CalledProcessError:
        return False


def takeovers() -> dict:
    binaries = {"new": CALYX}
    if OLD:
        binaries["v0.3.5"] = OLD
    out = {}
    for name, binary in binaries.items():
        killed = [takeover(binary, False) for _ in range(REPS)]
        out[f"process_killed_{name}"] = {"seconds": killed, "median": statistics.median(killed)}
        print("process killed", name, killed, flush=True)
        if can_go_silent():
            n = REPS if name == "new" else 1
            silent = [takeover(binary, True) for _ in range(n)]
            ok = [s for s in silent if s is not None]
            out[f"machine_silent_{name}"] = {
                "seconds": silent, "cap": SILENT_CAP,
                "median": statistics.median(ok) if len(ok) == len(silent) else None}
            print("machine silent", name, silent, flush=True)
    return out


def entities(ports) -> dict:
    work = os.path.join(WORK, "entities")
    shutil.rmtree(work, ignore_errors=True)
    for m in ["a", "b"]:
        os.makedirs(os.path.join(work, m))
        with open(os.path.join(work, m, "c.clyx"), "w") as f:
            f.write(COUNTER)
    items = json.dumps([str(i) for i in range(50)])
    out = {}
    backends = {"file": None, "pg_delay_0": ports[0], f"pg_delay_{DELAYS[-1]}": ports[DELAYS[-1]]}
    for backend, port in backends.items():
        for procs in [1, 2, 4, 8]:
            key = f"{backend}-{procs}-{time.time_ns()}"
            env = {k: v for k, v in os.environ.items() if k != "CALYX_DATABASE_URL"}
            if port:
                env["CALYX_DATABASE_URL"] = url(port)
            t = time.perf_counter()
            ps = [subprocess.Popen([CALYX, "run", "c.clyx", "--graph", "bumps", "--quiet",
                                    "--name", key, "--items", items],
                                   # In files, one machine: the lock is local.
                                   cwd=os.path.join(work, "a" if not port or i % 2 == 0 else "b"),
                                   env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                  for i in range(procs)]
            for p in ps:
                assert p.wait() == 0
            took = time.perf_counter() - t
            count = int(subprocess.run([CALYX, "run", "c.clyx", "--graph", "read", "--quiet",
                                        "--no-journal", "--name", key],
                                       cwd=os.path.join(work, "b" if port else "a"),
                                       env=env, capture_output=True, text=True,
                                       check=True).stdout.strip())
            out[f"{backend}_{procs}"] = {"messages": procs * 50, "count": count,
                                         "seconds": round(took, 3),
                                         "per_second": round(procs * 50 / took, 1)}
            print(backend, procs, out[f"{backend}_{procs}"], flush=True)
    return out


def main():
    subprocess.run(["cargo", "build", "--release", "-q"], cwd=ROOT, check=True)
    procs, ports = proxies()
    try:
        results = {
            "database": DB,
            "server": psql("SHOW server_version"),
            "synchronous_commit": psql("SHOW synchronous_commit"),
            "reps": REPS,
            "journal": journal_cost(ports),
            "takeover": takeovers(),
            "entities": entities(ports),
        }
    finally:
        for p in procs:
            p.kill()
    path = os.path.join(HERE, "results", "w8.json")
    with open(path, "w") as f:
        json.dump(results, f, indent=2)
        f.write("\n")
    print("wrote", path)


if __name__ == "__main__":
    main()
