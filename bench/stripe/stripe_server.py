#!/usr/bin/env python3
"""An MCP server (stdio) in front of the real Stripe API, in test mode.

The other side of Calyx's contracts, against a service Calyx's author did
not write: a refund that forwards Calyx's idempotency key to Stripe's own
`Idempotency-Key` header, and a store credit (a customer balance
transaction) that Stripe cannot deduplicate, so Calyx declares it
`write once` and verifies before resending.

Tools (each order is a PaymentIntent whose id is the order id):

- `get_order(id)`: read. The order's customer, total and amount refunded.
- `refund(request, order, amount)`: write. A refund of `amount` on the
  order; the key in the call's `_meta` (`calyx/idempotency_key`) goes to
  Stripe as `Idempotency-Key`, so Stripe applies it once. Annotated
  `idempotencyKeyHint: true` (docs/mcp/idempotency-key-hint.md).
- `credit(order, request, amount)`: write. A store credit on the order's
  customer (a negative balance transaction), tagged with the request. No
  key: Stripe applies every call. Annotated `idempotencyKeyHint: false`.
- `credit_given(order, request)`: read. Whether that credit exists.
- `probe_order()` and `refunds_with(order, request)`: for
  `calyx check --tools --probe` (calyx.toml): a fresh test order, and how
  many refunds a request made on it.

With STRIPE_DROP_KEY=1 the server does not forward the key (a bug a server
can have) while still claiming `idempotencyKeyHint: true`: what the probe
is for.

Faults, for the crash matrix (bench/stripe/run_stripe.py): a request id
with `__slowrefund__` or `__slowcredit__` makes the server answer 30 s
after Stripe applied the write, the window in which the harness kills the
caller. Stripe's 5xx, 429 and network errors are reported with the error
text starting `Unavailable:`, `RateLimit:` or `Network:`, so Calyx treats
them as an uncertain outcome.

Needs STRIPE_API_KEY (a test key, `sk_test_...`; live keys are refused).
STRIPE_API_BASE changes the endpoint (for a local double in tests).
"""
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

KEY = os.environ.get("STRIPE_API_KEY", "")
BASE = os.environ.get("STRIPE_API_BASE", "https://api.stripe.com")
SLOW = int(os.environ.get("STRIPE_SLOW", "30"))


class Temporary(Exception):
    pass


def stripe(method, path, params=None, idempotency_key=None):
    if not KEY.startswith("sk_test_"):
        raise RuntimeError("STRIPE_API_KEY must be a test key (sk_test_...)")
    data = urllib.parse.urlencode(params or {}).encode() if method == "POST" else None
    url = BASE + path
    if method == "GET" and params:
        url += "?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Bearer {KEY}")
    if idempotency_key:
        req.add_header("Idempotency-Key", idempotency_key)
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")[:300]
        if e.code == 429:
            raise Temporary(f"RateLimit: Stripe {e.code} {body}")
        if e.code >= 500:
            raise Temporary(f"Unavailable: Stripe {e.code} {body}")
        raise RuntimeError(f"Stripe {e.code} {body}")
    except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
        raise Temporary(f"Network: {e}")


def cents(amount):
    return int(round(float(amount) * 100))


def get_order(a, meta):
    pi = stripe("GET", f"/v1/payment_intents/{a['id']}", {"expand[]": "latest_charge"})
    charge = pi.get("latest_charge") or {}
    return {"id": pi["id"], "customer": pi.get("customer") or "",
            "total": pi["amount"] / 100, "refunded": (charge.get("amount_refunded") or 0) / 100}


def refund(a, meta):
    out = stripe("POST", "/v1/refunds",
                 {"payment_intent": a["order"], "amount": cents(a["amount"]),
                  "metadata[request]": a["request"]},
                 idempotency_key=None if os.environ.get("STRIPE_DROP_KEY") == "1"
                 else meta.get("calyx/idempotency_key"))
    if "__slowrefund__" in a["request"]:
        time.sleep(SLOW)
    return {"id": out["id"]}


