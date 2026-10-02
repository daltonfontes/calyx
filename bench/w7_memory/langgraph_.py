"""W7 in LangGraph: the user's memory in LangGraph's Store (SQLite), the way
the LangGraph docs show it: read the memories, answer, extract facts, then
`store.get` + append + `store.put`. Runs are separate processes, each with
its own thread in a SQLite checkpointer.

`--careful` adds what a careful programmer writes by hand: no
read-modify-write; each fact is its own item, keyed by the message id, so
two runs never overwrite each other and a repeated message writes the same
keys again. The count of conversations is the number of messages seen.

BENCH_CRASH_IN_SEND=1 makes the process die right after the memory is
written, before the node's checkpoint: `resume` continues the thread.

    python langgraph_.py run|resume <message id> <user> <text> [--careful]
    python langgraph_.py dump - <user> - [--careful]    # the memory, as JSON
    python langgraph_.py setup - - -                    # creates the tables
"""
import os
import sqlite3
import sys
from typing import TypedDict

from langgraph.checkpoint.sqlite import SqliteSaver
from langgraph.graph import END, START, StateGraph
from langgraph.store.sqlite import SqliteStore

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from common.fakes import llm  # noqa: E402

args = [a for a in sys.argv[1:] if not a.startswith("--")]
mode, message_id, user, text = args[:4]
careful = "--careful" in sys.argv


class State(TypedDict, total=False):
    user: str
    text: str
    memories: list
    reply: str
    learned: list


def connect(path: str) -> sqlite3.Connection:
    conn = sqlite3.connect(path, check_same_thread=False, isolation_level=None, timeout=30)
    conn.execute("PRAGMA journal_mode=WAL")
    return conn


store = SqliteStore(connect(os.environ["BENCH_STORE"]))
if mode == "setup":  # once, before the runs start: they would race to create the tables
    store.setup()
    SqliteSaver(connect(os.environ["BENCH_CHECKPOINT"])).setup()
    sys.exit(0)


def recall(s: State) -> State:
    if careful:
        return {"memories": [i.value for i in store.search(("memory", s["user"], "facts"), limit=1000)]}
    item = store.get(("memory",), s["user"])
    return {"memories": item.value["facts"] if item else []}


def reply(s: State) -> State:
    return {"reply": llm(f"Responda: {s['text']}\nFatos conhecidos: {s['memories']}")}


def learn(s: State) -> State:
    llm(f"Fatos de: {s['text']}")
    # What the fake model answers in Calyx too: three facts from the text.
    return {"learned": [{"topic": f"item falso {i + 1} (Fatos de: {s['text']})",
                         "content": f"item falso {i + 1} (Fatos de: {s['text']})"} for i in range(3)]}


def remember(s: State) -> State:
    if careful:
        for i, fact in enumerate(s["learned"]):
            store.put(("memory", s["user"], "facts"), f"{message_id}:{i}", fact)
        store.put(("memory", s["user"], "conversations"), message_id, {"seen": True})
    else:
        item = store.get(("memory",), s["user"])
        mem = item.value if item else {"facts": [], "conversations": 0}
        store.put(("memory",), s["user"], {"facts": mem["facts"] + s["learned"],
                                           "conversations": mem["conversations"] + 1})
    if os.environ.get("BENCH_CRASH_IN_SEND"):
        os._exit(137)
    return {}


if mode == "dump":
    import json

    if careful:
        facts = [i.value for i in store.search(("memory", user, "facts"), limit=100000)]
        conv = len(store.search(("memory", user, "conversations"), limit=100000))
    else:
        item = store.get(("memory",), user)
        facts, conv = (item.value["facts"], item.value["conversations"]) if item else ([], 0)
    print(json.dumps({"facts": facts, "conversations": conv}))
    sys.exit(0)

graph = StateGraph(State)
for name, fn in [("recall", recall), ("reply", reply), ("learn", learn), ("remember", remember)]:
    graph.add_node(name, fn)
graph.add_edge(START, "recall")
graph.add_edge("recall", "reply")
graph.add_edge("reply", "learn")
graph.add_edge("learn", "remember")
graph.add_edge("remember", END)

app = graph.compile(checkpointer=SqliteSaver(connect(os.environ["BENCH_CHECKPOINT"])))
config = {"configurable": {"thread_id": message_id}}
start = {"user": user, "text": text} if mode == "run" else None
app.invoke(start, config, durability="sync")
