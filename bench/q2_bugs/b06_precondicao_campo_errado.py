# Bug 06: a precondition on a field the state does not have (a typo): it
# would never hold. Here the order is a TypedDict, the best case for Python.
# Calyx: E0603 (compiler)
from typing import TypedDict

from world import Store

store = Store()


class Order(TypedDict):
    status: str
    total: float
    refunded: float


def refund_if_allowed(order: Order, amount: float) -> bool:
    if order["status"] == "Delivered" and order["refundd"] + amount <= order["total"]:
        store.refund("A100", amount)
        return True
    return False


print(refund_if_allowed({"status": "Delivered", "total": 300.0, "refunded": 0.0}, 50.0))
