# Bug 17: the result of a refund that may fail is used as if it succeeded.
# Store.refund returns `Refund | None`.
# Calyx: E0608 (compiler)
from world import Store

store = Store()


def refund_and_confirm(order: str) -> str:
    r = store.refund(order, 50.0)
    return f"reembolso {r.id} de {r.amount}"


print(refund_and_confirm("A100"))
