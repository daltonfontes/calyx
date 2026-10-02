"""W3 in LangGraph: the wait is `interrupt()` in its own node, as the
LangGraph docs recommend (a node that is resumed runs again from its start,
so the model call goes in the node before). The checkpoint is SQLite.

- `start`: runs until the interrupt; the process ends there.
- `deliver <answer>`: what the app does when the person answers: resumes the
  thread with `Command(resume=answer)` (this runs the rest of the graph).
- `tick`: what the app does on a schedule. LangGraph has no deadline for an
  interrupt, so without `--careful` there is nothing to do.

`--careful` adds what a careful programmer writes by hand: the deadline
computed once and kept in the state; `deliver` refuses an answer when the
thread is no longer waiting, and stamps the time the answer arrived; the
node takes an answer that arrived after the deadline as no answer; and
`tick` resumes waiting threads whose deadline passed.

    python langgraph_.py start|deliver|tick <thread> [answer] [--careful]
"""
import os
import sqlite3
import sys
import time

from langgraph.checkpoint.sqlite import SqliteSaver
from langgraph.graph import END, START, StateGraph
from langgraph.types import Command, interrupt

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import flow  # noqa: E402

args = [a for a in sys.argv[1:] if not a.startswith("--")]
careful = "--careful" in sys.argv
mode, thread = args[0], args[1]


def decide(s: flow.State) -> flow.State:
    out = flow.decide(s)
    if careful:
        out["deadline"] = time.time() + flow.DEADLINE_S
    return out


def wait_answer(s: flow.State) -> flow.State:
    got = interrupt({"amount": s["amount"]})
    if careful:
        late = got["at"] is None or got["at"] > s["deadline"]
        return {"answer": "Timeout" if late else got["answer"]}
    return {"answer": got}


def finish(s: flow.State) -> flow.State:
    if s["answer"] == "Approved":
        return flow.pay(s)
    reason = "sem resposta no prazo" if s["answer"] == "Timeout" else "recusado"
    return flow.refuse(s, reason)


graph = StateGraph(flow.State)
graph.add_node("get_order", flow.get_order)
graph.add_node("decide", decide)
graph.add_node("wait_answer", wait_answer)
graph.add_node("finish", finish)
graph.add_edge(START, "get_order")
graph.add_edge("get_order", "decide")
graph.add_edge("decide", "wait_answer")
graph.add_edge("wait_answer", "finish")
graph.add_edge("finish", END)

db = sqlite3.connect(os.environ["BENCH_CHECKPOINT"], check_same_thread=False)
app = graph.compile(checkpointer=SqliteSaver(db))
config = {"configurable": {"thread_id": thread}}
waiting = bool(app.get_state(config).next)

if mode == "start":
    app.invoke({"request": "R1", "order_id": "A100", "message": "chegou quebrado"}, config)
elif mode == "deliver":
    answer = args[2]
    if careful and not waiting:
        sys.exit("not waiting")
    resume = {"answer": answer, "at": time.time()} if careful else answer
    print(app.invoke(Command(resume=resume), config).get("result"))
elif mode == "tick":
    if careful and waiting and time.time() > app.get_state(config).values["deadline"]:
        print(app.invoke(Command(resume={"answer": None, "at": None}), config).get("result"))
