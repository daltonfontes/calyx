"""W1 in asyncio, written by hand: gather, with at most 8 calls at once."""
import asyncio
import json
import sys

sys.path.insert(0, __file__.rsplit("/", 2)[0])
from common.fakes import allm, search  # noqa: E402


async def research(questions: list[str]) -> str:
    limit = asyncio.Semaphore(8)

    async def answer(q: str) -> str:
        async with limit:
            return await allm(f"Responda {q} usando {search(q)}")

    answers = await asyncio.gather(*(answer(q) for q in questions))
    async with limit:
        return await allm(f"Escreva um relatório com {list(answers)}")


print(asyncio.run(research(json.loads(sys.argv[1]))))
