# ReAct como ciclo no grafo

## A pergunta

Na [primeira leitura](02-papers.md), o ReAct foi tratado como "o átomo que roda dentro de um nó". Agora, com o modelo de grafo definido, a pergunta muda:

> **Como representar um laço de raciocínio e ação dentro de um grafo?**

```
Model → Tool → Observation → Model → Tool → Observation → … → resposta
```

É o **primeiro caso de ciclo** da Calyx e, por isso, define como ciclos funcionam na linguagem (D5).

## Como os outros representam

| Sistema | Onde fica o laço | Visível no grafo? |
|---|---|---|
| **ReAct (paper)** | No texto gerado pelo LLM (*Thought / Action / Observation*) | Não há grafo |
| **DSPy** | `for idx in range(max_iters)` dentro do módulo `dspy.ReAct` (padrão: 20), com uma tool especial `finish` | Não: escondido no Python |
| **AgentSPEX** | `for iter in range(max_tool_calls_per_step + 1)` dentro do executor de cada `step` | Não: escondido no runtime |
| **Temporal** | Um `while` no código do workflow, com cada chamada sendo uma activity | Não há grafo |
| **AutoGen** | Uma conversa entre dois agentes (um raciocina, o outro executa) | Não: emerge da conversa |
| **LangGraph** | Um nó "agente", um nó "tools" e uma aresta condicional que volta | Sim: ciclo explícito |

Quase todos **escondem** o laço. Para a Calyx isso é um problema, porque a hipótese depende de o runtime enxergar a estrutura.

---

## Três formas de representar

### Opção A: laço opaco dentro de um nó

```
nó agir  efeito: (inferido)  tools: [buscar, ler, editar]  laço ReAct, no máximo 30 passos
```

- ✅ Simples de escrever.
- ❌ O grafo não mostra o laço. O compilador não vê o ciclo, só um nó.
- ❌ É o que DSPy e AgentSPEX fazem: o runtime precisa de lógica especial para o diário, a observabilidade e a concorrência *dentro* do nó.

### Opção B: ciclo explícito no grafo

```
         ┌──────────────────────────────────────────┐
         ▼                                          │
nó modelo   efeito: llm   continua: conversa         │
         │  produz: final(resposta) | chamar(lista<chamada>)
         │                                          │
   se final ─────────► saída: resposta              │
   se chamar                                        │
         ▼                                          │
nó executar[c]  para cada c em chamadas  (tool escolhida por c.nome)
         │  produz: observação[c]                   │
         ▼                                          │
nó observar  efeito: pure  junção: lista ordenada    │
         │  produz: conversa + chamadas + observações
         └──────────────────────────────────────────┘
   limite: no máximo 30 voltas
```

- ✅ Tudo visível: cada passo é um nó, e o compilador e o runtime raciocinam sobre o ciclo como sobre qualquer outra parte do grafo.
- ❌ Verboso. Quase todo agente tem esse laço, e escrevê-lo à mão toda vez é o tipo de "encanamento" que a Calyx quer eliminar.

### Opção C: construção da linguagem que o compilador expande no ciclo explícito

```
agente pesquisar  modelo: …  tools: [buscar, ler]  no máximo 30 voltas
```

O programador escreve a forma curta. O **compilador expande** para o ciclo da opção B, e todas as análises (efeitos, orçamento, concorrência, diário) rodam sobre a forma expandida.

- ✅ Curto como A, visível como B.
- ✅ A forma expandida aparece na visualização e no trace.
- ✅ Quem precisar de um laço diferente (com um nó de verificação no meio, por exemplo) escreve o ciclo à mão, como na opção B, e recebe as mesmas garantias.

**Recomendação: opção C.** O "agente" é açúcar sintático sobre um ciclo de verdade, não um conceito à parte no runtime.

---

## O que o ciclo explícito revela

Escrever o ReAct como ciclo força decisões que as outras representações escondem.

### 1. O ciclo é um laço com valor carregado, não uma aresta que volta com estado mutável

