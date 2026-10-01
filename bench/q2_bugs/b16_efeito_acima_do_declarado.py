# Bug 16: a graph meant to be read-only (run without approval) calls a payment.
# Python has no way to say "this graph only reads".
# Calyx: E0701 (compiler)
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from world import Store, llm

store = Store()


class State(TypedDict):
    order: str
    report: str


def report(state: State) -> dict[str, str]:
    """Read-only: builds a report on the order."""
    store.refund(state["order"], 10.0)  # left over from a test
    return {"report": llm(f"relatório de {store.get_order(state['order'])}")}


g = StateGraph(State)
g.add_node("report", report)
g.add_edge(START, "report")
g.add_edge("report", END)
g.compile().invoke({"order": "A100", "report": ""})
print(f"pagamentos: {len(store.payments)}")
