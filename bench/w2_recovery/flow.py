"""The W2 refund flow in Python, shared by the three baselines:
get_order -> decide (model) -> refund (pays) -> reply (model) -> email.

`careful` adds what a careful programmer writes by hand, and nothing checks
that it is there: the idempotency key on the payment (the store's protocol,
like Stripe's Idempotency-Key) and a check that the e-mail did not go out
before sending it.

BENCH_CRASH_BEFORE=<step> makes the process die (like `kill -9`) when that
step starts: the steps before it finished.
"""
import json
import os
import sys
from typing import TypedDict

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from common.fakes import Mcp, llm  # noqa: E402

store = Mcp("examples/tools/fake_store.py")


class State(TypedDict, total=False):
    request: str
    order_id: str
    message: str
    order: dict
    amount: float
    body: str
    result: str


def crash_point(step: str) -> None:
    if os.environ.get("BENCH_CRASH_BEFORE") == step:
        os._exit(137)


def get_order(s: State) -> State:
    crash_point("get_order")
    return {"order": json.loads(store.call("get_order", {"id": s["order_id"]}))}


def decide(s: State) -> State:
    crash_point("decide")
    llm(f"Proponha um reembolso para {s['order']} por causa de: {s['message']}")
    return {"amount": 1.5}  # what the fake model proposes in Calyx too


def refund(s: State, careful: bool) -> State:
    crash_point("refund")
    args = {"request": s["request"], "order": s["order_id"], "amount": s["amount"]}
    meta = {"calyx/idempotency_key": s["request"]} if careful else None
    store.call("refund", args, meta)
    return {}


def reply(s: State) -> State:
    crash_point("reply")
    return {"body": llm(f"Escreva a {s['order']['customer']} confirmando o reembolso de {s['amount']}")}


def email(s: State, careful: bool) -> State:
    crash_point("email")
    to, subject = s["order"]["email"], f"Reembolso do pedido {s['order_id']}"
    if careful and store.call("email_sent", {"to": to, "subject": subject}) == "true":
        return {"result": "já enviado"}
    store.call("email", {"to": to, "subject": subject, "body": f"{s['body']} {s['message']}"})
    return {"result": f"reembolso de {s['amount']} enviado para {to}"}
