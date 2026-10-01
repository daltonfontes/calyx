# Especificação da Calyx (rascunho v0)

**Status:** rascunho consolidado ao fim do discovery. Reúne as 34 decisões de [`docs/discovery/03-decisoes.md`](../discovery/03-decisoes.md) num lugar só. Onde a sintaxe ainda é provisória, isso está indicado. O que o compilador já verifica hoje está no [roadmap](../roadmap.md).

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

A sintaxe **parece Python e se comporta como uma linguagem funcional**: blocos por indentação, `#` para comentários, `nome = expressão` para cada passo. Mas todo valor é imutável, cada atribuição é um passo do grafo, e a ordem das linhas não define a ordem de execução: quem define são as dependências.

```
model claude = "claude-sonnet-5-5":
    max_output 2000 tokens

tool web_search(query: Text) -> Text:
    effect read
    max_output 4000 tokens

type Plan:
    questions: List[Text] max 5

prompt split_topic(topic: Text) -> Plan:
    """
    Divida o tema abaixo em até 5 perguntas de pesquisa.
    Tema: {topic}
    """

graph research(topic: Text) -> List[Text]:
    limits threads 8, budget 2 USD

    plan = claude(split_topic(topic))
    findings = for each q in plan.questions:
        web_search(q)

    return findings
```

Exemplos completos: [`examples/research.clyx`](../../examples/research.clyx) (verificado hoje pelo compilador), [`examples/teste.clyx`](../../examples/teste.clyx) e [`examples/workflows/`](../../examples/workflows/) (usam construções dos próximos marcos).

---

## 3. Léxico

| Elemento | Forma |
|---|---|
| Comentário | `# até o fim da linha` |
| Texto | `"..."`, com interpolação `{expressão}` |
| Texto longo (prompts) | `"""..."""`, várias linhas, com interpolação |
| Números | `42`, `1_000`, `0.5` |
| Unidades | tokens (`2000 tokens`), dinheiro (`2 USD`, `100 BRL`), tempo (`30 s`, `5 min`, `3 days`), memória (`4 GB`), taxa (`50/s`) |
| Blocos | `:` no fim da linha e o conteúdo indentado com **espaços** (tab é erro); sem `{ }` e sem `;` |
| Quebra de linha | dentro de `( )` e `[ ]` a linha pode continuar na seguinte |
| Palavras-chave | em inglês; nomes, textos e comentários em qualquer língua |
| Extensão | `.clyx` |

---

## 4. Declarações de topo

### 4.1 Modelo

```
model NOME = "identificador-do-modelo"

model NOME = "identificador-do-modelo":
    max_output N tokens
```

### 4.2 Roteador (D30)

```
router NOME = route [modelo1, modelo2, modelo3]:
    policy cheapest_that_passes(verificacao)
```

Tenta os modelos na ordem dada (do mais barato ao mais caro) até a resposta passar em `verificacao` (uma função pura, `def`). A escolha feita é gravada no diário. Na v1, esta é a única política.

### 4.3 Tool

```
tool NOME(parametros) -> Tipo:
    effect read | write | write once | sandbox
    max_output N tokens                  # obrigatório para tools usadas por agentes (D16)
    timeout duração                      # opcional; há padrão por efeito (D22)
    retry_on [Erro, ...]                 # erros temporários, repetidos pelo runtime
    idempotency_key expressão            # para `write`
    on_uncertain verify(f(...)) | pause | accept_loss   # obrigatório para `write once`
    checks TipoDeEstado                  # estado validável no momento do efeito (D29)
    repeatable                           # repetir com os mesmos argumentos é legítimo (D5)
    description "texto"                  # o que a tool faz, para modelos que a chamam (agentes)
```

**Implementação (D34):** a tool roda num servidor **MCP** separado, escrito em qualquer linguagem. A declaração `tool` é o **contrato** que a Calyx verifica e que o runtime aplica (efeito, limites, timeout, retentativa, idempotência, precondições). O nome da tool e o servidor que a implementa são ligados no `calyx.toml` (seção 11.1).

### 4.4 Tipos

```
type Registro:
    campo: Tipo
    outro: List[Text] max 5

type Variante = Caso1 | Caso2(campo: Tipo)

type Apelido = Text
```

