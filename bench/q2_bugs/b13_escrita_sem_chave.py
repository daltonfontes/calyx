# Bug 13: a payment with no idempotency key: a retry after a timeout pays twice.
# Calyx: W0601 (compiler)
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langgraph.types import RetryPolicy
from world import Store

store = Store()
attempts = 0


class State(TypedDict):
    order: str


def pay(state: State) -> dict[str, str]:
    global attempts
    attempts += 1
    store.refund(state["order"], 50.0)
    if attempts == 1:
        raise TimeoutError("o provedor não respondeu a tempo")  # but it paid
    return {}


g = StateGraph(State)
g.add_node("pay", pay, retry_policy=RetryPolicy(max_attempts=3, retry_on=TimeoutError))
g.add_edge(START, "pay")
g.add_edge("pay", END)
g.compile().invoke({"order": "A100"})
print(f"pagamentos: {len(store.payments)}")
