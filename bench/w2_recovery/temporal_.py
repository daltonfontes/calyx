"""W2 in Temporal (Python SDK): each step an activity, the flow a workflow.
The workflow's history lives in the Temporal server, so when this process
(the worker) dies, the run survives; `resume` starts a new worker, which
picks the workflow up where its history ends. Activities that were running
when the worker died are retried after their `start_to_close_timeout`.

`--careful` adds what the Temporal docs recommend for activities with side
effects: an idempotency key for the payment, and checking before sending
the e-mail again.

    python temporal_.py run|resume <id> <request> <order> <message> [--careful]

The server address comes from TEMPORAL_ADDRESS (default 127.0.0.1:7299).
"""
import asyncio
import os
import sys
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta

from temporalio import activity, workflow
from temporalio.client import Client
from temporalio.common import RetryPolicy
from temporalio.worker import UnsandboxedWorkflowRunner, Worker

with workflow.unsafe.imports_passed_through():
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import flow

CAREFUL = "--careful" in sys.argv


@activity.defn
def get_order(s: dict) -> dict:
    return dict(flow.get_order(s))


@activity.defn
def decide(s: dict) -> dict:
    return dict(flow.decide(s))


@activity.defn
def refund(s: dict) -> dict:
    return dict(flow.refund(s, careful=CAREFUL))


@activity.defn
def reply(s: dict) -> dict:
    return dict(flow.reply(s))


@activity.defn
def email(s: dict) -> dict:
    return dict(flow.email(s, careful=CAREFUL))


@workflow.defn
class RefundFlow:
    @workflow.run
    async def run(self, s: dict) -> str:
        opts = {
            "start_to_close_timeout": timedelta(seconds=10),
            "retry_policy": RetryPolicy(
                initial_interval=timedelta(milliseconds=200), maximum_attempts=5
            ),
        }
        for step in (get_order, decide, refund, reply, email):
            s.update(await workflow.execute_activity(step, s, **opts))
        return s["result"]


async def main() -> None:
    mode, wid, request, order, message = sys.argv[1:6]
    client = await Client.connect(os.environ.get("TEMPORAL_ADDRESS", "127.0.0.1:7299"))
    async with Worker(
        client,
        task_queue=wid,
        workflows=[RefundFlow],
        activities=[get_order, decide, refund, reply, email],
        activity_executor=ThreadPoolExecutor(4),
        # The sandbox re-imports this module (and the store client in flow.py);
        # it guards workflow determinism, not durability, which is what we measure.
        workflow_runner=UnsandboxedWorkflowRunner(),
    ):
        if mode == "run":
            start = {"request": request, "order_id": order, "message": message}
            result = await client.execute_workflow(RefundFlow.run, start, id=wid, task_queue=wid)
        else:
            result = await client.get_workflow_handle(wid).result()
    print(result)


if __name__ == "__main__":
    asyncio.run(main())