Variantes sem campos são valores: `Caso1` tem o tipo `Variante`.

### 4.5 Mensagens (D21)

```
message Nome = Caso1 | Caso2(campo: Tipo)
```

### 4.6 Prompt

```
prompt NOME(parametros) -> TipoDeSaida:
    """
    Texto com {interpolação}.
    """
```

O compilador verifica que toda `{variável}` existe (inclusive caminhos como `{pedido.cliente}`) e que a saída é decodificada para `TipoDeSaida`.

### 4.7 Função pura (D27)

```
def NOME(parametros) -> Tipo:
    corpo
    return expressão
```

Sem efeitos; o compilador pode recalculá-la à vontade; não vai para o diário. Dentro de `def`, `x = ...` cria um valor novo; nada é alterado no lugar.

### 4.8 Grafo

```
graph NOME(parametros) -> Tipo:
    effect EFEITO_MAXIMO                 # opcional
    decreases PARAMETRO                  # obrigatório se o grafo chama a si mesmo
    corpo
```

- `effect` restringe o efeito máximo do grafo; o compilador verifica.
- `decreases` indica o parâmetro que diminui a cada chamada recursiva (D17).

### 4.9 Entidade (D15)

```
entity NOME(key CHAVE: Tipo):
    state CAMPO: Tipo = valor_inicial

    on Mensagem(parametros) -> Tipo:     # leitura: pode rodar em paralelo com outras leituras
        return ...

    on Mensagem(parametros):             # escrita: exclusiva
        next CAMPO = ...
```

No máximo **uma** entidade aberta por chave. Se um handler altera `state`, ele é de escrita; caso contrário, de leitura (inferido pelo compilador).

---

## 5. O corpo de um grafo

### 5.1 Limites (D3)

```
limits threads 8, rate 50/s, budget 2 USD, memory 4 GB
```

Todos opcionais. O programador **limita** a concorrência, nunca a cria.

| Limite | Significado | Padrão |
|---|---|---|
| `threads N` | Chamadas (de modelo e de tool) em andamento ao mesmo tempo | 8 |
| `rate N/s`, `N/min`, `N/h` | Chamadas iniciadas por unidade de tempo (ex.: `rate 15/min` para o plano gratuito do Gemini) | sem limite |
| `budget N USD` | Custo máximo da execução; os preços por modelo vêm do `calyx.toml`. Esgotado, a execução para e pode continuar com `calyx resume <id> --budget <maior>` | sem limite |
| `memory` | *(ainda não aplicado)* | — |

Os limites que valem são os do grafo executado; os de subgrafos ainda são ignorados.

### 5.2 Passos e valores

| Forma | Significado |
|---|---|
| `x = expr` | Um passo do grafo. Se tem efeito, vai para o diário; se é puro, o compilador sabe que pode recalcular (D27) |
| `xs = for each i in lista: expr` | Fan-out: um passo por item; `xs` é uma lista **na ordem de `lista`** (D7). O corpo pode vir na linha seguinte, indentado |
| `return expr` | Resultado do grafo |
| `respond expr` | Entrega o resultado antes do fim; o resto do grafo continua em segundo plano. Grafo com `respond` não tem `return` de valor; no máximo um `respond` por caminho (D19) |

Cada nome é atribuído uma vez. A ordem das linhas não importa: o compilador ordena os passos pelas dependências e recusa ciclos.

### 5.3 Chamada a modelo (D28)

| Forma | Resultado |
|---|---|
| `modelo(prompt(...))` | Valor do tipo de saída do prompt |
| `modelo(prompt(...), continue=conversa)` | `Reply[T]`, com `.value` e `.conversation` |
| `modelo(prompt(...), continue=new)` | Começa uma conversa nova |

### 5.4 Escolha

```
x = if condição:
    expr
else:
    expr

match expr:
    case Caso1:
        ...
    case Caso2(campo):
        ...
```

`match` precisa cobrir todas as variantes (ou ter `case _`). **Passos de um ramo não escolhido nunca rodam** (D32).

- Os nomes de um `case` ligam os campos **pela posição**, como no Python: `case Rejected(f)` liga o primeiro campo a `f`; `_` ignora um campo. Um nome não pode esconder outro (parâmetro, passo ou nome de um `case` de fora).
- Cada ramo é uma expressão, na mesma linha ou indentada; todos dão o mesmo tipo.
- `if` exige `else` (é uma expressão: sempre dá um valor). A condição é um `Bool`.

