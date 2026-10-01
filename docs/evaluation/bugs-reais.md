# Bugs reais: o que a Calyx teria pegado

Bugs relatados por usuários em issues públicas de LangGraph, CrewAI e
AutoGen, classificados pelo que a Calyx faria com o mesmo workflow. É a
resposta ao maior viés da avaliação: o corpus Q2 (`tests/state_bugs/`) foi
escrito por quem fez o compilador.

## Protocolo (escrito antes de ler os resultados)

**Fontes:** issues (abertas ou fechadas) de `langchain-ai/langgraph`,
`crewAIInc/crewAI` e `microsoft/autogen`, pela busca de issues do GitHub.

**Buscas**, as mesmas nos três repositórios, os 8 primeiros resultados de
cada (ordem de relevância da busca):

1. `tool executed twice duplicate call after resume or retry`
2. `parallel nodes overwrite each other state concurrent update`
3. `side effect repeated when resuming after interrupt`
4. `agent stuck in loop repeating the same tool call`
5. `state lost or not saved after crash checkpoint restore`
6. `waiting for human approval forever no timeout`
7. `retry sends email or payment twice`
8. `wrong order of tool calls side effect before check`

**Entra** um relato de um comportamento errado de um workflow de agentes
(não um pedido de recurso) numa destas categorias:

| Categoria | O quê |
|---|---|
| C1 efeito repetido | Uma tool com efeito externo roda de novo (nova tentativa, retomada, replay) |
| C2 escrita concorrente | Ramos paralelos gravam o mesmo estado; uma escrita se perde ou dá erro |
| C3 atualização perdida | Estado lido, mudado fora e gravado por cima, entre execuções ou dentro de uma |
| C4 espera | Espera por humano ou evento: sem prazo, retomada que repete, resposta aplicada duas vezes |
| C5 laço de agente | Agente repete a mesma tool ou não termina |
| C6 ordem de efeitos | Um efeito acontece antes do que deveria (ou sem o que deveria vir antes) |
| C7 recuperação | Retomada após falha perde trabalho, refaz trabalho ou restaura estado inconsistente |

**Fica de fora:** pedido de recurso sem bug, documentação, instalação, erro
do provedor de modelo, interface, desempenho puro, tipagem do próprio
framework. Cada exclusão fica registrada com o motivo.

**Classificação**, para o mesmo workflow escrito em Calyx:

| Classe | Quando |
|---|---|
| **compilador** | O programa em Calyx seria recusado ou receberia um aviso, com o código do erro citado |
| **runtime** | O runtime da Calyx evita o bug quando ele aconteceria (diário, chaves, `flock`, cancelamento), sem código do programador |
| **construção** | O bug não pode ser escrito em Calyx (ex.: não há estado mutável compartilhado dentro de uma execução) |
| **não pega** | A Calyx deixaria o bug passar |
| **bug do framework** | Erro de implementação do próprio framework (ex.: serialização do checkpoint), sem análogo na linguagem. Não conta a favor nem contra: a Calyx tem os próprios bugs de implementação |

Na dúvida entre duas classes, vale a menos favorável à Calyx. Os casos de
"compilador" serão conferidos escrevendo o programa em Calyx e rodando o
`calyx check`.

## Resultados

As 24 buscas deram **79 issues distintas** (LangGraph 40, CrewAI 25,
AutoGen 14), todas em [`bench/real_bugs/candidates.json`](../../bench/real_bugs/candidates.json).
**35 ficaram de fora** pelo protocolo (pedido de recurso, provedor, desempenho,
documentação, duplicatas), cada uma com o motivo; **44 entraram**. A
classificação de cada uma, com o motivo, está em
[`bench/real_bugs/classify.py`](../../bench/real_bugs/classify.py) (e
`classification.json`).

| | Excluídas | Compilador | Runtime | Construção | Não pega | Bug do framework |
|---|---|---|---|---|---|---|
| LangGraph | 12 | 0 | 3 | 0 | 4 | 21 |
| CrewAI | 13 | 0 | 2 | 1 | 3 | 6 |
| AutoGen | 10 | 0 | 0 | 1 | 1 | 2 |
| **Total** | **35** | **0** | **5** | **2** | **8** | **29** |

| Categoria | Runtime | Construção | Não pega | Bug do framework |
|---|---|---|---|---|
| C1 efeito repetido | | | 1 | 8 |
| C2 escrita concorrente | | | | 5 |
| C3 atualização perdida | 1 | | | |
| C4 espera | 1 | 2 | 1 | 6 |
| C5 laço de agente | 2 | | 4 | 2 |
| C6 ordem de efeitos | | | 1 | |
| C7 recuperação | 1 | | 1 | 8 |

