"""W3 in Temporal (Python SDK): the wait is a signal with a deadline,
`workflow.wait_condition(..., timeout=3 s)`, as the Temporal docs show. The
timer lives in the server, so it runs out even with no worker; the next
worker sees, in the workflow's history, whether the answer or the timer
came first.

- `start`: starts the workflow and runs a worker until the workflow waits.
- `deliver <answer>`: sends the signal (the server keeps it; no worker needed).
  The server refuses it when the workflow already finished.
- `tick`: runs a worker until the workflow finishes, or gives up after 10 s
  if it is still waiting.

When no worker runs at the deadline, the timer and a late signal can reach
the next worker together, and the SDK hands signals to the workflow before
timers: the late answer counts. `--careful` adds what a careful programmer
writes by hand: the sender stamps the time it answered, the workflow writes
down its deadline when it starts waiting, and takes an answer stamped after
it as no answer.

    python temporal_.py start|deliver|tick <workflow id> [answer] [--careful]

The server address comes from TEMPORAL_ADDRESS (default 127.0.0.1:7299).
"""
import asyncio
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta

from temporalio import activity, workflow
from temporalio.client import Client
from temporalio.common import RetryPolicy
from temporalio.service import RPCError
from temporalio.worker import UnsandboxedWorkflowRunner, Worker

CAREFUL = "--careful" in sys.argv

with workflow.unsafe.imports_passed_through():
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import flow


@activity.defn
def get_order(s: dict) -> dict:
    return dict(flow.get_order(s))


@activity.defn
def decide(s: dict) -> dict:
    return dict(flow.decide(s))


@activity.defn
def pay(s: dict) -> dict:
    return dict(flow.pay(s))


@activity.defn
def refuse(s: dict) -> dict:
    return dict(flow.refuse(s, s["reason"]))


@workflow.defn
class ApprovalFlow:
    def __init__(self) -> None:
        self.answer: str | None = None
        self.answered_at = 0.0
        self.stage = "starting"

    @workflow.signal
    def answer_signal(self, got: dict) -> None:
        self.answer, self.answered_at = got["answer"], got["at"]

    @workflow.query
    def current_stage(self) -> str:
        return self.stage

    @workflow.run
    async def run(self, s: dict) -> str:
        opts = {
            "start_to_close_timeout": timedelta(seconds=10),
            "retry_policy": RetryPolicy(maximum_attempts=5),
        }
        for step in (get_order, decide):
            s.update(await workflow.execute_activity(step, s, **opts))
        self.stage = "waiting"
        deadline = workflow.now().timestamp() + flow.DEADLINE_S
        try:
            await workflow.wait_condition(lambda: self.answer is not None,
                                          timeout=timedelta(seconds=flow.DEADLINE_S))
        except asyncio.TimeoutError:
            self.answer = "Timeout"
        if CAREFUL and self.answer != "Timeout" and self.answered_at > deadline:
            self.answer = "Timeout"
        self.stage = "deciding"
        if self.answer == "Approved":
            s.update(await workflow.execute_activity(pay, s, **opts))
        else:
            reason = "sem resposta no prazo" if self.answer == "Timeout" else "recusado"
            s.update(await workflow.execute_activity(refuse, {**s, "reason": reason}, **opts))
        return s["result"]


async def main() -> None:
    mode, wid = sys.argv[1], sys.argv[2]
    client = await Client.connect(os.environ.get("TEMPORAL_ADDRESS", "127.0.0.1:7299"))
    handle = client.get_workflow_handle(wid)
    if mode == "deliver":
        try:
            await handle.signal(ApprovalFlow.answer_signal,
                                {"answer": sys.argv[3], "at": time.time()})
        except RPCError as e:
            sys.exit(f"not delivered: {e}")
        return
    async with Worker(
        client,
        task_queue=wid,
        workflows=[ApprovalFlow],
        activities=[get_order, decide, pay, refuse],
        activity_executor=ThreadPoolExecutor(4),
        workflow_runner=UnsandboxedWorkflowRunner(),
    ):
        if mode == "start":
            start = {"request": "R1", "order_id": "A100", "message": "chegou quebrado"}
            handle = await client.start_workflow(ApprovalFlow.run, start, id=wid, task_queue=wid)
            while await handle.query(ApprovalFlow.current_stage) != "waiting":
                await asyncio.sleep(0.05)
        else:
            try:
                print(await asyncio.wait_for(handle.result(), timeout=10))
            except asyncio.TimeoutError:
                print("still waiting")


if __name__ == "__main__":
    asyncio.run(main())
