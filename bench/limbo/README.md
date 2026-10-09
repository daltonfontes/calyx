# A Calyx no LIMBO

[LIMBO](https://github.com/jaxblack/limbo-bench) ("Where Does Exactly-Once
Live?", 2026) mede efeitos duplicados em agentes: 6 serviços simulados, 12
tarefas, modos de falha injetados no serviço (timeout antes e depois do
commit, commit atrasado, erro 500, lote pela metade, reentrega) e um
livro-razão do que cada serviço de fato fez. É um benchmark de outros
autores, com avaliador próprio.

Aqui as 12 tarefas viram programas Calyx e rodam no ambiente do LIMBO, com o
injetor de falhas e o avaliador dele, sem mudança nenhuma no código do LIMBO.

## Arquivos

| Arquivo | O que é |
|---|---|
| `programs/tools_native.clyx` | As tools declaradas lendo só a documentação delas, no contrato `native` (só a cobrança e o mastodon aceitam chave) |
| `programs/tools_keys.clyx` | O mesmo no contrato `keys_everywhere` (toda escrita aceita chave) |
| `programs/tasks.clyx` | As 12 tarefas, iguais nos dois contratos |
| `adapter.py` | O servidor MCP das tools: repassa cada chamada ao ambiente do LIMBO e implementa as leituras usadas pelo `verify` |
| `run_limbo.py` | Monta cada episódio com as classes do LIMBO, roda a Calyx e dá a nota com o avaliador do LIMBO |
| `summarize.py` | As tabelas, com os mesmos filtros do relatório do LIMBO |
| `declarations.py` | Declara cada tool de cada forma possível e confere com `calyx check --tools` o que as anotações MCP do LIMBO pegam |

## Rodar

```sh
git clone https://github.com/jaxblack/limbo-bench /tmp/limbo && git -C /tmp/limbo checkout 2db09fb
cargo build --release
python3 bench/limbo/run_limbo.py --limbo /tmp/limbo                              # contrato native
python3 bench/limbo/run_limbo.py --limbo /tmp/limbo --contract keys_everywhere
python3 bench/limbo/summarize.py
python3 bench/limbo/declarations.py --limbo /tmp/limbo
```

Os modos de falha são os da grade E2 do LIMBO: 12 tarefas, cada escrita
focal, 8 modos, instância 0. Não há modelo: o que é medido é a camada de
execução.

## O que o adaptador faz, e só isso

- **Transporte.** Um timeout ou um 5xx do serviço volta para a Calyx como
  erro com texto começando por `Timeout:` ou `Unavailable:`, a convenção de
  um servidor MCP na frente de outro serviço. A chave que a Calyx manda no
  `_meta` vai para o serviço como `idempotency_key`.
- **As leituras do `verify`.** Cada uma procura a chamada com as tools de
  leitura públicas do LIMBO e devolve o que acha. Onde a documentação diz
  que a listagem atrasa (weibo, até 3 min; pasta de enviados, até 2 min),
  ela espera esse tempo antes, no relógio simulado.
- **`refund`.** A documentação diz que reembolsar de novo responde 409; essa
  resposta quer dizer que o reembolso está feito.
- **Anotações.** O `tools/list` repassa as anotações MCP que o LIMBO dá à
  tool que cada uma chama (`readOnlyHint`, `idempotentHint`).

Quando uma `write once` com `on_uncertain pause` para a execução, o
executor faz o papel da pessoa: pergunta ao operador do LIMBO
(`escalate_to_human`, que custa 15 min simulados, como para os agentes) e
retoma com `--uncertain done`, `retry` ou `failed`, conforme o que ele
achou.

Resultados e leitura: [`docs/evaluation/limbo.md`](../../docs/evaluation/limbo.md).
