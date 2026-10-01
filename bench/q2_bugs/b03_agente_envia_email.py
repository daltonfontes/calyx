# Bug 03: an agent holds an e-mail tool and may call it again every turn
# (duplicate e-mails). The fake model asks for the tool on every turn.
# Calyx: E0640 (compiler)
from typing import Annotated, TypedDict

from langchain_core.messages import AIMessage, AnyMessage, HumanMessage
from langchain_core.tools import tool
from langgraph.graph import END, START, StateGraph
from langgraph.graph.message import add_messages
from langgraph.prebuilt import ToolNode
from world import Store

store = Store()


@tool
def send_email(to: str, body: str) -> str:
    """Sends an e-mail."""
    store.email(to, "Atualização", body)
    return "enviado"


class State(TypedDict):
    messages: Annotated[list[AnyMessage], add_messages]


def model(state: State) -> dict[str, list[AnyMessage]]:
    n = len(state["messages"])
    call = {"name": "send_email", "args": {"to": "ana@exemplo.org", "body": "oi"}, "id": f"c{n}"}
    return {"messages": [AIMessage(content="", tool_calls=[call])]}


def route(state: State) -> str:
    last = state["messages"][-1]
    return "tools" if isinstance(last, AIMessage) and last.tool_calls else END


g = StateGraph(State)
g.add_node("model", model)
g.add_node("tools", ToolNode([send_email]))
g.add_edge(START, "model")
g.add_conditional_edges("model", route, ["tools", END])
g.add_edge("tools", "model")
try:
    g.compile().invoke({"messages": [HumanMessage("avise a cliente")]}, {"recursion_limit": 12})
finally:
    print(f"e-mails enviados: {len(store.outbox)}")
