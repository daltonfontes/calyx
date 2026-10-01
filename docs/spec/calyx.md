# Especificação da Calyx (rascunho v0)

**Status:** rascunho consolidado ao fim do discovery. Reúne as 34 decisões de [`docs/discovery/03-decisoes.md`](../discovery/03-decisoes.md) num lugar só. Onde a sintaxe ainda é provisória, isso está indicado.

A Calyx é uma linguagem para programar agentes de IA como **grafos**. O programador escreve o grafo; o compilador verifica as garantias; o runtime extrai a concorrência, recupera falhas e registra tudo.

---

## 1. Princípios

1. **O programa é um grafo.** Nós são passos (chamadas de modelo, tools, cálculos); arestas são dependências de dados ou de ordem.
2. **Você escreve o grafo, o runtime extrai as threads.** O programador nunca cria threads; só declara limites.
3. **Efeitos com tipo.** Toda interação com o mundo externo tem um tipo de efeito, verificado pelo compilador.
4. **Nunca segurar trava durante a inferência; validar no momento do efeito.**
5. **Tudo o que custa caro ou não é determinístico vai para o diário**, e a recuperação reaproveita o diário em vez de repetir.
6. **Compila para código nativo, roda rápido, e `calyx check` verifica um programa em até 1 segundo**, para que um agente de IA possa verificar a cada mudança. Por isso, toda análise do compilador é linear ou composicional.

---

## 2. Visão geral de um programa

```
model claude = llm("claude-sonnet-5-5", max_output: 2_000 tokens)

tool web_search(query: Text) -> Text {
  effect     read
  max_output 4_000 tokens
}

type Plan = { questions: List<Text> max 5 }

prompt split_topic(topic: Text) -> Plan {
  """
  Divida o tema abaixo em até 5 perguntas de pesquisa.
  Tema: {topic}
  """
}

graph research(topic: Text) -> List<Text> {
  limits { threads: 8, budget: 2 USD }

  node plan     = claude(split_topic(topic))
  node findings[q in plan.questions] = web_search(q)

  return findings
}
```

Exemplos completos: [`examples/teste.clyx`](../../examples/teste.clyx) e [`examples/workflows/`](../../examples/workflows/).

---

## 3. Léxico

| Elemento | Forma |
|---|---|
| Comentário | `// até o fim da linha` |
| Texto | `"..."`, com interpolação `{expressão}` |
| Texto longo (prompts) | `"""..."""`, várias linhas, com interpolação |
| Números | `42`, `1_000`, `0.5` |
| Unidades | tokens (`2_000 tokens`), dinheiro (`2 USD`, `100 BRL`), tempo (`30 s`, `5 min`, `3 days`), memória (`4 GB`), taxa (`50/s`) |
| Blocos | `{ }`; sem `;`; formatação padronizada por `calyx fmt` |
| Palavras-chave | em inglês; nomes, textos e comentários em qualquer língua |
| Extensão | `.clyx` |

---

## 4. Declarações de topo

### 4.1 Modelo

```
model NOME = llm("identificador-do-modelo", max_output: N tokens)
```

### 4.2 Roteador (D30)

```
router NOME = route [modelo1, modelo2, modelo3] {
  policy cheapest_that_passes(verificacao)
}
```

Tenta os modelos na ordem dada (do mais barato ao mais caro) até a resposta passar em `verificacao` (uma `fn` pura). A escolha feita é gravada no diário. Na v1, esta é a única política.

### 4.3 Tool

```
tool NOME(parametros) -> Tipo {
  effect          read | write | write once | sandbox
  max_output      N tokens                // obrigatório para tools usadas por agentes (D16)
  timeout         duração                 // opcional; há padrão por efeito (D22)
  retry_on        [Erro, ...]             // erros temporários, repetidos pelo runtime
  idempotency_key expressão               // para `write`
  on_uncertain    verify(fn) | pause | accept_loss   // obrigatório para `write once`
  checks          TipoDeEstado            // estado validável no momento do efeito (D29)
  repeatable                              // repetir com os mesmos argumentos é legítimo (D5)
}
```

**Implementação (D34):** a tool roda num servidor **MCP** separado, escrito em qualquer linguagem. A declaração `tool` é o **contrato** que a Calyx verifica e que o runtime aplica (efeito, limites, timeout, retentativa, idempotência, precondições). O nome da tool e o servidor que a implementa são ligados na configuração do projeto *(formato a definir no M2)*.

### 4.4 Tipos

```
type Registro = { campo: Tipo, ... }

type Variante =
  | Caso1
  | Caso2 { campo: Tipo }
```

### 4.5 Mensagens (D21)

```
message Nome =
  | Caso1
  | Caso2 { campo: Tipo }
```

### 4.6 Prompt

