# Comparação: Calyx × Python × LangGraph × Temporal

Os mesmos workflows escritos em Calyx e em Python (sequencial, asyncio,
LangGraph 1.2 e Temporal), rodando contra o mesmo mundo falso. Os resultados e a
discussão estão em [`docs/evaluation/comparacao.md`](../docs/evaluation/comparacao.md);
o plano de avaliação para o paper, em
[`docs/evaluation/plano-paper.md`](../docs/evaluation/plano-paper.md).

| Pasta | O que mede | Pergunta |
|---|---|---|
| `w1_fanout/` | N perguntas → busca + resumo de cada → relatório. Tempo com modelo de 1 s por chamada (N = 5, 20, 50) e custo por item sem latência (N = 10 a 10.000) | Q1 |
| `w2_recovery/` | Reembolso (pedido → decisão → pagamento → resposta → e-mail) contra a loja falsa (MCP), em Calyx, LangGraph, Temporal e Python. O processo morre em 6 pontos (depois de cada passo registrado e com cada efeito em andamento) e é retomado; contamos pagamentos, e-mails e chamadas de modelo | Q3 |
| `w3_approval/` | Reembolso com aprovação humana e prazo de 3 s, em Calyx (`receive`), LangGraph (`interrupt()`) e Temporal (*signal* com timeout). 6 cenários com o processo parado durante a espera: resposta em dobro, atrasada, no prazo com retomada depois | Q3 |
| `q2_bugs/` | 16 bugs do corpus Q2 (`tests/state_bugs/`) em Python + LangGraph, com tipos. Onde cada um aparece: pyright, mypy, ao rodar, ou em lugar nenhum | Q2 |
| `real_bugs/` | 79 issues públicas de LangGraph, CrewAI e AutoGen, classificadas pelo que a Calyx faria (`classify.py`), e os programas que conferem os casos a favor dela (`run_verify.py`). Resultados em [`docs/evaluation/bugs-reais.md`](../docs/evaluation/bugs-reais.md) | Q2 |
| `common/fakes.py` | O que as versões em Python dividem com as da Calyx: o modelo falso (mesma latência, mesma resposta), a busca falsa e um cliente MCP para a mesma loja | — |

## Como rodar

```sh
make build                                   # o calyx em target/release
python3 -m venv /tmp/lg && /tmp/lg/bin/pip install langgraph langgraph-checkpoint-sqlite temporalio pyright mypy
export BENCH_PYTHON=/tmp/lg/bin/python
export TEMPORAL_BIN=$(which temporal)        # a CLI do Temporal; sem ela, a W2 roda sem o Temporal

python3 bench/run_w1.py      # ~10 min; bench/results/w1.json  (--quick: ~1 min)
python3 bench/run_w2.py      # ~12 min; bench/results/w2.json (sobe o servidor do Temporal sozinho)
python3 bench/run_w3.py      # ~2 min;  bench/results/w3.json (sobe o servidor do Temporal sozinho)
python3 bench/run_q2.py      # ~1 min;  bench/results/q2.json
python3 bench/real_bugs/classify.py      # contagens do estudo de bugs reais
python3 bench/real_bugs/run_verify.py    # ~5 s; bench/results/real_bugs_verify.json
python3 bench/loc.py bench/w1_fanout/* bench/w2_recovery/*   # linhas de código
```

Versões usadas nos resultados publicados: Python 3.11.15, langgraph 1.2.12,
langchain-core 1.6.6, langgraph-checkpoint-sqlite 3.1.1, temporalio 1.34.0,
CLI do Temporal 1.9.1 (servidor 1.32.0), pyright 1.1.414, mypy 2.3.1,
Calyx 0.2.0.

A CLI do Temporal normalmente vem de `temporal.download` ou das releases no
GitHub. Onde a rede não deixa, dá para compilá-la pelo proxy de módulos do
Go: baixar `github.com/temporalio/cli@v1.9.1` e o submódulo
`github.com/temporalio/cli/cliext` com `go mod download`, colocar o segundo
em `cliext/` dentro do primeiro e rodar `go build ./cmd/temporal`.

## Regras para ser justo

- **Mesmo mundo:** o modelo falso das duas linguagens espera a mesma latência
  e responde o mesmo texto; a loja da W2 é o mesmo servidor MCP
  (`examples/tools/fake_store.py`), com o mesmo estado em JSON.
- **Mesmo limite:** no máximo 8 chamadas ao mesmo tempo em todas as versões
  paralelas (`limits threads 8`, `Semaphore(8)`, `max_concurrency=8`).
- **Python no melhor caso:** estado tipado (`TypedDict`), funções anotadas,
  pyright e mypy. A W2 também roda LangGraph com `durability="sync"`, o
  Temporal, e todos com o "cuidado manual" que a documentação recomenda e
  um programador atento escreveria (chave de idempotência, conferir se o
  e-mail já saiu).
- **Quem escreveu:** o mesmo autor escreveu as versões nas duas linguagens.
  É a maior ameaça à validade; o plano do paper pede versões escritas por
  outras pessoas.
