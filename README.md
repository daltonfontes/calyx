# Calyx

Calyx é uma linguagem de programação **graph-native** para agentes de IA.

## O problema

> **Colocar agentes de IA em produção exige escrever à mão uma infraestrutura difícil (paralelismo, retentativas, recuperação de falhas, consistência de estado, controle de custo, rastreamento), e é exatamente nesse código escrito à mão que os agentes quebram.**

Um agente real faz dezenas ou centenas de chamadas de LLM e de tools, muitas em paralelo, às vezes ao longo de horas ou dias, e algumas mudam o mundo (enviam e-mail, fazem pagamento, editam código). Hoje, quem constrói isso enfrenta seis problemas:

| Problema | O que acontece hoje | Evidência |
|---|---|---|
| **Fluxo escondido** | No ReAct e no AutoGen, o controle está dentro do texto do LLM ou da conversa; não dá para inspecionar nem garantir nada | [Papers](docs/discovery/02-papers.md) |
| **Paralelismo feito à mão, e errado** | O programador escreve `parallel` ou `async`, e o framework erra | No código do AgentSPEX, o `parallel` roda em sequência e os ramos perdem o estado ([Papers](docs/discovery/02-papers.md)) |
| **Agentes concorrentes corrompem estado** | Dois agentes leem, pensam por segundos e escrevem; um apaga o trabalho do outro | Sem proteção, 97,5% das execuções concorrentes violam regras de negócio; sistemas multiagente falham em 41–87% dos benchmarks ([SVBE](docs/discovery/10-svbe.md), [concorrência](docs/discovery/07-escalonamento-e-concorrencia.md)) |
| **Proteger com trava é lento** | Travar durante o raciocínio do LLM bloqueia todo mundo | Latência P95 de 5 s para 29 s ([SVBE](docs/discovery/10-svbe.md)) |
| **Falhas custam caro e duplicam efeitos** | O processo cai no meio; recomeçar repete chamadas pagas ou envia o e-mail de novo | No Temporal, o determinismo depende de disciplina e a idempotência é só recomendação ([Temporal](docs/discovery/05-temporal.md)) |
| **Custo e contexto explodem em produção** | O agente entra em laço, a conversa estoura a janela, a conta chega alta | Bibliotecas Python não analisam o grafo antes de rodar ([Papers](docs/discovery/02-papers.md)) |

## A proposta

**Você escreve o que o agente faz, como um grafo. A linguagem garante o resto.**

- **Antes de rodar**, o compilador verifica, em até 1 segundo: teto de custo, se a conversa cabe na janela do modelo, se todo efeito perigoso tem política, se dois ramos não disputam o mesmo recurso, se todo laço termina.
- **Ao rodar**, o runtime deriva sozinho o que hoje se escreve à mão: o paralelismo, a retomada depois de uma queda sem pagar de novo, a garantia de não enviar nada duas vezes, e o rastreamento de custo por passo.

```
graph research(topic: Text) -> Text:
    limits threads 8, budget 2 USD

    plan = claude(split_topic(topic))
    findings = for each q in plan.questions:   # paralelismo derivado, sem "parallel"
        agent claude:
            tools [web_search]
            max_turns 10
            task investigate(q)
            on turn_limit: final_answer
            on stuck: final_answer

    return claude(write_report(topic, findings))
```

A sintaxe **parece Python e se comporta como uma linguagem funcional**: cada linha `nome = ...` é um passo do grafo, valores não mudam, e quem decide a ordem de execução são as dependências, não a ordem das linhas.

A analogia mais próxima é o **SQL**: você declara o que quer, e o banco decide como executar, paralelizar e se recuperar. A Calyx tenta fazer isso para agentes.

**Para quem:** times que colocam agentes em produção com efeitos reais (dinheiro, mensagens, código), e **agentes que escrevem agentes**: com verificação rápida e erros estruturados, um agente de IA pode escrever código Calyx e corrigi-lo sozinho.

**O que ela não resolve:** a qualidade do raciocínio do modelo e dos prompts; decidir qual agente construir; regras de negócio de sistemas externos além das precondições declaradas.

**Por que uma linguagem, e não uma biblioteca:** as garantias dependem de analisar o programa inteiro antes de rodar. Uma biblioteca não consegue impedir que se chame o relógio dentro do workflow ou que se envie um e-mail duas vezes. O preço é a adoção, bem mais difícil que a de uma biblioteca.

**O que ainda não está provado:** a hipótese central foi testada só no papel ([teste no papel](docs/discovery/04-teste-no-papel.md)). As perguntas de pesquisa da [hipótese](docs/discovery/01-hipotese.md) só se respondem com uma implementação.

## Estado do projeto

O **discovery** está concluído (34 decisões fechadas). Na implementação, os marcos M0 (estrutura), M1 (`calyx check` para o subconjunto inicial) M2 (`calyx run`: o runtime executa o grafo de ponta a ponta, com chamadas reais de modelo e tools via MCP) e M3 (diário: uma execução interrompida continua sem refazer nenhuma chamada concluída) estão concluídos. O objetivo desta fase é entender o estado da arte e definir a hipótese central e as decisões de design. Princípios do projeto: **compilar para código nativo, rodar rápido e verificar um programa em até 1 segundo**, para que um agente de IA possa verificar a cada mudança.

## Como compilar e testar

Requisitos: Rust (stable) e um compilador C.

