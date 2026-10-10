#!/usr/bin/env python3
"""A fake store, served over MCP (stdio): orders, refunds and e-mail.

It shows the other side of Calyx's contracts for external writes:

- `refund` is a `write` with an idempotency key: the same key never pays
  twice (decision D2). Its preconditions (`requires`, decision D29) come in
  the call's `_meta` and are checked against the order's current state,
  in the same step that pays. If one does not hold, it answers
  `PreconditionFailed: ...` and pays nothing.
- `email` is `write once`. Words in the body simulate failures:
  `__lost__` sends the e-mail and then never answers (the caller times out
  not knowing it was sent); `__down__` crashes before sending; `__flaky__`
  crashes before sending the first time only; `__slowmail__` sends it and
  answers 3 s later; `__gateway__` sends it and answers with an error
  starting `Timeout:`, as a server in front of a mail gateway that did not
  answer would. `email_many` to an address starting `metade@` sends half
  of its e-mails and crashes, the first time. `payments_for` counts the
  payments of a request (what `calyx check --tools --probe` reads); with
  CALYX_FAKE_STORE_IGNORE_KEYS=1 the store ignores keys while still
  claiming `idempotencyKeyHint: true`, as a broken server would.
  `charge` and `uncharge` are a write and the write that undoes it (the
  saga of a race, decision D12): `charges` and `uncharged` count them. A refund whose request has `__slowpay__` pays and
  answers 3 s later. The benchmarks (bench/) kill the caller in that window.
- Every payment made is listed in `payments`, so duplicates can be counted.
- `email_sent` tells whether an e-mail went out: what `on_uncertain
  verify(...)` asks after such a failure. `mail` is `email` returning the
  message's id, and `mails_to` finds the ids of what went out.

State lives in a JSON file: $CALYX_FAKE_STORE, or .calyx/fake_store.json
next to calyx.toml. Delete it to start over.
"""
import json
import os
import sys
import time

PATH = os.environ.get("CALYX_FAKE_STORE") or os.path.join(".calyx", "fake_store.json")

ORDERS = {
    "A100": {"id": "A100", "customer": "Ana", "email": "ana@exemplo.org",
             "total": 300.0, "refunded": 0.0, "status": "Delivered"},
    "A200": {"id": "A200", "customer": "Bruno", "email": "bruno@exemplo.org",
             "total": 120.0, "refunded": 0.0, "status": "Pending"},
    "A300": {"id": "A300", "customer": "Carla", "email": "carla@exemplo.org",
             "total": 50.0, "refunded": 40.0, "status": "Delivered"},
}


def load():
    try:
        with open(PATH) as f:
            return json.load(f)
    except FileNotFoundError:
        return {"orders": ORDERS, "refund_keys": {}, "outbox": []}


def save(db):
    os.makedirs(os.path.dirname(PATH) or ".", exist_ok=True)
    tmp = PATH + ".tmp"
    with open(tmp, "w") as f:
        json.dump(db, f, ensure_ascii=False, indent=1)
    os.replace(tmp, PATH)


def schema(**props):
    return {"type": "object", "properties": {k: {"type": t} for k, t in props.items()},
            "required": list(props)}


# What the server says about each tool (MCP `annotations`); `calyx check
# --tools` compares them with the program's declarations.
READ = {"readOnlyHint": True}
SEND = {"readOnlyHint": False, "destructiveHint": False, "idempotentHint": False}

TOOLS = [
    {"name": "get_order", "description": "Um pedido, pelo id.",
     "inputSchema": schema(id="string"), "annotations": READ},
    {"name": "refund", "description": "Reembolsa parte de um pedido.",
     "inputSchema": schema(request="string", order="string", amount="number"),
     "annotations": {"readOnlyHint": False, "destructiveHint": True, "idempotentHint": False,
                     "idempotencyKeyHint": True}},
    {"name": "charge", "description": "Cobra um valor; a mesma chave nunca cobra duas vezes.",
     "inputSchema": schema(request="string", amount="number"),
     "annotations": {"readOnlyHint": False, "destructiveHint": False, "idempotentHint": False,
                     "idempotencyKeyHint": True}},
    {"name": "uncharge", "description": "Estorna a cobrança de uma solicitação (nada, se não houve).",
     "inputSchema": schema(request="string"),
     "annotations": {"readOnlyHint": False, "destructiveHint": False, "idempotentHint": True,
                     "idempotencyKeyHint": True}},
    {"name": "payments_for", "description": "Quantos pagamentos uma solicitação fez.",
     "inputSchema": schema(request="string"), "annotations": READ},
    {"name": "email", "description": "Envia um e-mail.",
     "inputSchema": schema(to="string", subject="string", body="string"), "annotations": SEND},
    {"name": "email_sent", "description": "Se um e-mail com esse assunto já foi enviado.",
     "inputSchema": schema(to="string", subject="string"), "annotations": READ},
    {"name": "mail", "description": "Envia um e-mail e devolve o id dele.",
     "inputSchema": schema(to="string", subject="string", body="string"), "annotations": SEND},
    {"name": "mails_to", "description": "Os ids dos e-mails enviados com esse assunto.",
     "inputSchema": schema(to="string", subject="string"), "annotations": READ},
    {"name": "email_many", "description": "Envia um e-mail por assunto, na ordem.",
     "inputSchema": schema(to="string", subjects="array"), "annotations": SEND},
    {"name": "subjects_sent", "description": "Dos assuntos dados, os já enviados para esse endereço.",
     "inputSchema": schema(to="string", subjects="array"), "annotations": READ},
]

# ----- preconditions (D29) ---------------------------------------------------

