"""W2 against the real Stripe API (test mode): recovery after `kill -9`
when the effects are a Stripe refund and a Stripe store credit.

For each crash point and each program, a fresh order (a customer and a
confirmed PaymentIntent of 300.00 USD, card pm_card_visa) is created; the
flow runs and is killed; it is resumed with `calyx resume`; then Stripe
itself is asked how many refunds and credits that run made. One of each is
right.

- refund.clyx: the contracts Calyx demands (a key on the refund, which
  Stripe honours; the credit `write once` with `verify`).
- refund_unsafe.clyx: the control, the same flow with the contracts left
  out (no key, the credit a plain `write`). Calyx warns about it (W0601).

Crash points: after each recorded step (CALYX_CRASH_AFTER), and with each
effect in flight: Stripe applied it, the server holds the answer
(`__slowrefund__`, `__slowcredit__`), and the harness kills the run as soon
as Stripe lists the effect.

    STRIPE_API_KEY=sk_test_... python bench/stripe/run_stripe.py

Results go to bench/results/stripe.json, and only when the endpoint is the
real Stripe: with STRIPE_API_BASE set (a local double, to test the wiring)
they go to bench/stripe/.work/ instead.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
CALYX = os.path.join(ROOT, "target", "release", "calyx")
WORK = os.path.join(HERE, ".work")
sys.path.insert(0, HERE)
import stripe_server as s  # noqa: E402

REAL = "STRIPE_API_BASE" not in os.environ
STEPS = ["get_order", "decide", "refund", "credit"]
SCENARIOS = {f"after-{step}": {"after": k} for k, step in enumerate(STEPS[:-1], start=1)}
SCENARIOS["refund-in-flight"] = {"in_flight": "refund", "marker": "__slowrefund__"}
SCENARIOS["credit-in-flight"] = {"in_flight": "credit", "marker": "__slowcredit__"}
PROGRAMS = {"calyx": "refund.clyx", "calyx without contracts (control)": "refund_unsafe.clyx"}


def new_order() -> str:
    customer = s.stripe("POST", "/v1/customers", {"description": "calyx bench"})["id"]
    pi = s.stripe("POST", "/v1/payment_intents", {
        "amount": 30000, "currency": "usd", "customer": customer,
        "payment_method": "pm_card_visa", "confirm": "true",
        "automatic_payment_methods[enabled]": "true",
        "automatic_payment_methods[allow_redirects]": "never",
    })
    return pi["id"]


def refunds(order: str, request: str) -> int:
    rs = s.stripe("GET", "/v1/refunds", {"payment_intent": order, "limit": 100})["data"]
    return sum(1 for r in rs if r.get("metadata", {}).get("request") == request)


def credits(order: str, request: str) -> int:
    return len(s.credits_for(order, request))


def env(extra: dict | None = None) -> dict:
    e = dict(os.environ, STRIPE_SLOW="8")
    e.update(extra or {})
    return e


def one(program: str, scenario: dict) -> dict:
    shutil.rmtree(os.path.join(HERE, ".calyx"), ignore_errors=True)
    order = new_order()
    request = f"rq-{uuid.uuid4().hex[:12]}{scenario.get('marker', '')}"
    cmd = [CALYX, "run", program, "--request", request, "--order", order,
           "--message", "arrived broken"]
    os.makedirs(WORK, exist_ok=True)
    log = os.path.join(WORK, "run.err")
    with open(log, "w") as err:
        if "after" in scenario:
            p = subprocess.Popen(cmd, cwd=HERE, env=env({"CALYX_CRASH_AFTER": str(scenario["after"])}),
                                 stdout=subprocess.DEVNULL, stderr=err, start_new_session=True)
            p.wait(timeout=180)
        else:
            p = subprocess.Popen(cmd, cwd=HERE, env=env(), stdout=subprocess.DEVNULL, stderr=err,
                                 start_new_session=True)
            seen = refunds if scenario["in_flight"] == "refund" else credits
            deadline = time.time() + 120
            while seen(order, request) == 0 and time.time() < deadline:
                time.sleep(0.3)
            os.killpg(p.pid, signal.SIGKILL)  # the runtime and the tool server with it
            p.wait()
    first = open(log).read()
    run_id = next(line.split()[-1] for line in first.splitlines() if line.startswith("calyx: run "))
    t0 = time.perf_counter()
    out = subprocess.run([CALYX, "resume", run_id], cwd=HERE, env=env(), capture_output=True,
                         text=True, timeout=300)
    took = time.perf_counter() - t0
    r, c = refunds(order, request), credits(order, request)
    return {"order": order, "request": request, "resume_exit": out.returncode,
            "refunds": r, "credits": c, "correct": out.returncode == 0 and r == 1 and c == 1,
            "resume_s": round(took, 1),
            "resume_tail": (out.stdout + out.stderr).strip().splitlines()[-1:] or [""]}


def main() -> None:
    if not os.path.exists(CALYX):
        sys.exit("build first: cargo build --release")
    rows = []
    for system, program in PROGRAMS.items():
        for name, scenario in SCENARIOS.items():
            row = {"system": system, "scenario": name, **one(program, scenario)}
            rows.append(row)
            mark = "ok " if row["correct"] else "BAD"
            print(f"{mark} {system:34} {name:18} refunds={row['refunds']} credits={row['credits']} "
                  f"resume={row['resume_s']}s {row['resume_tail'][0][:60]}", flush=True)
    for system in PROGRAMS:
        rs = [r for r in rows if r["system"] == system]
        print(f"{system}: {sum(r['correct'] for r in rs)}/{len(rs)} correct, "
              f"{sum(max(r['refunds'] - 1, 0) for r in rs)} duplicate refunds, "
              f"{sum(max(r['credits'] - 1, 0) for r in rs)} duplicate credits")
    path = (os.path.join(ROOT, "bench", "results", "stripe.json") if REAL
            else os.path.join(WORK, "stripe_double.json"))
    with open(path, "w") as f:
        json.dump({"endpoint": "api.stripe.com (test mode)" if REAL else os.environ["STRIPE_API_BASE"],
                   "when": time.strftime("%Y-%m-%d"), "rows": rows}, f, indent=1)
    print(f"results in {os.path.relpath(path, ROOT)}")


if __name__ == "__main__":
    main()
