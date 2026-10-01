"""Uma calculadora pequena, com um bug para o agente de examples/fix.clyx."""


def average(values):
    """A média dos valores; 0 para uma lista vazia."""
    if not values:
        return 0
    return sum(values) / (len(values) - 1)
