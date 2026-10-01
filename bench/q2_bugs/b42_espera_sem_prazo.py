# Bug 42: the run waits for an approval with no deadline: if nobody answers,
# it stays paused forever, holding what it reserved.
# Calyx: E0671 (compiler)
import sqlite3
from typing import TypedDict

from langgraph.checkpoint.sqlite import SqliteSaver
from langgraph.graph import END, START, StateGraph
from langgraph.types import interrupt


class State(TypedDict):
    request: str
    approved: bool


def ask(state: State) -> dict[str, bool]:
    answer = interrupt({"approve": state["request"]})  # no deadline exists
    return {"approved": bool(answer)}


g = StateGraph(State)
g.add_node("ask", ask)
g.add_edge(START, "ask")
g.add_edge("ask", END)
app = g.compile(checkpointer=SqliteSaver(sqlite3.connect(":memory:", check_same_thread=False)))
print(app.invoke({"request": "reembolso", "approved": False}, {"configurable": {"thread_id": "1"}}))