```
prompt NOME(parametros) -> TipoDeSaida {
  """
  Texto com {interpolação}.
  """
}
```

O compilador verifica que toda `{variável}` existe e que a saída é decodificada para `TipoDeSaida`.

### 4.7 Função pura (D27)

```
fn NOME(parametros) -> Tipo { expressão }
```

Sem efeitos; o compilador pode recalculá-la à vontade; não vai para o diário.

### 4.8 Grafo

```
graph NOME(parametros) -> Tipo [effect EFEITO_MAXIMO] [decreases PARAMETRO] {
  corpo
}
```

- `effect` (opcional) restringe o efeito máximo do grafo; o compilador verifica.
- `decreases` (obrigatório se o grafo chama a si mesmo) indica o parâmetro que diminui a cada chamada recursiva (D17).

### 4.9 Entidade (D15)

```
entity NOME key CHAVE: Tipo {
  state CAMPO: Tipo = valor_inicial

  on Mensagem(parametros) -> Tipo { ... }   // leitura: pode rodar em paralelo com outras leituras
  on Mensagem(parametros) { next CAMPO = ... }  // escrita: exclusiva
}
```

No máximo **uma** entidade aberta por chave. Se um handler altera `state`, ele é de escrita; caso contrário, de leitura (inferido pelo compilador).

---

## 5. O corpo de um grafo

### 5.1 Limites (D3)

```
limits { threads: 8, rate: 50/s, budget: 2 USD, memory: 4 GB }
```

Todos opcionais. O programador **limita** a concorrência, nunca a cria.

### 5.2 Nós e valores

| Forma | Significado |
|---|---|
| `node x = expr` | Um passo do grafo. Se tem efeito, vai para o diário |
| `node xs[i in lista] = expr` | Fan-out: um nó por item; `xs` é uma lista **na ordem de `lista`** (D7) |
| `let x = expr` | Valor da camada pura; não vai para o diário (D27) |
| `return expr` | Resultado do grafo |
| `respond expr` | Entrega o resultado antes do fim; o resto do grafo continua em segundo plano. Grafo com `respond` não tem `return` de valor; no máximo um `respond` por caminho (D19) |

### 5.3 Chamada a modelo (D28)

| Forma | Resultado |
|---|---|
| `modelo(prompt)` | Valor do tipo de saída do prompt |
| `modelo(prompt, continue: conversa)` | `Reply<T>`, com `.value` e `.conversation` |
| `modelo(prompt, continue: new)` | Começa uma conversa nova |

### 5.4 Escolha

```
if condição { ... } else { ... }

match expr {
  Caso1             => expr
  Caso2 { campo }   => { ... }
}
```

`match` precisa cobrir todas as variantes. **Nós de um ramo não escolhido nunca rodam** (D32).

### 5.5 Laço (D5)

```
node final = loop x = valor_inicial, max N {
  ...
  done valor        // termina com este valor
  next valor        // próxima volta com este valor
} else last | fail "motivo"
```

O limite é obrigatório. `else` define o que acontece se o limite for atingido.

### 5.6 Rodadas (D18)

```
node final = rounds 1..3 carry x: Tipo = valor_inicial {
  node passo[r in participantes] = ...
  next novo_valor
}
```

Como o `loop`, mas com **barreira** no fim de cada rodada: só ali os resultados de uma rodada ficam visíveis para a próxima.

### 5.7 Agente (D5)

```
node resultado = agent modelo {
  tools      [tool1, tool2(reads recurso), tool3(edits recurso)]
  max_turns  N
  task       prompt(...)
  compact    with prompt_de_resumo      // opcional; compactação por tamanho
  on turn_limit => final_answer | fail "motivo"
  on stuck      => final_answer | fail "motivo"
}
```

`agent` é atalho: o compilador o expande num ciclo explícito `modelo → tools → observação`. As variantes `turn_limit` e `stuck` (mesma tool, mesmos argumentos, repetidamente) são obrigatórias.

### 5.8 Corrida (D12)

```
node vencedor = race {
  a: expr
  b: expr
} first where condição else fail "motivo"
```

Os ramos rodam em paralelo; vence o primeiro que satisfaz a condição; os outros são cancelados entre nós. O vencedor é gravado no diário. Recursos passados aos ramos são consumidos; só os do vencedor voltam.

### 5.9 Falha como valor (D11)

```
node r = try expr          // Ok(valor) | Failed(erro)
node bons  = lista.ok()
node falhas = lista.failed()
```

O compilador obriga a tratar `Failed` antes de usar o valor.

### 5.10 Ordem entre efeitos (D2)

```
notificar after salvar
```

Aresta de ordem, sem dados. O compilador avisa quando dois nós com efeito de escrita externa não têm ordem definida. Recursos com dono (seção 7) já geram ordem sozinhos.