### O que os números dizem

**1. Dois terços (29 de 44) são bugs de implementação do próprio
framework**, quase todos na maquinaria de checkpoint, retomada e interrupt do
LangGraph (21) e na execução de tools do CrewAI (6). Não contam a favor da
Calyx: ela tem a própria implementação, que pode ter bugs parecidos. Mas 8
deles repetem um efeito externo (tool chamada duas vezes), e aí uma defesa
da Calyx vale mesmo contra bugs do runtime: uma tool `write` leva a chave de
idempotência até o servidor, que recusa a segunda aplicação venha de onde
vier. Isso não entra na conta.

**2. Dos 15 bugs de workflow, a Calyx evita 7 e deixa passar 8.**

- **Runtime (5):** duas esperas no mesmo passo não refazem a primeira
  (LangGraph 6208); o agente que repete a mesma chamada para na terceira
  volta igual, com o `on stuck` que o compilador exige (CrewAI 737, LangGraph
  5099); atualizações concorrentes do mesmo estado passam por uma entidade,
  uma por vez (CrewAI 6125); o recurso de uma tarefa volta pelo snapshot na
  retomada (LangGraph 8582).
- **Construção (2):** o passo depois da aprovação humana usa o valor do
  `receive`, então não começa antes dela (CrewAI 960, AutoGen 6819).
- **Não pega (8):** agentes que variam as chamadas, param cedo ou chamam a
  mesma tool só duas vezes (6731, 2294, 2209, 833); tool perigosa chamada por
  um agente sem confirmação (8817); passagem de controle entre agentes numa
  conversa, que a Calyx não tem (6064); resposta parcial perdida ao cancelar
  (5672); e o pagamento repetido numa nova tentativa (CrewAI 5802, abaixo).

**3. O compilador não pegou nenhum.** É o resultado mais importante, e o
menos favorável. Os bugs que o compilador recusa no corpus Q2 (efeito sem
contrato, escrita sem chave, ordem ao acaso) são erros do programador; as
issues relatam o framework se comportando mal. Quem esquece a chave de
idempotência não abre uma issue, descobre o pagamento duplicado em produção.
Ou seja: issues públicas não medem bem a afirmação A1, e não a confirmam.

### A lacuna encontrada

Ao conferir o CrewAI 5802 (a tarefa é repetida quando a saída não passa numa
conferência, e o pagamento sai de novo), a Calyx acerta com a tool `write` e
chave de idempotência: o laço chama `refund` três vezes e a loja paga uma.
Mas com a mesma tool declarada `write once` dentro de um `loop`, **o
compilador não diz nada e a loja paga três vezes**: cada volta tem sua chave
no diário, então cada volta é um pagamento novo. Pela regra do empate, o
5802 conta como "não pega".

É um aviso que falta: uma chamada `write once` no corpo de um `loop` ou
`rounds` cujos argumentos não dependem da volta repete o mesmo efeito a cada
volta. Fica registrado aqui como achado do estudo; se o aviso for criado, a
contagem acima não muda (ela vale para a Calyx de antes do estudo).

### Verificação

`python bench/real_bugs/run_verify.py` roda os programas de
[`bench/real_bugs/verify/`](../../bench/real_bugs/verify/), um por caso
classificado a favor da Calyx (e um para a lacuna), e confere o que está dito
acima. Os de entidades e sandbox já têm testes (`tests/entities.rs`,
`tests/sandbox.rs`). Resultado em `bench/results/real_bugs_verify.json`.

### Ameaças à validade

- **Leitura por resumo.** As issues foram lidas pela página web resumida por
  um modelo pequeno, não pelo texto inteiro com os comentários. Uma causa
  explicada só num comentário pode ter escapado (é o caso do LangGraph 2610,
  que ficou na classe neutra por isso).
- **Um classificador só**, o autor da linguagem. A regra do empate reduz o
  viés, mas uma segunda pessoa classificando às cegas é o certo para o paper.
- **Amostra pela busca do GitHub:** 8 resultados por busca, pela relevância
  da busca. Outras buscas dariam outros bugs; as buscas foram fixadas antes.
- **"Construção" e "runtime" supõem o programa natural** em Calyx. Um
  programador pode escrever o passo seguinte sem usar a aprovação (aí ele
  roda em paralelo com a espera), ou uma `write once` num laço.