A cada volta, a `conversation` cresce:

```
conversa₁ = conversa₀ + resposta do modelo + observações
conversa₂ = conversa₁ + …
```

É um **acumulador** (um *fold*, em programação funcional), coerente com a D1: o valor flui pelas arestas, imutável, e cada volta produz um valor novo. Não existe "estado do laço" mutável.

**Regra proposta para ciclos em geral:** um ciclo é sempre um **laço com valores carregados declarados** e um **limite**. Não existem arestas de volta arbitrárias.

### 2. O template tem ciclo, mas a execução é sempre acíclica

O survey de ACG separa *template*, *realized graph* e *trace*. Com ciclos, isso fica muito útil:

```
template (o código):      modelo → executar → observar → (volta)
realized graph (a execução):  modelo¹ → executar¹ → observar¹ → modelo² → executar² → … → modelo⁷ → final
```

**O ciclo só existe no template.** A execução é o ciclo **desenrolado**, e é sempre um grafo acíclico (DAG). Consequências:

- Cada volta tem identidade própria: *(nó, número da volta)*. O diário usa essa identidade.
- Recuperação, observabilidade e concorrência funcionam sobre o grafo desenrolado **exatamente como em qualquer outro grafo**. Nenhuma lógica especial para ciclos no runtime.
- O GraphFlow só aceitava DAG; na Calyx, ciclos são permitidos no template e a execução continua sendo DAG.

### 3. A saída do modelo é um tipo com variantes, e o compilador verifica todas

O nó `modelo` produz `final(resposta)` **ou** `chamar(lista de chamadas)`. A aresta seguinte escolhe o caminho pela variante, e o compilador verifica que **todas as variantes têm destino**. No ReAct original, a decisão era parsear texto (`Finish[...]`); o DSPy simula com uma tool `finish`. As APIs de LLM atuais já devolvem as chamadas de tool estruturadas, então a variante sai direto da resposta.

**Pergunta em aberto:** o que acontece quando o limite de voltas é atingido sem `final`? Opções: falha (D11), ou uma última volta forçando resposta final sem tools.

### 4. As tools possíveis são fixas; qual delas roda é dinâmico

O conjunto de tools é declarado; a escolha a cada volta é do modelo. Isso basta para o compilador:

- **Efeito do laço** = o maior efeito entre as tools permitidas (D2). Um agente com `buscar` e `ler` é no máximo `read`; se ganhar `enviar_email`, passa a `write once` e exige política.
- **Permissões:** restringir as tools de um agente é restringir o efeito máximo dele, verificado antes de rodar.

### 5. Concorrência: nenhuma entre voltas, possível dentro de uma volta

- **Entre voltas:** a volta 2 depende da `conversation` da volta 1. O runtime deriva corretamente que é **sequencial**.
- **Dentro de uma volta:** quando o modelo pede várias tools de uma vez, `executar[c]` é um **fan-out**. As chamadas `read` rodam em paralelo; as `write` seguem a ordem em que o modelo as pediu. A junção devolve as observações **na ordem das chamadas**, para a conversa ser reproduzível.

É a derivação "tools paralelas no mesmo turno" do [teste no papel](04-teste-no-papel.md), agora como consequência direta do grafo.

### 6. Erro de tool vira observação

No ReAct, uma tool que falha não deveria derrubar o agente: o modelo deveria ver o erro e tentar outra coisa. Com falhas como valores (D11):

- **Erro temporário** (rede, limite de requisições): o runtime repete sozinho, conforme o efeito.
- **Erro permanente** (declarado pela tool como não retentável, ou retentativas esgotadas): vira um valor `falha(erro)`, que entra na `conversation` como observação.

O modelo decide o que fazer. O agente só falha se o programador quiser.

### 7. Orçamento de contexto vira um invariante do laço

Na D3, o compilador somava o pior caso da `conversation` ao longo do caminho. Num laço de 30 voltas, a soma pode ficar enorme:

```
pior caso = 30 × (máximo de tokens do modelo + máximo da saída das tools)
```

