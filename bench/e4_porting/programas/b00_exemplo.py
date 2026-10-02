# Bug 00: exemplo do formato (não faz parte dos 54): o e-mail sai duas vezes.
# Dano: a segunda chamada a store.email
from world import Store

store = Store()


def notify(customer: str) -> None:
    store.email(customer, "Seu pedido", "Chegou!")


notify("ana@exemplo.org")
notify("ana@exemplo.org")
print(f"e-mails: {len(store.outbox)}")
