# Bug 02: a non-idempotent e-mail with no plan for a crash in the middle of
# sending: on resume, send again or not? Nothing asks the programmer.
# Calyx: E0304 (compiler)
import sqlite3
from typing import TypedDict

from langgraph.checkpoint.sqlite import SqliteSaver
from langgraph.graph import END, START, StateGraph
from world import Store, llm

store = Store()


class State(TypedDict):
    customer: str
    body: str


def write(state: State) -> dict[str, str]:
    return {"body": llm(f"e-mail para {state['customer']}")}


def send(state: State) -> dict[str, str]:
    store.email(state["customer"], "Seu pedido", state["body"])
    return {}


g = StateGraph(State)
g.add_node("write", write)
g.add_node("send", send)
g.add_edge(START, "write")
g.add_edge("write", "send")
g.add_edge("send", END)
app = g.compile(checkpointer=SqliteSaver(sqlite3.connect(":memory:", check_same_thread=False)))
print(app.invoke({"customer": "ana@exemplo.org", "body": ""}, {"configurable": {"thread_id": "1"}}))