Isso mostra que a D16 (tamanho máximo da saída das tools) é **obrigatória** para agentes. E pede um mecanismo novo: um **nó de compactação dentro do ciclo**. Se ele garante que a conversa sai com no máximo K tokens, o compilador verifica um **invariante**: *se a conversa entra numa volta com até K tokens, sai com até K tokens*. Aí o laço é seguro **independentemente do número de voltas**.

### 8. Recuperação: o laço é reencenado a partir do diário

Se o processo cair na volta 5:

- As respostas do modelo e as observações das voltas 1 a 4 vêm do diário; a `conversation` é reconstruída sem chamar o LLM.
- A volta 5 continua do ponto exato: se o modelo já respondeu e uma das três tools já rodou, só as outras duas rodam.
- Uma tool `write once` já concluída nunca roda de novo.

É o modelo do Temporal (cada chamada é uma *activity*), mas sem o risco de *non-deterministic error*, porque o laço é estrutura do grafo e não código livre.

### 9. As falhas do ReAct, revistas

| Falha apontada no ReAct | Como o ciclo explícito ajuda |
|---|---|
| Contexto estoura em trajetórias longas | Erro de compilação (item 7) |
| Laços repetitivos (mesma ação várias vezes) | As chamadas são estruturadas e estão no diário; o runtime pode detectar *(tool, argumentos)* repetidos e o programador pode declarar o que fazer |
| Uma busca ruim descarrila o raciocínio | Não se resolve com estrutura, mas o ciclo explícito permite inserir um nó de verificação dentro do laço |
| Controle escondido no LLM | O controle é o ciclo, visível; o LLM só escolhe a variante e as tools |

### 10. Agentes dentro de agentes

Um submódulo pode ser exposto como tool (ideia do AgentSPEX). Se essa tool for outro agente, temos **ciclos aninhados**. Os limites se multiplicam (30 voltas × 10 voltas internas), e o compilador consegue calcular o pior caso de custo e de contexto, desde que cada ciclo tenha limite (D5) e as recursões tenham parâmetro decrescente (D17).

---

## Proposta para D5 (ciclos), refinada

| Regra | Motivo |
|---|---|
| Ciclo = laço com **valores carregados declarados** e **limite obrigatório** | Coerente com a D1: sem estado mutável; termina por construção |
| A saída que decide continuar ou parar é um **tipo com variantes**, verificado por completo | Nenhum caminho sem destino |
| O ciclo existe só no **template**; a execução é o ciclo **desenrolado** (DAG) | Recuperação, observabilidade e concorrência sem lógica especial |
| Cada volta tem identidade *(nó, volta)* no diário | Retomada exata no meio de uma volta |
| Orçamento de contexto verificado por **invariante** quando há compactação no ciclo | Laços longos seguros independentemente do número de voltas |
| `agente` é açúcar sintático que o compilador expande num ciclo explícito | Curto para escrever, visível para analisar |

## Decisões

**Decidido** (aprovado com o bloco A, ver D5 em [03-decisoes.md](03-decisoes.md)):

1. **Opção C:** `agent` é atalho que o compilador expande num ciclo explícito.
2. **Limite de voltas atingido** e **repetição detectada** viram variantes do resultado do agente (`turn_limit`, `stuck`), e o compilador obriga a tratá-las. A linguagem oferece o tratamento comum em uma linha (`=> final_answer`: uma última volta pedindo a resposta). Nem falha automática, nem resposta forçada escondida.
3. **Repetição:** o runtime detecta chamadas idênticas (mesma tool, mesmos argumentos). Tools em que repetir é legítimo (ex.: consultar status) declaram isso.
4. **Compactação:** por tamanho, não a cada N voltas. O limite é calculado pelo compilador (janela do modelo menos o pior caso de uma volta). Compactar menos vezes preserva o cache por prefixo.
5. **Desempenho do `check`:** o pior caso de um laço é calculado multiplicando pelo limite, sem desenrolar.