**Operadores:** `+ - * /` para números (e `Money`, `Duration`); `+` também junta textos e listas; `== !=` para valores do mesmo tipo; `< <= > >=` para números, dinheiro, durações e textos; `and`, `or`, `not` para `Bool` (`and`/`or` não avaliam o lado direito quando não precisam). Parênteses agrupam.

**Construir valores:** `Ponto(x=1, y=2)` para registros e `Rejected(feedback="...")` para variantes, sempre com os campos pelo nome (um campo único pode ir pela posição). Uma variante sem campos é o próprio nome: `Approved`.

### 5.5 Laço (D5)

```
final = loop x = valor_inicial, max N:
    ...
    done valor        # termina com este valor
    next valor        # próxima volta com este valor
    on limit: last | fail "motivo"
```

O limite é obrigatório. `on limit` define o que acontece se ele for atingido; sem ele, o laço falha.

- O corpo é **uma expressão** que termina em `done` ou `next`, diretamente ou em cada ramo de um `match` ou `if` (o compilador confere).
- `next` precisa ter o tipo do valor inicial; `on limit: last` exige que `done` dê o mesmo tipo.
- Cada volta é um lugar próprio no grafo realizado: as chamadas da volta `k` têm chaves `passo#laço.k#…` no diário, então um laço interrompido retoma na volta em que estava.

### 5.6 Rodadas (D18)

```
final = rounds N, carry x = valor_inicial:
    passo = for each r in participantes:
        ...
    next novo_valor
```

Como o `loop`, mas com **barreira** no fim de cada rodada: só ali os resultados de uma rodada ficam visíveis para a próxima.

### 5.7 Agente (D5)

```
resultado = agent modelo:
    tools [tool1, tool2(reads recurso), tool3(edits recurso)]
    max_turns N
    task prompt(...)
    compact with prompt_de_resumo        # opcional; compactação por tamanho
    on turn_limit: final_answer | fail "motivo"
    on stuck: final_answer | fail "motivo"
```

`agent` é atalho: o compilador o expande num ciclo explícito `modelo → tools → observação`. As variantes `turn_limit` e `stuck` (mesma tool, mesmos argumentos, repetidamente) são obrigatórias.

**Como está implementado (M5):**

- **Chamada de tools nativa do provedor** (*function calling* no formato da OpenAI). O esquema dos argumentos vem dos parâmetros da tool; a descrição, da propriedade `description` da tool.
- **Toda volta manda a conversa inteira.** A mensagem do modelo volta **exatamente como veio**: provedores anexam dados a ela (a assinatura de raciocínio do Gemini) que precisam voltar.
- **Tools pedidas numa mesma volta rodam em paralelo.** Uma tool que falha vira uma observação ("error: ...") para o modelo, não uma falha da execução.
- **`stuck`:** as mesmas chamadas (tool e argumentos) em três voltas seguidas; tools marcadas `repeatable` não contam.
- **`final_answer`:** uma última chamada, sem tools, pedindo a resposta com o que o agente já sabe.
- **Tipo da resposta:** se o `task` não devolve `Text`, uma chamada a mais converte a resposta final para o tipo do prompt.
- **Exigências do compilador:** toda tool usada por um agente declara `max_output` (D16); `max_turns`, `task`, `on turn_limit` e `on stuck` são obrigatórios; `compact` e o empréstimo de recursos ficam para depois.
- **Diário:** cada volta e cada chamada de tool têm a sua chave (`passo#agente.t2`, `passo#agente.t2.c0`), então um agente interrompido retoma na volta em que estava, e o `replay` reproduz a conversa inteira sem chamar nada.

### 5.8 Corrida (D12)

```
vencedor = race first where condição:
    a: expr
    b: expr
    on none: fail "motivo"
```

Os ramos rodam em paralelo; vence o primeiro que satisfaz a condição (`it` é o resultado de cada ramo); os outros são cancelados entre passos. O vencedor é gravado no diário. Recursos passados aos ramos são consumidos; só os do vencedor voltam.

### 5.9 Falha como valor (D11)

