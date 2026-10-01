# Bug 04: the confirmation e-mail may go out before the refund (or without
# it, if the refund fails): both depend only on the decision.
# Calyx: W0602 (compiler)
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from world import Store, llm

store = Store()


class State(TypedDict):
    order: str
    decision: str


def decide(state: State) -> dict[str, str]:
    return {"decision": llm(f"reembolsar {state['order']}?")}


def pay(state: State) -> dict[str, str]:
    store.refund(state["order"], 50.0)
    return {}


def notify(state: State) -> dict[str, str]:
    store.email("ana@exemplo.org", "Reembolso feito", state["decision"])
    return {}


g = StateGraph(State)
g.add_node("decide", decide)
g.add_node("pay", pay)
g.add_node("notify", notify)
g.add_edge(START, "decide")
g.add_edge("decide", "pay")
g.add_edge("decide", "notify")
g.add_edge("pay", END)
g.add_edge("notify", END)
print(g.compile().invoke({"order": "A100", "decision": ""}))
