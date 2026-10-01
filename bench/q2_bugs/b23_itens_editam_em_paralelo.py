# Bug 23: each item of a fan-out edits the same repository at the same time:
# edits overwrite each other.
# Calyx: E0644 (compiler)
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langgraph.types import Send
from world import drop_repo, edit_file, new_repo, run_tests

repo = new_repo()


class State(TypedDict):
    fixes: list[str]


class Item(TypedDict):
    fix: str


def fan_out(state: State) -> list[Send]:
    return [Send("apply", {"fix": f}) for f in state["fixes"]]


def apply(state: Item) -> dict[str, str]:
    edit_file(repo, "calc.py", state["fix"])
    return {}


g = StateGraph(State)
g.add_node("apply", apply)
g.add_conditional_edges(START, fan_out, ["apply"])
g.add_edge("apply", END)
g.compile().invoke({"fixes": ["x = 2\n", "x = 3\n", "x = 4\n"]})
print(run_tests(repo))
drop_repo(repo)