```
r = try expr                 # Ok(valor) | Failed(erro)
bons = lista.ok()
falhas = lista.failed()
```

O compilador obriga a tratar `Failed` antes de usar o valor.

- O tipo de `try e` é `Result[T]`, com as variantes `Ok(value: T)` e `Failed(error: Text)`, desmontadas com `match`.
- Também existe na forma de bloco: `try:` seguido da expressão indentada.
- `try` captura falhas de chamadas (depois das novas tentativas), de subgrafos, de laços que atingem o limite com `fail` e de agentes. Não captura falhas que precisam parar a execução: diário corrompido, programa diferente na retomada, `write once` com resultado incerto.

### 5.10 Ordem entre efeitos (D2)

```
notificar after salvar
```

Aresta de ordem, sem dados. O compilador avisa quando dois passos com efeito de escrita externa não têm ordem definida. Recursos com dono (seção 7) já geram ordem sozinhos.

### 5.11 Precondições e invariantes (D29, D25)

```
pago = refund(pedido, valor):
    requires state.status == Delivered
    requires state.refunded + valor <= state.total

gastos = for each i in itens: ...
ensures sum(gastos) <= limite
```

- `requires`: avaliado **pela tool**, sobre o estado atual, na mesma transação do efeito. Só operadores permitidos (comparação, aritmética, pertencimento). Pode ser escrito pelo programador ou vir de um LLM como saída tipada.
- `ensures`: avaliado na junção; se falhar, produz um valor de conflito.

### 5.12 Mensagens (D21)

```
aprovacao = receive Approval, timeout 3 days:
    on timeout: Denied(reason="expirou")
fatos = ask Memoria(usuario).Recall(texto)     # síncrono
send Memoria(usuario).Remember(novos)          # assíncrono
```

O compilador recusa ciclos de `ask` (D33).

### 5.13 Grafos gerados por LLM (D4)

```
type Plano = Graph[T]:
    tools [busca, leitura]
    models [worker]
    max_effect read
    max_nodes 20

plano = planner(make_plan(pedido))
resultado = try run(plano)
```

`run` passa o grafo pelo mesmo verificador do compilador antes de executar. O tipo `Graph[...]` limita o que o LLM pode gerar.

### 5.14 Estado nomeado (D1) — *sintaxe provisória*

```
state gasto: Money = 0 USD, merge sum
```

Ramos paralelos veem o valor do momento da bifurcação (snapshot) e suas escritas se juntam pelo redutor (`merge`) na junção. Redutor é obrigatório para estado escrito por ramos concorrentes.

---

## 6. Tipos

| Tipo | Uso |
|---|---|
| `Text`, `Nat`, `Int`, `Float`, `Bool`, `Money`, `Duration`, `Date` | Básicos |
| `List[T]`, `List[T] max N` | Listas; o limite entra nas análises de custo |
| `Map[K, V]` | Mapas |
| Registros e variantes | Seção 4.4 |
| `Conversation` | Histórico de conversa com um modelo; valor imutável (D3) |
| `Reply[T]` | Resposta com `.value: T` e `.conversation` |
| `Ok(T) \| Failed(Erro)` | Resultado de `try` |
| `Prompt[T]` | Prompt como valor (ex.: passado a um subgrafo) |
| `Graph[T]` | Grafo gerado em tempo de execução |
| `Sandbox`, `Budget` | **Recursos** (seção 7) |

---

## 7. Efeitos e recursos

### 7.1 Efeitos (D2)

| Efeito | Exemplo | Na recuperação | Retentativa automática |
|---|---|---|---|
| `pure` | `def`, passos sem chamadas | Recalcula | — |
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

**Como está implementado (M4):**