OPS = {
    "==": lambda a, b: a == b, "!=": lambda a, b: a != b,
    "<": lambda a, b: a < b, "<=": lambda a, b: a <= b,
    ">": lambda a, b: a > b, ">=": lambda a, b: a >= b,
    "+": lambda a, b: a + b, "-": lambda a, b: a - b,
    "*": lambda a, b: a * b, "/": lambda a, b: a / b,
    "and": lambda a, b: a and b, "or": lambda a, b: a or b,
}


def evaluate(e, state):
    if "state" in e:
        return state[e["state"]]
    if "value" in e:
        return e["value"]
    if "v" in e:
        v = evaluate(e["v"], state)
        return (not v) if e["op"] == "not" else -v
    return OPS[e["op"]](evaluate(e["l"], state), evaluate(e["r"], state))


def show(e):
    if "state" in e:
        return "state." + e["state"]
    if "value" in e:
        return json.dumps(e["value"], ensure_ascii=False)
    if "v" in e:
        return f"{e['op']} {show(e['v'])}"
    return f"{show(e['l'])} {e['op']} {show(e['r'])}"


# ----- tools -------------------------------------------------------------------

def text(t, error=False):
    out = {"content": [{"type": "text", "text": t}]}
    if error:
        out["isError"] = True
    return out


def call(name, args, meta):
    db = load()
    if name == "get_order":
        order = db["orders"].get(args.get("id"))
        if not order:
            return text(f"pedido {args.get('id')} não existe", error=True)
        return text(json.dumps(order, ensure_ascii=False))
    if name == "refund":
        key = meta.get("calyx/idempotency_key")
        if os.environ.get("CALYX_FAKE_STORE_IGNORE_KEYS") == "1":
            key = None  # a broken server: says it honours keys, does not
        if key is not None and key in db["refund_keys"]:
            return text("null")  # already done: the same key never pays twice
        order = db["orders"].get(args.get("order"))
        if not order:
            return text(f"pedido {args.get('order')} não existe", error=True)
        state = {k: order[k] for k in ("status", "total", "refunded")}
        for cond in meta.get("calyx/requires", []):
            if not evaluate(cond, state):
                return text(f"PreconditionFailed: {show(cond)} "
                            f"(status {state['status']}, total {state['total']}, "
                            f"refunded {state['refunded']})", error=True)
        order["refunded"] += float(args.get("amount", 0))
        if key is not None:
            db["refund_keys"][key] = args
        db.setdefault("payments", []).append(args)
        save(db)
        if "__slowpay__" in str(args.get("request", "")):
            time.sleep(3)  # paid; the answer is still on its way
        return text("null")
    if name in ("email", "mail"):
        body = args.get("body", "")
        if "__down__" in body:
            sys.exit(3)  # crashes before sending
        if "__flaky__" in body and args.get("subject") not in db.setdefault("crashed", []):
            db["crashed"].append(args.get("subject"))
            save(db)
            sys.exit(3)  # crashes before sending, this time only
        db["outbox"].append({"to": args.get("to"), "subject": args.get("subject"), "body": body})
        save(db)
        mid = f"msg-{len(db['outbox'])}"
        if "__gateway__" in body:
            return text("Timeout: the mail gateway did not answer", error=True)
        if "__lost__" in body:
            time.sleep(3600)  # sent, but the answer never comes
        if "__slowmail__" in body:
            time.sleep(3)  # sent; the answer is still on its way
        return text(mid if name == "mail" else "null")
    if name == "email_many":
        # To `metade@...`, the first time: sends half of them, then crashes.
        subjects = args.get("subjects", [])
        half = args.get("to", "").startswith("metade@") and not db.get("half_done")
        for k, subject in enumerate(subjects):
            if half and k == len(subjects) // 2:
                db["half_done"] = True
                save(db)
                sys.exit(3)
            db["outbox"].append({"to": args.get("to"), "subject": subject, "body": ""})
            save(db)
        return text("null")
    if name == "subjects_sent":
        sent = {m["subject"] for m in db["outbox"] if m["to"] == args.get("to")}
        return text(json.dumps([x for x in args.get("subjects", []) if x in sent]))
    if name == "mails_to":
        return text(json.dumps([f"msg-{i + 1}" for i, m in enumerate(db["outbox"])
                                if m["to"] == args.get("to") and m["subject"] == args.get("subject")]))
    if name == "charge":
        key = meta.get("calyx/idempotency_key")
        if key is not None and key in db.setdefault("charge_keys", []):
            return text("null")
        db.setdefault("charges", []).append(args)
        if key is not None:
            db["charge_keys"].append(key)
        save(db)
        if "__slowcharge__" in str(args.get("request", "")):
            time.sleep(1.5)  # charged; the answer is still on its way
        return text("null")
    if name == "uncharge":
        # Undoes the charge of a request; nothing if there was none, or it
        # was already undone (the undo may be sent again after a crash).
        charged = [c for c in db.get("charges", []) if c.get("request") == args.get("request")]
        undone = db.setdefault("uncharged", [])
        if charged and args.get("request") not in undone:
            undone.append(args.get("request"))
            save(db)
        return text("null")
    if name == "payments_for":
        return text(str(sum(1 for p in db.get("payments", []) if p.get("request") == args.get("request"))))
    if name == "email_sent":
        sent = any(m["to"] == args.get("to") and m["subject"] == args.get("subject")
                   for m in db["outbox"])
        return text("true" if sent else "false")
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
        continue  # notifications need no answer
    if method == "initialize":
        answer(msg_id, {
            "protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake-store", "version": "0.1"},
        })
    elif method == "tools/list":
        answer(msg_id, {"tools": TOOLS})
    elif method == "tools/call":
        p = msg["params"]
        answer(msg_id, call(p.get("name"), p.get("arguments", {}), p.get("_meta", {})))
    else:
        answer(msg_id, error={"code": -32601, "message": f"unknown method {method}"})
