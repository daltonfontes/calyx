# Bug 00: example of the format (not one of the 54): the e-mail goes out twice.
# Damage: the second call to store.email
from world import Store

store = Store()


def notify(customer: str) -> None:
    store.email(customer, "Your order", "It arrived!")


notify("ana@example.org")
notify("ana@example.org")
print(f"e-mails: {len(store.outbox)}")