- **Tarefas:** cada passo é uma tarefa, e cada item de um `for each` também. Uma tarefa fica pronta quando os passos que ela lê terminam.
- **Workers:** um por núcleo (no máximo 8), cada um com uma fila em ordem de prioridade; sem trabalho, um worker pega da fila dos outros.
- **Prioridade:** o compilador calcula para cada passo o caminho mais longo até o fim do grafo, estimando 3 s por chamada de modelo ou subgrafo e 1 s por chamada de tool. Workers e chamadas esperando vaga seguem essa ordem; em empate, a ordem de criação.
- **Chamadas:** um worker nunca espera a rede. A chamada vai para uma thread de E/S (no máximo `threads` ao mesmo tempo), e a tarefa para ali. Quando a resposta chega, a tarefa roda de novo desde o início; as chamadas que já terminaram vêm da memória, pelas mesmas chaves do diário. Assim não é preciso guardar pilha de C durante a espera.
- **Dentro de um passo:** argumentos independentes são avaliados juntos, então chamadas independentes num mesmo passo também saem em paralelo.
- **Ordem dos resultados:** a de `for each`, nunca a de término (D7). O resultado é o mesmo com uma ou muitas threads; `--deterministic` roda uma chamada por vez, sempre na mesma ordem.
- **Numa falha:** nada novo começa; as chamadas em andamento terminam e entram no diário antes de a execução parar.

### 9.3 Diário e recuperação (D6, D14, D20)

- Uma entrada **por chamada** de modelo ou tool, mais timers, mensagens recebidas e escolhas não-determinísticas (vencedor de `race`, modelo do roteador).
- Escrito só no fim do arquivo, em lotes. Conteúdos grandes ficam fora, referenciados por hash.
- **Suspender, retomar e se recuperar de uma queda são a mesma operação:** reconstruir o estado a partir do diário.
- Armazenamento: formato próprio em arquivo local; PostgreSQL depois, para várias máquinas.

**Como está implementado (M3):**

- Cada execução tem um diretório `.calyx/runs/<id>/` (no diretório atual) com `journal.jsonl`, uma linha JSON por entrada, e `blobs/`, com as respostas acima de 4 KB guardadas pelo SHA-256 do conteúdo.
- **Chave de cada chamada:** o lugar dela no grafo realizado: grafo, passo, item do fan-out e posição dentro do passo (`research/answers[2]#1`; num subgrafo, `research/x#0/sub/y#0`). Não depende da ordem em que as chamadas rodaram, então continua valendo com paralelismo (M4).
- Cada entrada guarda também o **hash do pedido**. Na retomada, um pedido diferente sob a mesma chave significa que a execução não é determinística, e ela para em vez de misturar resultados.
- O diário guarda o **hash do programa compilado**. Uma execução só continua com o mesmo programa (D23); se o código mudou, a retomada é recusada.
- Só respostas aceitas entram no diário (uma resposta que não decodifica no tipo do prompt é pedida de novo e não é gravada).
- **Durabilidade:** cada entrada chega ao sistema operacional antes da próxima chamada começar, então uma queda do processo não perde nada já concluído. O `fsync` (que protege também de queda de energia) roda no máximo uma vez por segundo, e sempre antes de escritas externas e no fim.
- **`write once`:** uma entrada `begin` é gravada (com `fsync`) antes da chamada. Se a execução cai entre o `begin` e o fim da chamada, o resultado é desconhecido: a retomada **não repete** a chamada e para, à espera das políticas `on_uncertain` (M6).
- Uma linha cortada no meio por uma queda é descartada na retomada.

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
| `calyx build` | Gera um executável autocontido para um grafo: `calyx build arquivo.clyx [-o nome] [--graph g]`. É uma cópia do próprio `calyx` com o programa e o `calyx.toml` dentro (D35); não precisa de compilador C, nem de Calyx onde roda. Os parâmetros do grafo viram opções (`./nome --param valor`), e `./nome resume <id>`, `replay` e `runs` funcionam como no `calyx` |
| `calyx run` | Executa um grafo: `calyx run arquivo.clyx --param valor`. Chamadas independentes rodam em paralelo; `--deterministic` roda uma por vez; `--budget` troca o orçamento |
| `calyx fmt` | Formata o código |
| `calyx resume` | Continua uma execução interrompida ou que falhou: `calyx resume <id>`. Chamadas já no diário não são feitas (nem pagas) de novo. `--budget` aumenta um orçamento esgotado |
| `calyx replay` | Reexecuta a partir de um diário, sem chamar modelos nem tools: `calyx replay <id>` |
| `calyx runs` | Lista as execuções, com estado (`finished`, `failed`, `interrupted`), chamadas e retomadas |
| `calyx trace` | Mostra o grafo realizado, custos e latências por nó |

### 11.1 Configuração do projeto (`calyx.toml`)

