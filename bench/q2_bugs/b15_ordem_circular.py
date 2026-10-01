# Bug 15: two writes, each waiting for the other: the run never ends.
# In LangGraph the "wait" is an edge, so it becomes a cycle.
# Calyx: E0506 (compiler)
from typing import TypedDict

from langgraph.graph import START, StateGraph
from world import Store

store = Store()


class State(TypedDict):
    order: str


def pay(state: State) -> dict[str, str]:
    store.refund(state["order"], 50.0)
    return {}


def notify(state: State) -> dict[str, str]:
    store.email("ana@exemplo.org", "Reembolso", "feito")
    return {}


g = StateGraph(State)
g.add_node("pay", pay)
g.add_node("notify", notify)
g.add_edge(START, "pay")
g.add_edge("pay", "notify")
g.add_edge("notify", "pay")
try:
    g.compile().invoke({"order": "A100"}, {"recursion_limit": 10})
finally:
    print(f"pagamentos: {len(store.payments)}, e-mails: {len(store.outbox)}")
