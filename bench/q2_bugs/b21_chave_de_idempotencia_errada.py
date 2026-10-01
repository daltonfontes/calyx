# Bug 21: the idempotency key is the order, not the request: a second,
# legitimate refund of the same order is dropped in silence.
# Calyx: none (nobody catches it: the key is the programmer's choice)
from world import Store

store = Store()


def refund(request: str, order: str, amount: float) -> None:
    store.refund(order, amount, key=order)  # should be key=request


refund("R1", "A100", 30.0)
refund("R2", "A100", 20.0)
print(f"pagamentos: {len(store.payments)} (esperado 2)")
