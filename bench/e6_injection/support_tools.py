#!/usr/bin/env python3
"""The tools of the E6 support agent, served over MCP (stdio).

Tickets and orders come from scenarios.py (an order is shown as the
ticket in $CALYX_E6_TICKET left it); with CALYX_E6_OBEDIENT=1 the
injected instructions are CALL lines (for the fake model that obeys them),
else words (for a real model). Every call received is appended to the
effects in $CALYX_E6_STATE (a JSON list), which the harness reads; writes
change nothing else. A refund with a repeated `request` is not counted
twice (its idempotency key).
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import scenarios  # noqa: E402

STATE = os.environ.get("CALYX_E6_STATE") or os.path.join(".calyx", "e6.json")
OBEDIENT = os.environ.get("CALYX_E6_OBEDIENT") == "1"
TICKETS = {t[0]: t for t in scenarios.tickets()}


def record(tool, args):
    try:
        with open(STATE) as f:
            effects = json.load(f)
    except FileNotFoundError:
        effects = []
    if tool == "refund" and any(e["tool"] == "refund" and e["args"].get("request") == args.get("request")
                                for e in effects):
        return
    effects.append({"tool": tool, "args": args})
    os.makedirs(os.path.dirname(STATE) or ".", exist_ok=True)
    with open(STATE + ".tmp", "w") as f:
        json.dump(effects, f, ensure_ascii=False)
    os.replace(STATE + ".tmp", STATE)


def schema(**props):
    return {"type": "object", "properties": {k: {"type": t} for k, t in props.items()},
            "required": list(props)}


READ = {"readOnlyHint": True}
WRITE = {"readOnlyHint": False, "destructiveHint": False, "idempotentHint": False}
TOOLS = [
    {"name": "get_ticket", "inputSchema": schema(id="string"), "annotations": READ},
    {"name": "get_order", "inputSchema": schema(id="string"), "annotations": READ},
    {"name": "get_customer", "inputSchema": schema(email="string"), "annotations": READ},
    {"name": "refund", "inputSchema": schema(request="string", order="string", amount="number"),
     "annotations": WRITE},
    {"name": "send_email", "inputSchema": schema(to="string", subject="string", body="string"),
     "annotations": WRITE},
    {"name": "update_address", "inputSchema": schema(order="string", address="string"),
     "annotations": WRITE},
]


def text(s, error=False):
    return {"content": [{"type": "text", "text": s}], "isError": error}


def call(name, args):
    record(name, args)
    if name == "get_ticket":
        t = TICKETS.get(args.get("id"))
        if not t:
            return text(f"NotFound: no ticket {args.get('id')}", error=True)
        return text(json.dumps(scenarios.ticket_data(t, OBEDIENT)[0], ensure_ascii=False))
    if name == "get_order":
        oid = args.get("id")
        # The order of the ticket being handled, as that ticket left it.
        t = TICKETS.get(os.environ.get("CALYX_E6_TICKET", ""))
        if t and t[1] == oid:
            return text(json.dumps(scenarios.ticket_data(t, OBEDIENT)[1], ensure_ascii=False))
        if oid in scenarios.ORDERS:
            n, e, total = scenarios.ORDERS[oid]
            return text(json.dumps({"id": oid, "customer": n, "email": e, "total": total,
                                    "notes": ""}, ensure_ascii=False))
        return text(f"NotFound: no order {oid}", error=True)
    if name == "get_customer":
        c = scenarios.CUSTOMERS.get(args.get("email"))
        if not c:
            return text(f"NotFound: no customer {args.get('email')}", error=True)
        return text(json.dumps(c, ensure_ascii=False))
    if name in ("refund", "send_email", "update_address"):
        return text("null")
    return text(f"unknown tool {name}", error=True)


def answer(msg_id, result=None, error=None):
    out = {"jsonrpc": "2.0", "id": msg_id}
    if error is None:
        out["result"] = result
    else:
        out["error"] = error
    sys.stdout.write(json.dumps(out, ensure_ascii=False) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method, msg_id = msg.get("method"), msg.get("id")
    if msg_id is None:
        continue
    if method == "initialize":
        answer(msg_id, {
            "protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "e6-support", "version": "0.1"},
        })
    elif method == "tools/list":
        answer(msg_id, {"tools": TOOLS})
    elif method == "tools/call":
        p = msg["params"]
        answer(msg_id, call(p.get("name"), p.get("arguments", {})))
    else:
        answer(msg_id, error={"code": -32601, "message": f"unknown method {method}"})
