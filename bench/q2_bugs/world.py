"""The world the Q2 programs act on, typed so pyright and mypy can check
every call: a store with payments and e-mail, accounts (the entity of the
Calyx versions), a repository and a model. All fake and instant.

Each program has one bug from tests/state_bugs/ (same number), written the
way a careful Python programmer would: typed state, typed functions.
"""
import os
import shutil
import tempfile
from dataclasses import dataclass


@dataclass
class Refund:
    id: str
    amount: float


class Store:
    def __init__(self) -> None:
        self.payments: list[tuple[str, float]] = []
        self.keys: dict[str, Refund] = {}
        self.outbox: list[tuple[str, str]] = []
        self.orders = {"A100": {"status": "Delivered", "total": 300.0, "refunded": 0.0}}

    def get_order(self, order: str) -> dict[str, object]:
        return dict(self.orders[order])

    def refund(self, order: str, amount: float, key: str | None = None) -> Refund | None:
        """Pays; `None` when the payment provider declines. The same `key`
        never pays twice (the provider's idempotency key)."""
        if key is not None and key in self.keys:
            return self.keys[key]
        self.payments.append((order, amount))
        done = Refund(id=f"rf-{len(self.payments)}", amount=amount)
        if key is not None:
            self.keys[key] = done
        return done

    def cancel(self, order: str) -> None:
        self.orders[order]["status"] = "Cancelled"

    def email(self, to: str, subject: str, body: str) -> None:
        self.outbox.append((to, subject))


class Account:
    """An account: what the Calyx versions declare as an `entity`."""

    def __init__(self) -> None:
        self.balance = 0.0

    def deposit(self, amount: float) -> None:
        self.balance += amount

    def get_balance(self) -> float:
        return self.balance

    def set_balance(self, value: float) -> None:
        self.balance = value


def llm(prompt: str) -> str:
    return f"[resposta falsa para: {prompt[:40]}]"


def new_repo() -> str:
    d = tempfile.mkdtemp(prefix="q2-repo-")
    with open(os.path.join(d, "calc.py"), "w") as f:
        f.write("x = 1\n")
    return d


def edit_file(repo: str, path: str, text: str) -> None:
    with open(os.path.join(repo, path), "a") as f:
        f.write(text)


def run_tests(repo: str) -> str:
    return open(os.path.join(repo, "calc.py")).read()


def drop_repo(repo: str) -> None:
    shutil.rmtree(repo, ignore_errors=True)