### 5.11 Precondições e invariantes (D29, D25)

```
node pago = refund(pedido, valor) requires {
  state.status == Delivered
  state.refunded + valor <= state.total
}

node gastos[i in itens] = ... ensures { sum(gastos) <= limite }
```

- `requires`: avaliado **pela tool**, sobre o estado atual, na mesma transação do efeito. Só operadores permitidos (comparação, aritmética, pertencimento). Pode ser escrito pelo programador ou vir de um LLM como saída tipada.
- `ensures`: avaliado na junção; se falhar, produz um valor de conflito.

### 5.12 Mensagens (D21)

```
node aprovacao = receive Approval timeout 3 days else Denied { reason: "expirou" }
node fatos     = ask Memoria(usuario).Recall(texto)     // síncrono
send Memoria(usuario).Remember(novos)                   // assíncrono
```

O compilador recusa ciclos de `ask` (D33).

### 5.13 Grafos gerados por LLM (D4)

```
type Plano = Graph<tools: [busca, leitura], models: [worker], max_effect: read, max_nodes: 20, returns: T>

node plano     = planner(make_plan(pedido))
node resultado = try run plano
```

`run` passa o grafo pelo mesmo verificador do compilador antes de executar. O tipo `Graph<...>` limita o que o LLM pode gerar.

### 5.14 Estado nomeado (D1) — *sintaxe provisória*

```
state gasto: Money = 0 USD, merge sum
```

Ramos paralelos veem o valor do momento da bifurcação (snapshot) e suas escritas se juntam pelo redutor (`merge`) na junção. Redutor é obrigatório para estado escrito por ramos concorrentes.

---

## 6. Tipos

| Tipo | Uso |
|---|---|
| `Text`, `Nat`, `Int`, `Bool`, `Money`, `Duration`, `Date` | Básicos |
| `List<T>`, `List<T> max N` | Listas; o limite entra nas análises de custo |
| `Map<K, V>` | Mapas |
| Registros e variantes | Seção 4.4 |
| `Conversation` | Histórico de conversa com um modelo; valor imutável (D3) |
| `Reply<T>` | Resposta com `.value: T` e `.conversation` |
| `Ok(T) \| Failed(Erro)` | Resultado de `try` |
| `Prompt<T>` | Prompt como valor (ex.: passado a um subgrafo) |
| `Graph<...>` | Grafo gerado em tempo de execução |
| `Sandbox`, `Budget` | **Recursos** (seção 7) |

---

## 7. Efeitos e recursos

### 7.1 Efeitos (D2)

| Efeito | Exemplo | Na recuperação | Retentativa automática |
|---|---|---|---|
| `pure` | `fn`, `let` | Recalcula | — |
| `llm` | Chamada a modelo | Reaproveita do diário | Sim, em erros temporários |
| `read` | Busca, leitura de arquivo, input humano, relógio | Reaproveita do diário | Sim |
| `sandbox` | Comando na sandbox | Restaura o snapshot da sandbox com o diário | Sim |
| `write` | Escrita idempotente (com chave) | Pode repetir com segurança | Sim |
| `write once` | Escrita não idempotente (e-mail) | Nunca repete; aplica `on_uncertain` | **Nunca** |

O efeito de um nó é **inferido**: o maior efeito de tudo o que ele chama. Ordem: `pure < llm < read < sandbox < write < write once`.

### 7.2 Timeouts padrão (D22)

| Efeito | Por tentativa |
|---|---|
| `llm` | 5 min |
| `read` | 30 s |
| `write` / `write once` | 60 s |
| `sandbox` | 10 min |

### 7.3 Recursos afins (D26)

`Sandbox`, `Budget` e capacidades `write once` têm **um dono por vez**.

- Emprestar: `reads recurso` (leitura; vários ao mesmo tempo) ou `edits recurso` (escrita; um por vez).
- Dividir explicitamente entre ramos:
  - `repo.fork(n)`: cópias isoladas (D13);
  - `repo.share(n)`: o mesmo repositório, com validação pelo conjunto de leitura a cada escrita (D13);
  - `orcamento.split(6 USD, 4 USD)` *(sintaxe provisória)*.

---

## 8. Garantias de concorrência e consistência

**Dentro de uma execução:**
- concorrência derivada das dependências; resultados independentes da ordem de execução (D7);
- isolamento por snapshot e nenhuma atualização perdida (D1);
- invariantes entre ramos: por afinidade (recursos) ou `ensures` (valores) (D25).

**Na sandbox:** `fork` (isolada) ou `share` (validação pelo conjunto de leitura) (D13).

**Em sistemas externos:** precondições `requires` validadas no momento do efeito (D29).

