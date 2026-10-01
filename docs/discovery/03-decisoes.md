# Decisões de design em aberto

Decisões que a [hipótese](01-hipotese.md) levanta. Cada uma traz as opções, o que a literatura faz e uma recomendação. **Nenhuma está fechada**: servem de pauta para discussão.

| # | Decisão | Recomendação preliminar |
|---|---|---|
| D1 | Modelo de estado | Dataflow por padrão + estado nomeado com regra de junção obrigatória |
| D2 | Tipos de efeito | 4 níveis verificados pelo compilador |
| D3 | O que "thread" significa | Thread = conversa (valor explícito); concorrência é derivada, nunca escrita |
| D4 | Quanto dinamismo permitir | Dinamismo limitado e verificável |
| D5 | Ciclos no grafo | Permitidos, com limite obrigatório |
| D6 | Unidade de recuperação | Diário de nós concluídos (event sourcing) |
| D7 | Junção de ramos paralelos | Lista ordenada por padrão; redutores declarados |
| D8 | Template / realized graph / trace | Os três são conceitos de primeira classe |
| D9 | Superfície da linguagem | Linguagem textual, com visualização derivada |
| D10 | Plataforma (C# ou C) | Adiada; critérios listados abaixo |

---

## D1. Modelo de estado

A decisão mais importante: sem ela, o runtime não consegue derivar concorrência.

| Opção | Quem faz assim | Concorrência derivável? | Problema |
|---|---|---|---|
| (a) Dicionário global mutável | AgentSPEX | Não | Qualquer nó escreve em tudo; os ramos paralelos precisam de cópia e perdem estado |
| (b) Canais de estado com *reducers* | LangGraph | Parcial | Leituras e escritas não são declaradas nem verificadas |
| (c) Dataflow puro: valores imutáveis nas arestas | Linguagens de dataflow, TensorFlow | Sim | Estado acumulado (memória, conversa) fica verboso |
| (d) Híbrido: dataflow + estado nomeado com junção obrigatória | — | Sim | Mais conceitos na linguagem |

**Recomendação: (d).** Por padrão, um nó recebe valores pelas arestas e produz valores. Quando precisa de estado compartilhado, o estado é nomeado, o nó declara se lê ou escreve, e **todo estado que pode ser escrito por ramos concorrentes tem uma regra de junção obrigatória**. O compilador recusa o programa se faltar a regra.

## D2. Tipos de efeito

Base da recuperação de falhas.

| Nível | Significado | Pode reexecutar? | Na recuperação |
|---|---|---|---|
| `pure` | Função determinística | Sim | Recalcula |
| `llm` | Não-determinístico, sem efeito externo | Tecnicamente sim, mas o resultado muda e custa dinheiro | Reaproveita o resultado gravado |
| `read` | Lê do mundo externo (busca, arquivo, API GET) | Sim, mas o resultado pode mudar | Reaproveita o resultado gravado |
| `write` | Altera o mundo (e-mail, banco, pagamento) | Só com idempotência ou compensação | Exige chave de idempotência ou ação de compensação declarada |

**Recomendação:** os 4 níveis, verificados pelo compilador (um nó não pode chamar algo de nível mais alto que o seu). Tools declaram o próprio nível. Inspiração: sistemas de efeitos em linguagens como Koka, e as garantias de atividades do Temporal.

**Em aberto:** um nó `write` sem idempotência deve ser **proibido** ou apenas **marcado como não recuperável automaticamente**?

## D3. O que "thread" significa

A literatura usa "thread" em três sentidos: execução paralela, conversa (histórico) e caminho no grafo.

| Opção | Consequência |
|---|---|
| (a) Thread = execução paralela escrita pelo programador | Contradiz a hipótese: a concorrência deixaria de ser derivada |
| (b) Thread = conversa, como valor explícito | O histórico flui pelas arestas; pode ser bifurcado (`fork`) e juntado; a concorrência continua derivada |
| (c) Unificar os dois | Mais simples de explicar, mas mistura dois conceitos com semânticas diferentes |

**Recomendação: (b).** Um thread de conversa é um valor explícito que flui pelo grafo. Dois ramos que recebem o mesmo thread trabalham sobre cópias imutáveis (bifurcação), e juntá-los exige regra (D7). A concorrência de execução nunca é escrita. Isso corrige diretamente o problema do `conversation_history` global do AgentSPEX.

**Em aberto:** o nome. "Thread" em programação sugere concorrência; se a linguagem usar "thread" para conversa, isso precisa ficar muito claro.

## D4. Quanto dinamismo permitir

A tensão expressividade × verificabilidade do survey de ACG.

| Nível | Exemplo | Verificável? |
|---|---|---|
| 0. Grafo estático | Pipeline fixo | Totalmente |
| 1. Condicionais e laços limitados | Laço de revisão com até N tentativas | Sim |
| 2. Fan-out tipado | Um nó de pesquisa por sub-pergunta gerada pelo LLM | Sim: não se sabe *quantos* nós, mas se sabe o tipo deles |
| 3. Subgrafo gerado e verificado antes de executar | Plan generator do AgentSPEX; GraphFlow | Sim, se passar pelo verificador |
| 4. Edição arbitrária durante a execução | DyFlow, EvoFlow | Não |

**Recomendação: níveis 0 a 3.** O nível 4 fica fora, pelo menos no início. Regra: *o grafo pode mudar em tempo de execução, mas só de formas que preservem as garantias estáticas.*

## D5. Ciclos no grafo

| Opção | Quem faz assim | Consequência |
|---|---|---|
| Só DAG | GraphFlow | Análise simples, mas não expressa laços de revisão |
| Ciclos com limite obrigatório | AgentSPEX (`max_iterations`) | Expressa revisão e retentativa; a análise trata cada iteração como uma "camada" |
| Ciclos livres | AutoGen, LangGraph | Risco de laço infinito e custo sem limite |

**Recomendação:** ciclos permitidos, com **limite obrigatório** verificado pelo compilador. Um limite de custo (tokens, dinheiro, tempo) também deve ser suportado.

## D6. Unidade de recuperação

| Opção | Quem faz assim | Problema |
|---|---|---|
| Checkpoint por "passo concluído" | AgentSPEX | Pressupõe execução sequencial |
| Checkpoint por super-passo | LangGraph (Pregel) | Refaz o super-passo inteiro |
| Diário de nós concluídos (event sourcing) | Temporal | — |

**Recomendação:** um **diário** em que cada entrada é *(nó, iteração, hash das entradas, saída, custo)*. Na retomada, todo nó cuja entrada já está no diário é pulado e tem a saída reaproveitada. Funciona naturalmente com ramos paralelos e é o mesmo dado que o trace de observabilidade (D8): recuperação e observabilidade saem da mesma estrutura.

## D7. Junção de ramos paralelos

| Caso | Regra padrão proposta |
|---|---|
| Fan-out sobre uma lista | Resultados em lista, **na ordem da entrada** (não na ordem de término), para a execução ser reproduzível |
| Dois ramos escrevem no mesmo estado nomeado | Obrigatório declarar o redutor: `concat`, `last`, `max`, `vote` ou função `pure` definida pelo usuário |
| Dois ramos estendem o mesmo thread de conversa | Obrigatório declarar: concatenar, resumir via LLM, ou manter separados |

**Em aberto:** redutores via LLM ("junte essas duas respostas") são nós `llm`, não `pure`. Permitir?

## D8. Template, realized graph e trace

**Recomendação:** adotar os três artefatos do survey de ACG como conceitos de primeira classe:

- **Template:** o código-fonte compilado.
- **Realized graph:** o que o runtime materializou nesta execução (após condicionais, fan-outs, subgrafos gerados). Pode ser inspecionado e visualizado.
- **Trace:** o diário da D6. Serve para retomada, replay, depuração, custo por nó e, no futuro, otimização estilo DSPy.

## D9. Superfície da linguagem

| Opção | Evidência |
|---|---|
| YAML | AgentSPEX: legível para coisas simples, mas usuários preferiram LangGraph para workflows complexos; sem tipos |
| Biblioteca em linguagem existente | LangGraph, DSPy: o grafo não é analisável antes de rodar |
| Linguagem textual própria | Permite tipos, verificação e compilador |
| Visual | Bom para inspeção; ruim como única forma de autoria |

**Recomendação:** linguagem **textual** própria, com **visualização do grafo derivada** do código (o editor sincronizado do AgentSPEX foi bem avaliado).

## D10. Plataforma: C# ou C

Adiada por decisão do projeto. Critérios a considerar:

| Critério | Por que importa |
|---|---|
| Primitivas de concorrência do runtime | O runtime é um escalonador de grafo com muita espera de I/O (LLM, tools) |
| Ecossistema HTTP, JSON e clientes de LLM | Quase todos os nós conversam com APIs externas |
| Custo de escrever compilador e verificador | Parser, sistema de tipos, verificação de efeitos |
| Performance e controle de memória | Relevante se o runtime for gerenciar cache e estado em escala (lição do GraphFlow) |
| Embutir o runtime em outras aplicações | C é mais fácil de embutir; C# depende do .NET |

---

## Exemplo ilustrativo

**Não é proposta de sintaxe.** Mostra só *que informação* cada nó precisaria declarar para a hipótese funcionar.

```
nó planejar      efeito: llm    lê: pergunta           produz: subperguntas: lista<texto>
nó pesquisar[q]  efeito: read   para cada q em subperguntas   produz: achado: texto
nó escrever      efeito: llm    lê: pergunta, achados (junção: lista ordenada)   produz: rascunho
nó revisar       efeito: llm    lê: rascunho           produz: aprovado: bool
laço escrever → revisar  até aprovado, no máximo 3 vezes
nó publicar      efeito: write  idempotência: hash(rascunho)   lê: rascunho
```

A partir só disso, o runtime saberia:

- **Concorrência:** os `pesquisar[q]` rodam em paralelo; `escrever` espera todos.
- **Dependências:** mudar `pergunta` invalida tudo; mudar um achado invalida só `escrever` em diante.
- **Recuperação:** se cair durante `revisar`, reaproveita `planejar`, os `pesquisar` e `escrever` do diário; `publicar` nunca roda duas vezes.
- **Observabilidade:** custo e latência por nó, por iteração do laço e por ramo do fan-out.

---

## Próximos passos sugeridos

1. **Discutir e fechar D1, D2 e D3.** São as três de que a hipótese depende diretamente.
2. **Ler mais referências** que cobrem as lacunas desta rodada:
   - LangGraph e o modelo Pregel (o concorrente mais próximo em concorrência e estado).
   - LLMCompiler (Kim et al., ICML 2024): paralelismo derivado de DAG de chamadas de função.
   - Temporal / execução durável (base da D6).
   - Sistemas de efeitos (Koka, efeitos algébricos), como base da D2.
3. **Validar a hipótese no papel:** escrever 5–10 workflows reais (dos benchmarks do AgentSPEX, por exemplo) na notação ilustrativa e checar se as quatro propriedades saem sem anotação extra.
4. **Só então:** sintaxe concreta e escolha entre C# e C.
