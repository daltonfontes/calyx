# Bug 52: the race's condition calls a model to judge every answer: one more
# paid call per branch, and a result that changes from run to run.
# Calyx: E0682 (compiler)
import asyncio

from world import llm


async def strategy(name: str) -> str:
    await asyncio.sleep(0.01)
    return llm(f"resolva com a estratégia {name}")


def good(answer: str) -> bool:
    return llm(f"esta resposta é boa? {answer}").startswith("[")


async def race() -> str:
    for done in asyncio.as_completed([strategy("rápida"), strategy("cuidadosa")]):
        answer = await done
        if good(answer):
            return answer
    raise RuntimeError("nenhuma")


print(asyncio.run(race()))
