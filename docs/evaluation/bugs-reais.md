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

*(a preencher)*