**Entre execuções:** entidades com uma execução por chave; leituras em paralelo, escritas exclusivas; sem ciclos de `ask` (D15, D33).

---

## 9. Modelo de execução

### 9.1 Do código à execução (D8)

| Artefato | O que é |
|---|---|
| **Template** | O código compilado: um grafo de tarefas **parametrizado** (fan-outs e laços simbólicos) |
| **Grafo realizado** | O grafo desenrolado de uma execução, **aos poucos**, à medida que os valores chegam |
| **Trace** | O diário da execução |

### 9.2 Escalonamento (D24, D31)

- Um nó fica pronto quando o seu contador de dependências chega a zero (decremento atômico).
- N workers, cada um com a sua fila; quem fica sem trabalho **rouba** da ponta mais antiga da fila de outro.
- Entre nós prontos, a prioridade é o **caminho crítico** (o caminho mais longo até o fim), com durações estimadas pelo compilador e refinadas pelo histórico.
- **E/S nunca bloqueia um worker:** a chamada vira um pedido pendente, e o worker segue.
- Ramos não escolhidos não rodam; o que deixou de ser necessário é cancelado (D32).

### 9.3 Diário e recuperação (D6, D14, D20)

- Uma entrada **por chamada** de modelo ou tool, mais timers, mensagens recebidas e escolhas não-determinísticas (vencedor de `race`, modelo do roteador).
- Escrito só no fim do arquivo, em lotes. Conteúdos grandes ficam fora, referenciados por hash.
- **Suspender, retomar e se recuperar de uma queda são a mesma operação:** reconstruir o estado a partir do diário.
- Armazenamento: formato próprio em arquivo local; PostgreSQL depois, para várias máquinas.

### 9.4 Atores do runtime (D10)

| Ator | Papel |
|---|---|
| Execução | Dona do estado de uma execução; escalona os nós |
| Chamada | Uma chamada de efeito em andamento, com timeout e retentativa |
| Entidade | Dona de um recurso compartilhado |
| Diário | Único que escreve no armazenamento |
| Supervisor | Recria do diário o que falhou |

### 9.5 Versionamento (D23)

Cada versão é um binário. Execuções terminam na versão em que começaram e só migram se o compilador provar que os grafos são compatíveis.

---

## 10. Verificações do compilador

Todas lineares ou composicionais (meta: `calyx check` em até 1 segundo):

| Verificação | Decisão |
|---|---|
| Tipos, variáveis dos prompts, variantes cobertas | D5, D11 |
| Inferência e restrição de efeitos; política de `write once` presente | D2 |
| Uso de recursos afins e suas visões | D26 |
| Escritas externas sem ordem definida (aviso) | D2 |
| Terminação: limite em laços e rodadas, `decreases` em recursão | D5, D17 |
| Teto de custo vs. orçamento (laços multiplicam pelo limite) | D3 |
| Orçamento de contexto por caminho, com invariante quando há compactação | D3, D5 |
| Redutor presente para estado escrito em paralelo | D1 |
| Ciclos de `ask` | D33 |
| Regras de `respond` / `return` | D19 |

Mensagens de erro estruturadas, com **esperado**, **observado** e **local**, para que um agente de IA consiga corrigir sozinho.

---

## 11. Ferramentas

| Comando | Função |
|---|---|
| `calyx check` | Verifica o programa (meta: até 1 s), sem gerar código |
| `calyx build` | Emite um arquivo C (runtime + grafo + efeitos) e compila para um binário nativo |
| `calyx run` | Executa; modos: `--deterministic` (uma thread), `--threads N` |
| `calyx fmt` | Formata o código |
| `calyx replay` | Reexecuta a partir de um diário, sem chamar modelos nem tools |
| `calyx trace` | Mostra o grafo realizado, custos e latências por nó |

## 12. Implementação (D10)

- **Compilador em Rust.** O verificador é compilado também como biblioteca estática com interface C e ligado ao runtime: **um verificador só**.
- **Runtime em C**, emitido junto com o programa num único arquivo C.
- Nós compilados como segmentos de uma máquina de estados, **sem pilha de chamadas do C**.
- Afinidade no lugar de coletor de lixo; contador de referências só no que é compartilhado.
- Um interpretador pequeno no runtime executa grafos gerados por LLM depois de verificados.
- O mesmo binário roda em uma thread (determinístico), várias threads, ou várias máquinas, sempre com o mesmo resultado.

## 13. Fora da v1

- Compensação (padrão *saga*) para ramos cancelados (D12).
- Políticas de roteamento além de `cheapest_that_passes` (D30).
- Várias máquinas (backend PostgreSQL do diário) (D6).
- Provas opcionais sobre grafos.
- Execução especulativa e *hedging* entre provedores.
- Edição arbitrária do grafo durante a execução (D4, nível 4).
