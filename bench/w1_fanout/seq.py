"""W1 in plain Python: one question at a time."""
import json
import sys

sys.path.insert(0, __file__.rsplit("/", 2)[0])
from common.fakes import llm, search  # noqa: E402


def research(questions: list[str]) -> str:
    answers = [llm(f"Responda {q} usando {search(q)}") for q in questions]
    return llm(f"Escreva um relatório com {answers}")


print(research(json.loads(open(sys.argv[1][1:]).read() if sys.argv[1].startswith("@") else sys.argv[1])))
