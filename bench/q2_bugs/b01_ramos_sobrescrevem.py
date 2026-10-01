# Bug 01: two parallel branches write the same key; the last one to finish wins.
# Calyx: E0501 (compiler)
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from world import llm


class State(TypedDict):
    topic: str
    summary: str


def short(state: State) -> dict[str, str]:
    return {"summary": llm(f"resumo curto de {state['topic']}")}


def long(state: State) -> dict[str, str]:
    return {"summary": llm(f"resumo longo de {state['topic']}")}


g = StateGraph(State)
g.add_node("short", short)
g.add_node("long", long)
g.add_edge(START, "short")
g.add_edge(START, "long")
g.add_edge("short", END)
g.add_edge("long", END)
print(g.compile().invoke({"topic": "baterias", "summary": ""}))