Procurado no diretório do programa e nos diretórios acima. O programa fixa o identificador do modelo (D23); a configuração só diz **para onde** mandar cada chamada.

```toml
[providers.nvidia]                       # API no formato da OpenAI
url = "https://integrate.api.nvidia.com/v1"
key_env = "NVIDIA_API_KEY"               # a chave vem do ambiente, nunca do arquivo
models = ["meta/", "nvidia/"]            # prefixos dos identificadores que este provedor atende

[tools.web_search]                       # servidor MCP que implementa a tool
command = ["python3", "tools/search.py"] # relativo ao calyx.toml
name = "search"                          # opcional: nome da tool no servidor
```

Preços (para `budget`), em USD por milhão de tokens:

```toml
[prices."gemini-3.5-flash-lite"]   # valores ilustrativos: use os da tabela do provedor
input = 0.10
output = 0.40
```

Sem preço para um modelo, as chamadas dele não contam no orçamento, e o runtime avisa.

Provedores embutidos: `gemini-*` / `gemma-*` (`GEMINI_API_KEY`), `gpt-*` / `o1*` / `o3*` / `o4*` (`OPENAI_API_KEY`), `nvidia/*` (`NVIDIA_API_KEY`). Identificadores que começam com `fake` (e a opção `--fake-models`) usam um modelo falso, sem rede, que responde no formato do tipo do prompt.

### 11.2 Como o runtime chama modelos e tools

- **Modelo:** o prompt é preenchido com os argumentos (texto como está; listas, um item por linha; registros, em JSON). Se o prompt não devolve `Text`, o tipo de saída vira um **JSON Schema** enviado junto, e a resposta é decodificada nesse tipo.
- **A resposta é conferida contra o tipo** (tipos, campos obrigatórios, valores permitidos, `max` de listas, variantes). Modelos nem sempre respeitam o esquema que recebem; uma resposta fora do tipo conta como erro temporário e o modelo é chamado de novo.
- **Variantes:** um tipo só com variantes sem campos (`Optimist | Skeptic`) vira um texto com um dos nomes; um tipo com campos vira um objeto com `kind` e os campos daquela variante, com uma alternativa por variante no esquema (`anyOf`).
- **Novas tentativas:** erros temporários (`Timeout`, `RateLimit`, `Unavailable`, `Network`) de modelos são repetidos até 4 tentativas, esperando 1 s, 2 s e 4 s (o dobro para `RateLimit`), ou mais, se o provedor pedir (cabeçalho `Retry-After` ou "retry in N s" na mensagem, até 60 s). Tools repetem só os erros listados em `retry_on`; `write once` nunca repete (D2).
- **Saída de tools:** cortada em `max_output` (D16).
- **Falha:** se um passo falha depois das tentativas, a execução para com o grafo, o passo e o motivo. Falha como valor (`try`, D11) chega no M5.

## 12. Implementação (D10)

- **Compilador em Rust.** O verificador é compilado também como biblioteca estática com interface C e ligado ao runtime: **um verificador só**.
- **Runtime em C**, emitido junto com o programa num único arquivo C.
- Nós compilados como segmentos de uma máquina de estados, **sem pilha de chamadas do C**.
- Afinidade no lugar de coletor de lixo; contador de referências só no que é compartilhado.
- Um interpretador pequeno no runtime executa grafos gerados por LLM depois de verificados. Ele já existe (M2): hoje executa todos os programas, a partir da representação intermediária em JSON (`calyx check --ir-json`).
- O que C faz mal fica numa camada em Rust ligada ao runtime: HTTPS, JSON das APIs de modelo e o cliente MCP. O interpretador continua dono das políticas (novas tentativas, timeouts por efeito, decodificação das respostas).
- O mesmo binário roda em uma thread (determinístico), várias threads, ou várias máquinas, sempre com o mesmo resultado.

## 13. Fora da v1

- Compensação (padrão *saga*) para ramos cancelados (D12).
- Políticas de roteamento além de `cheapest_that_passes` (D30).
- Várias máquinas (backend PostgreSQL do diário) (D6).
- Provas opcionais sobre grafos.
- Execução especulativa e *hedging* entre provedores.
- Edição arbitrária do grafo durante a execução (D4, nível 4).
