"""W1 in LangGraph: Send for the fan-out, a reducer to gather the answers."""
import json
import operator
import sys
from typing import Annotated, TypedDict

from langgraph.graph import END, START, StateGraph
from langgraph.types import Send

sys.path.insert(0, __file__.rsplit("/", 2)[0])
from common.fakes import llm, search  # noqa: E402


class State(TypedDict):
    questions: list[str]
    answers: Annotated[list[str], operator.add]
    report: str


class Item(TypedDict):
    q: str


def fan_out(state: State) -> list[Send]:
    return [Send("answer", {"q": q}) for q in state["questions"]]


def answer(item: Item) -> dict:
    return {"answers": [llm(f"Responda {item['q']} usando {search(item['q'])}")]}


def report(state: State) -> dict:
    return {"report": llm(f"Escreva um relatório com {state['answers']}")}


graph = StateGraph(State)
graph.add_node("answer", answer)
graph.add_node("report", report)
graph.add_conditional_edges(START, fan_out, ["answer"])
graph.add_edge("answer", "report")
graph.add_edge("report", END)
app = graph.compile()

questions = json.loads(open(sys.argv[1][1:]).read() if sys.argv[1].startswith("@") else sys.argv[1])
out = app.invoke(
    {"questions": questions, "answers": [], "report": ""},
    config={"max_concurrency": 8, "recursion_limit": len(questions) + 10},
)
print(out["report"])
