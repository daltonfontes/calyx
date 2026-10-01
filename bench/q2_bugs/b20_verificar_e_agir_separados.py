# Bug 20: the status is checked with a read and the refund comes later, with
# no condition checked when paying: the order is cancelled in between.
# Calyx: none (nobody catches it: `requires` is the fix, and it is optional)
from world import Store, llm

store = Store()


def handle(order: str) -> str:
    if store.get_order(order)["status"] != "Delivered":
        return "não reembolsável"
    decision = llm(f"quanto reembolsar de {order}?")
    store.cancel(order)  # another process, while the model thinks
    store.refund(order, 50.0)
    return f"reembolsado: {decision}"


print(handle("A100"), f"status final: {store.orders['A100']['status']}")