def customer_of(order):
    return stripe("GET", f"/v1/payment_intents/{order}")["customer"]


def credit(a, meta):
    out = stripe("POST", f"/v1/customers/{customer_of(a['order'])}/balance_transactions",
                 {"amount": -cents(a["amount"]), "currency": "usd",
                  "metadata[request]": a["request"]})
    if "__slowcredit__" in a["request"]:
        time.sleep(SLOW)
    return {"id": out["id"]}


def credits_for(order, request):
    txns = stripe("GET", f"/v1/customers/{customer_of(order)}/balance_transactions", {"limit": 100})
    return [t for t in txns["data"] if t.get("metadata", {}).get("request") == request]


def credit_given(a, meta):
    return bool(credits_for(a["order"], a["request"]))


def probe_order(a, meta):
    customer = stripe("POST", "/v1/customers", {"description": "calyx probe"})["id"]
    return stripe("POST", "/v1/payment_intents", {
        "amount": 1000, "currency": "usd", "customer": customer,
        "payment_method": "pm_card_visa", "confirm": "true",
        "automatic_payment_methods[enabled]": "true",
        "automatic_payment_methods[allow_redirects]": "never"})["id"]


def refunds_with(a, meta):
    rs = stripe("GET", "/v1/refunds", {"payment_intent": a["order"], "limit": 100})["data"]
    return sum(1 for r in rs if r.get("metadata", {}).get("request") == a["request"])


def schema(*names):
    return {"type": "object", "properties": {n: {} for n in names}, "required": list(names)}


READ = {"readOnlyHint": True}
TOOLS = [
    {"name": "get_order", "inputSchema": schema("id"), "annotations": READ},
    {"name": "refund", "inputSchema": schema("request", "order", "amount"),
     "annotations": {"readOnlyHint": False, "destructiveHint": False, "idempotentHint": False,
                     "idempotencyKeyHint": True}},
    {"name": "credit", "inputSchema": schema("order", "request", "amount"),
     "annotations": {"readOnlyHint": False, "destructiveHint": False, "idempotentHint": False,
                     "idempotencyKeyHint": False}},
    {"name": "credit_given", "inputSchema": schema("order", "request"), "annotations": READ},
    {"name": "probe_order", "inputSchema": schema(),
     "annotations": {"readOnlyHint": False, "idempotentHint": False}},
    {"name": "refunds_with", "inputSchema": schema("order", "request"), "annotations": READ},
]
CALLS = {"get_order": get_order, "refund": refund, "credit": credit, "credit_given": credit_given,
         "probe_order": probe_order, "refunds_with": refunds_with}


def text(value, error=False):
    out = {"content": [{"type": "text", "text": value if isinstance(value, str) else json.dumps(value)}]}
    if error:
        out["isError"] = True
    return out


def call(name, args, meta):
    if name not in CALLS:
        return text(f"unknown tool {name}", error=True)
    try:
        return text(CALLS[name](args, meta))
    except Temporary as e:
        return text(str(e), error=True)
    except Exception as e:  # noqa: BLE001 (reported to the caller, not retried)
        return text(f"{type(e).__name__}: {e}", error=True)


def answer(msg_id, result=None, error=None):
    out = {"jsonrpc": "2.0", "id": msg_id}
    if error is None:
        out["result"] = result
    else:
        out["error"] = error
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        method, msg_id = msg.get("method"), msg.get("id")
        if msg_id is None:
            continue
        if method == "initialize":
            answer(msg_id, {"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "stripe-test", "version": "0.1"}})
        elif method == "tools/list":
            answer(msg_id, {"tools": TOOLS})
        elif method == "tools/call":
            p = msg["params"]
            answer(msg_id, call(p.get("name"), p.get("arguments", {}), p.get("_meta", {})))
        else:
            answer(msg_id, error={"code": -32601, "message": f"unknown method {method}"})


if __name__ == "__main__":
    main()