```sh
make test     # testes do compilador (Rust) e do runtime (C); precisam de python3
make lint     # rustfmt + clippy
cargo run -p calyx-cli -- check examples/research.clyx --ir --time
```

## Como rodar um programa

```sh
# Sem rede e sem chave: os modelos devolvem respostas falsas no formato do tipo do prompt.
cargo run -p calyx-cli -- run examples/research.clyx --fake-models --topic "energia solar no Brasil"

# Com um modelo de verdade (o exemplo usa gemini-3.5-flash-lite):
export GEMINI_API_KEY=...      # nunca no código nem no calyx.toml
cargo run -p calyx-cli -- run examples/research.clyx --topic "energia solar no Brasil"
```

Cada parâmetro do grafo vira uma opção (`--topic`). O resultado vai para a saída padrão; o rastro (uma linha por passo e por chamada, com tempo e tokens) vai para a saída de erro, e some com `--quiet`.

**Diário e retomada.** Toda execução grava cada chamada concluída num diário, em `.calyx/runs/<id>/`. Se o processo cair, ou uma chamada falhar de vez, a execução continua de onde parou, sem pagar de novo pelo que já terminou:

```sh
calyx runs                 # lista as execuções e o estado de cada uma
calyx resume <id>          # continua; chamadas já no diário vêm dele
calyx replay <id>          # reexecuta só a partir do diário, sem chamar nada
```

**Modelos.** O runtime fala o formato de API da OpenAI, aceito por Gemini, NVIDIA, OpenAI, OpenRouter, Groq e Ollama. Identificadores `gemini-*`, `gpt-*` e `nvidia/*` já têm provedor embutido; outros se configuram no `calyx.toml`.

**Tools.** Cada tool roda num servidor MCP (decisão D34), declarado no `calyx.toml` ao lado do programa. O exemplo usa uma busca falsa ([`examples/tools/fake_search.py`](examples/tools/fake_search.py)); para usar uma busca real, troque o comando:

```toml
[tools.web_search]
command = ["python3", "tools/fake_search.py"]   # relativo ao calyx.toml
```

| Pasta | Conteúdo |
|---|---|
| `compiler/calyx-syntax` | Fonte, diagnósticos estruturados, lexer e parser |
| `compiler/calyx-check` | O verificador e a geração da representação intermediária |
| `compiler/calyx-ir` | Representação intermediária (o template do grafo) |
| `compiler/calyx-cli` | O comando `calyx` |
| `runtime/src` | O interpretador em C: valores, execução do grafo, novas tentativas, rastro |
| `runtime/rs` | A camada de E/S em Rust que o interpretador chama: modelos por HTTPS, tools por MCP, `calyx.toml`; e o verificador exposto ao C. Tudo sai numa biblioteca estática só |
| `tests/programs/` | Programas de teste com os diagnósticos esperados (`.expected`) e a representação intermediária esperada (`.ir`) |
| `examples/` | Programas de exemplo. `research.clyx` passa na verificação e roda; os outros usam construções de marcos futuros e, por enquanto, só precisam ser válidos lexicamente. `calyx.toml` e `tools/` configuram as tools dos exemplos |

## Plano de implementação

[`docs/roadmap.md`](docs/roadmap.md): marcos de implementação, começando por uma fatia vertical (parser → check → runtime) para provar o modelo de execução.

## Especificação

[`docs/spec/calyx.md`](docs/spec/calyx.md): rascunho v0 da especificação, consolidando todas as decisões do discovery.

## Documentos de discovery

| Documento | Conteúdo |
|---|---|
| [Hipótese](docs/discovery/01-hipotese.md) | A ideia central, o que a linguagem precisa garantir e como validar |
| [Papers](docs/discovery/02-papers.md) | Leitura dos trabalhos de referência a partir de 8 perguntas |
| [Decisões](docs/discovery/03-decisoes.md) | Decisões de design, com opções e recomendação |
| [Teste no papel](docs/discovery/04-teste-no-papel.md) | 8 workflows reais usados para testar a hipótese |
| [Temporal](docs/discovery/05-temporal.md) | Leitura do Temporal (execução durável) e impacto nas decisões de recuperação |
| [ReAct como ciclo](docs/discovery/06-react-como-ciclo.md) | Como representar o laço de raciocínio e ação como o primeiro ciclo do grafo |
| [Escalonamento e concorrência](docs/discovery/07-escalonamento-e-concorrencia.md) | Controle de concorrência entre agentes e escalonamento de grafos de tarefas |
| [Bend](docs/discovery/08-bend.md) | Runtime paralelo e tipos afins do Bend, e o que se transfere para a Calyx |
| [Sintaxe](docs/discovery/09-sintaxe.md) | Proposta de sintaxe testada com 9 programas em `examples/` |
| [SVBE](docs/discovery/10-svbe.md) | Consistência de estado entre agentes concorrentes: validação semântica no momento do efeito |
| [Mapa da orquestração](docs/discovery/11-mapa-orquestracao.md) | Onde a Calyx está na pilha de orquestração de agentes, e o que falta |
| [Arquitetura do runtime](docs/discovery/12-arquitetura-runtime.md) | Compilador e runtime nativos com modelo de atores; princípio de rodar e compilar rápido |
| [Concorrência](docs/discovery/13-concorrencia.md) | Os três problemas da concorrência (descobrir, executar, estado) e o modelo de concorrência da Calyx |
