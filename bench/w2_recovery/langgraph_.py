"""W2 in LangGraph with a durable checkpointer (SQLite): one node per step,
a checkpoint after each. `resume` continues the thread from its last
checkpoint, as LangGraph documents. By default LangGraph writes checkpoints
while the next step runs (`durability="async"`); `--sync` writes each one
before the next step starts.

    python langgraph_.py run|resume <thread> <request> <order> <message> [--careful] [--sync]
"""
import os
import sqlite3
import sys
from functools import partial

from langgraph.checkpoint.sqlite import SqliteSaver
from langgraph.graph import END, START, StateGraph

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import flow  # noqa: E402

mode, thread, request, order, message = sys.argv[1:6]
careful = "--careful" in sys.argv
durability = "sync" if "--sync" in sys.argv else "async"  # "async" is LangGraph's default

graph = StateGraph(flow.State)
graph.add_node("get_order", flow.get_order)
graph.add_node("decide", flow.decide)
graph.add_node("refund", partial(flow.refund, careful=careful))
graph.add_node("reply", flow.reply)
graph.add_node("email", partial(flow.email, careful=careful))
graph.add_edge(START, "get_order")
graph.add_edge("get_order", "decide")
graph.add_edge("decide", "refund")
graph.add_edge("refund", "reply")
graph.add_edge("reply", "email")
graph.add_edge("email", END)

db = sqlite3.connect(os.environ["BENCH_CHECKPOINT"], check_same_thread=False)
app = graph.compile(checkpointer=SqliteSaver(db))
config = {"configurable": {"thread_id": thread}}
start = {"request": request, "order_id": order, "message": message} if mode == "run" else None
print(app.invoke(start, config, durability=durability)["result"])
