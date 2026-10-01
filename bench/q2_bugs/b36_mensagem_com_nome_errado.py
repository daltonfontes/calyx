# Bug 36: a message the entity does not handle (a typo): the deposit is lost.
# Messages to an actor are usually dispatched by name.
# Calyx: E0655 (compiler)
from world import Account

account = Account()


def send(target: Account, message: str, *args: float) -> None:
    handler = getattr(target, message, None)
    if handler is not None:
        handler(*args)


send(account, "deposti", 10.0)
print(f"saldo: {account.balance} (esperado 10.0)")
