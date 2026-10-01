"""W2 in plain Python, no checkpoint: after a crash, the only way to go on
is to run it again from the start.

    python naive.py <request> <order> <message> [--careful]
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import flow  # noqa: E402

request, order, message = sys.argv[1:4]
careful = "--careful" in sys.argv
s: flow.State = {"request": request, "order_id": order, "message": message}
for step in (flow.get_order, flow.decide, flow.refund, flow.reply, flow.email):
    if step in (flow.refund, flow.email):
        s.update(step(s, careful))
    else:
        s.update(step(s))
print(s["result"])
