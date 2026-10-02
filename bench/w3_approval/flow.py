"""The W3 flow in Python, shared by LangGraph and Temporal:
get_order -> decide (model) -> wait for a person's answer (deadline 3 s)
-> pay and e-mail, or e-mail the refusal.
"""
import json
import os
import sys
from typing import TypedDict

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from common.fakes import Mcp, llm  # noqa: E402

DEADLINE_S = 3.0
store = Mcp("examples/tools/fake_store.py")


class State(TypedDict, total=False):
    request: str
    order_id: str
    message: str
    order: dict
    amount: float
    deadline: float
    answer: str
    result: str


def get_order(s: State) -> State:
    return {"order": json.loads(store.call("get_order", {"id": s["order_id"]}))}


def decide(s: State) -> State:
    llm(f"Proponha um reembolso para {s['order']} por causa de: {s['message']}")
    return {"amount": 1.5}  # what the fake model proposes in Calyx too


def pay(s: State) -> State:
    o = s["order"]
    store.call("refund", {"request": s["request"], "order": s["order_id"], "amount": s["amount"]},
               {"calyx/idempotency_key": s["request"]})
    store.call("email", {"to": o["email"], "subject": f"Reembolso do pedido {s['order_id']}",
                         "body": f"Reembolsamos {s['amount']}."})
    return {"result": f"reembolso de {s['amount']} enviado para {o['email']}"}


def refuse(s: State, reason: str) -> State:
    o = s["order"]
    store.call("email", {"to": o["email"], "subject": f"Reembolso do pedido {s['order_id']}",
                         "body": f"Não aprovado: {reason}"})
    return {"result": f"recusado: {reason}"}
