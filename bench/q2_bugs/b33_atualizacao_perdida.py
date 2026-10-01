# Bug 33: the run reads the balance, adds and writes the result back: two
# runs at the same time lose a deposit.
# Calyx: W0603 (compiler)
import threading
import time

from world import Account

account = Account()


def deposit_run(amount: float) -> None:
    balance = account.get_balance()
    time.sleep(0.01)  # e.g. a model call between the read and the write
    account.set_balance(balance + amount)


runs = [threading.Thread(target=deposit_run, args=(10.0,)) for _ in range(2)]
for t in runs:
    t.start()
for t in runs:
    t.join()
print(f"saldo: {account.balance} (esperado 20.0)")
