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

**Como está implementado (M10):**

```
model lite = "gemini-3.5-flash-lite"
model flash = "gemini-3.5-flash"

def confident(t: Triage) -> Bool:
    return t.confidence >= 95 and len(t.summary) > 0

router triager = route [lite, flash]:
    policy cheapest_that_passes(confident)

result = try triager(triage(ticket))
```

- **Chamado como um modelo:** `roteador(prompt(...))`, com o tipo da resposta do prompt.
- **Um modelo por vez**, do mais barato ao mais caro; o seguinte só é chamado quando o anterior falha (depois das novas tentativas) ou a resposta dele não passa na verificação. Cada tentativa tem a sua chave no diário (`passo#id.0`, `passo#id.1`), e o modelo escolhido também (`passo#id`): a retomada e o `replay` não refazem a escolha.
- **Nenhuma resposta passou:** a chamada falha ("no model's answer passed"), e `try` captura. O exemplo manda o chamado para uma pessoa.
- **O compilador confere:** pelo menos dois modelos declarados (`E0690`, `E0691`), a política `cheapest_that_passes` (`E0692`), a verificação como um `def` que recebe a resposta e dá `Bool` (`E0693`), e, em cada chamada, que o prompt responde o tipo que a verificação recebe (`E0694`).
- Agentes ainda usam um modelo só (`agent modelo:`), não um roteador.

### 4.3 Tool

```
tool NOME(parametros) -> Tipo:
    effect read | write | write once | sandbox
    max_output N tokens                  # obrigatório para tools usadas por agentes (D16)
    timeout duração                      # opcional; há padrão por efeito (D22)
    retry_on [Erro, ...]                 # erros temporários, repetidos pelo runtime
    idempotency_key expressão            # para `write`
    batch parametro                      # `write once` em lote: refaz só os itens que faltam
    on_uncertain verify(f(...)) | pause | accept_loss   # obrigatório para `write once`
    checks TipoDeEstado                  # estado validável no momento do efeito (D29)
    repeatable                           # repetir com os mesmos argumentos é legítimo (D5)
    description "texto"                  # o que a tool faz, para modelos que a chamam (agentes)
```

**Contratos de escrita (implementados no M6a):**

- `idempotency_key param`: o valor desse parâmetro vai para a tool como chave; a mesma chave nunca pode ser aplicada duas vezes. Com a chave, o runtime repete a escrita em erros temporários e na retomada. Uma tool `write` sem chave recebe o aviso `W0601`.
- `on_uncertain`: obrigatório para `write once` (`E0304`) e só para elas (`E0641`). Vale quando a chamada **pode ter acontecido** sem que a resposta chegasse: timeout, servidor que caiu, ou a execução que caiu entre o início e o fim da chamada.
  - `pause`: a execução para; uma pessoa confere e retoma com `calyx resume <id> --uncertain done|retry|failed`.
  - `accept_loss`: segue como se a chamada tivesse acontecido.
  - `verify(f(a, b))`: chama a tool `f` (`effect read`, `E0631`), com parâmetros da própria tool (`E0632`). Se `f` devolve `Bool`: `true` segue, `false` faz a chamada de novo. Se `f` devolve `List[T]`, com `T` o tipo da resposta da tool (`E0633`), ela **acha de novo o que a chamada fez**: vazia, faz a chamada de novo; senão, o primeiro item é a resposta da chamada (o id do ticket criado, do e-mail enviado), gravado no diário como se tivesse chegado. Se a verificação falha, para como `pause`.
  - `accept_loss` e o `verify` com `Bool` seguem sem a resposta da tool, então exigem uma tool que devolve `Unit` (`E0634`); o `verify` com `List[T]` não. Ao retomar uma pausa, `--uncertain done` também segue sem resposta; para uma tool que devolve algo, a pessoa dá a resposta que achou: `--uncertain done=<resposta>` (JSON, ou o texto como está).
- `batch p`: a `write once` aplica os itens da lista `p` (`List[T]`) um a um, e pode ficar pela metade. Exige `on_uncertain verify(f(...))` com `f` devolvendo `List[T]`, os itens já aplicados, e uma tool que devolve `Unit` (`E0637`). Quando a chamada pode ter acontecido em parte, o runtime pergunta a `f` e manda de novo só os itens que ela não achou (cada item achado casa com um item pedido); se achou todos, segue.
- `checks Tipo`: o registro com o estado que a tool valida nas precondições (`E0635`).

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

**Como está implementado (M7):**

```
def check(c: Contract) -> List[Text]:
    problems = []
    if len(c.parties) < 2:
        problems = problems + ["menos de duas partes"]
    elif c.value > 1000:
        problems = problems + ["valor alto"]
    return problems
```

- O corpo tem linhas `nome = valor`, `if` / `elif` / `else` com blocos e um `return` na última linha (`E0665`). Um nome pode receber um valor novo, do mesmo tipo (`E0664`; listas podem crescer). Um nome novo criado dentro de um `if` só existe depois dele se todos os ramos lhe dão valor.
- **Puro:** nada de modelos, tools, grafos ou entidades (`E0660`). Pode chamar outros `def`s, mas **sem recursão** (`E0661`): todo `def` termina. Repetição fica para `loop` ou para grafos com `decreases` (D17).
- Usável em qualquer lugar: grafos, handlers de entidades, outros `def`s.
- **Listas por compreensão:** `[f.content for f in facts if f.topic == t]`. Também puras (`E0663`); para chamadas por item, `for each`.
- **`in`:** `x in lista`, `"parte" in texto`.
- **Literais:** `true` e `false`.
- **Funções embutidas:** `len` (itens ou caracteres), `take(lista, n)`, `sum(lista)`, `join(textos, separador)`, `lower`, `upper` (com letras acentuadas), `trim`. Um nome declarado no programa tem prioridade sobre elas. Argumentos errados: `E0662`.

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

**Como está implementado (M6c):**

- **Handlers são puros:** calculam com o estado, a chave e a mensagem; não chamam modelos, tools, grafos nem outras entidades (`E0654`). As chamadas com efeito ficam no grafo, que manda o resultado. Por isso nenhum handler espera nada, e ciclos de `ask` (D33) não podem existir.
- Um handler **responde** (`-> T` e um `return`) ou **muda o estado** (linhas `next campo = valor`, todas calculadas sobre o estado anterior à mensagem); nunca os dois (`E0652`). `next` só em campos do estado, uma vez cada, com o tipo do campo (`E0653`). O valor inicial de cada campo é um valor escrito no programa (`E0651`).
- **Onde vive:** `.calyx/entities/<Entidade>/<hash da chave>/entity.json` (no diretório em que o programa roda), com o estado e as mensagens já aplicadas, trocados juntos de forma atômica.
- **Um dono por chave, entre processos:** `flock`; perguntas compartilham a trava, mudanças a têm sozinhas. 50 execuções simultâneas mandando ao mesmo contador terminam com 50.
- **Cada mensagem é aplicada uma vez:** o id de uma mensagem é a execução e o lugar da chamada; uma execução retomada que manda de novo encontra o id e não aplica outra vez.

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
- Cada ramo é uma expressão na mesma linha, ou um bloco indentado: passos (`nome = valor`, cada um vendo os anteriores, com `requires` depois de uma chamada a tool) e, por último, o valor do ramo. Os passos de um ramo rodam **na ordem do texto**: um e-mail escrito depois de um pagamento só sai depois dele. Todos os ramos dão o mesmo tipo. O mesmo vale para `if` e `else`.
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

- O corpo são **passos** (`nome = valor`, cada um vendo os anteriores; `nome = for each x in lista: ...` roda os itens ao mesmo tempo) seguidos de **uma expressão** que termina em `done` ou `next`, diretamente ou em cada ramo de um `match` ou `if` (o compilador confere).
- `next` precisa ter o tipo do valor inicial; `on limit: last` exige que `done` dê o mesmo tipo.
- Cada volta é um lugar próprio no grafo realizado: as chamadas da volta `k` têm chaves `passo#laço.k#…` no diário, então um laço interrompido retoma na volta em que estava.
- Por isso uma chamada `write once` no corpo é uma escrita nova a cada volta. Se os argumentos dela não usam nada que muda de volta para volta (a variável do laço, um passo do corpo), o compilador avisa (`W0605`): é o pagamento repetido a cada nova tentativa. O certo é escrever depois do laço, com o resultado dele. Vale também para `rounds`.

### 5.6 Rodadas (D18)

```
final = rounds N, carry x = valor_inicial:
    passo = for each r in participantes:
        ...
    next novo_valor
```

Como o `loop`, mas com **barreira** no fim de cada rodada: só ali os resultados de uma rodada ficam visíveis para a próxima.

**Como está implementado (M9):**

```
final = rounds 2, carry answers = start:
    turn = for each r in roles:
        gemini(rebut(r, question, answers))
    next turn
```

- **Sempre `N` rodadas**, e o valor é o último carregado; `done valor` (do mesmo tipo, `E0685`) termina antes. Não há `on limit`: chegar a `N` é o fim normal.
- **A barreira não precisa de mecanismo próprio:** dentro de uma rodada só existe o valor carregado da anterior, e o `next` só tem valor quando todos os itens do `for each` terminaram. Ninguém vê uma resposta pela metade.
- Os itens de um `for each` dentro do corpo rodam ao mesmo tempo, cada um com a sua chave no diário (`passo#rodadas.k#for[j]#…`): uma execução interrompida continua na rodada e no item em que estava.
- Uma lista carregada pode mudar de tamanho entre as rodadas (o tipo do valor carregado não guarda o `max` da lista inicial).

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
- **Exigências do compilador:** toda tool usada por um agente declara `max_output` (D16); `max_turns`, `task`, `on turn_limit` e `on stuck` são obrigatórios; tools `write once` não são aceitas (`E0640`); `compact` fica para depois.
- **Empréstimo de recursos:** `tools [read_file(reads repo), edit_file(edits repo)]`. Cada sandbox que a tool pede é emprestada a todas as chamadas do agente (`E0649` se faltar); o modelo não vê nem escolhe esse parâmetro.
- **Diário:** cada volta e cada chamada de tool têm a sua chave (`passo#agente.t2`, `passo#agente.t2.c0`), então um agente interrompido retoma na volta em que estava, e o `replay` reproduz a conversa inteira sem chamar nada.

### 5.8 Corrida (D12)

```
vencedor = race first where condição:
    a: expr
    b: expr
    on none: fail "motivo"
```

Os ramos rodam em paralelo; vence o primeiro que satisfaz a condição (`it` é o resultado de cada ramo); os outros são cancelados entre passos. O vencedor é gravado no diário. Recursos passados aos ramos são consumidos; só os do vencedor voltam.

**Como está implementado (M9):**

```
best = race first where it.confident:
    direct: gemini(direct(question))
    sources: with_sources(question)
    on none: Answer(answer="nenhuma estratégia teve certeza", confident=false)
```

- **Ramos:** dois ou mais (`E0680`), com nomes diferentes (`E0681`) e do mesmo tipo (`E0615`). Cada um é uma expressão, normalmente uma chamada a um subgrafo. `where` é opcional: sem ele, vence o primeiro ramo que não falha.
- **A condição é pura** (`E0682`): operadores e `def`s sobre `it`. Uma condição que chamasse um modelo pagaria uma chamada por ramo e mudaria a cada execução.
- **`on none` é obrigatório** (`E0683`): `fail "motivo"` ou um valor do tipo dos ramos (`E0684`). Vale quando todos os ramos terminaram e nenhum passou; um ramo que falha perde.
- **O vencedor vai para o diário** com o seu valor. A retomada e o `replay` não disputam a corrida de novo, mesmo que outro ramo terminasse primeiro desta vez.
- **Cancelamento entre passos:** os subgrafos dos ramos perdedores param (as tarefas deles não rodam mais) e as chamadas que ainda esperavam a vez não são feitas. Uma chamada já em andamento termina, e a resposta não é usada (o rastro diz "lost the race"); a execução espera por ela antes de terminar.
- **Escritas nos ramos:** aviso `W0604`. Um ramo que perde pode já ter escrito, e uma escrita em andamento termina quando a corrida é decidida. Compensação (*saga*) fica para depois; o caminho seguro é escrever depois da corrida, com o vencedor.
- **Sandboxes:** dois ramos não podem editar a mesma sandbox (`E0645`). `fork` (uma cópia por ramo) fica para depois.

### 5.9 Falha como valor (D11)

```
r = try expr                 # Ok(valor) | Failed(erro)
bons = lista.ok()
falhas = lista.failed()
```

O compilador obriga a tratar `Failed` antes de usar o valor.

- O tipo de `try e` é `Result[T]`, com as variantes `Ok(value: T)` e `Failed(error: Text)`, desmontadas com `match`.
- Também existe na forma de bloco: `try:` seguido da expressão indentada.
- `try` captura falhas de chamadas (depois das novas tentativas), de subgrafos, de laços que atingem o limite com `fail` e de agentes. Não captura falhas que precisam parar a execução: diário corrompido, programa diferente na retomada, `write once` com resultado incerto, e erros de configuração (chave de API ausente, tool sem servidor no `calyx.toml`): esses não dependem dos dados, e um `try` ou um roteador que os tratasse como uma chamada que falhou esconderia o problema.

### 5.10 Ordem entre efeitos (D2)

```
notificar after salvar
notificar after salvar, cobrar
```

Aresta de ordem, sem dados: `notificar` só começa depois que os passos listados terminaram. O compilador avisa (`W0602`) quando dois passos com efeito de escrita externa (`write`, `write once`) não têm ordem definida, nem por dados nem por `after`. Nomes que não são passos e um passo depois de si mesmo são erros (`E0639`); ordens circulares, `E0506`. Recursos com dono (seção 7) já geram ordem sozinhos (M6b).

### 5.11 Precondições e invariantes (D29, D25)

```
pago = refund(pedido, valor):
    requires state.status == Delivered
    requires state.refunded + valor <= state.total

gastos = for each i in itens: ...
ensures sum(gastos) <= limite
```

- `requires`: avaliado **pela tool**, sobre o estado atual, na mesma transação do efeito. Só operadores permitidos (comparação, aritmética, pertencimento). Pode ser escrito pelo programador ou vir de um LLM como saída tipada.
  - **Como está implementado (M6a):** a tool precisa ser de escrita e declarar `checks Tipo` (`E0636`); `state.campo` são os campos desse tipo; o resto são valores do grafo, calculados antes da chamada. Só valores, campos, comparações, `+ - * /`, `and`, `or` e `not` (`E0638`); cada condição é `Bool`. Também na forma `r = try tool(...):` com as linhas `requires`.
  - A tool recebe as condições como árvore (seção 11.2). Se uma não vale, ela não faz nada e responde `PreconditionFailed`: uma falha local, que o `try` captura e que nunca é repetida.
  - Pertencimento (`in`) e precondições vindas de um LLM ainda não estão implementados.
- `ensures`: avaliado na junção; se falhar, produz um valor de conflito.

### 5.12 Mensagens (D21)

```
aprovacao = receive Approval, timeout 3 days:
    on timeout: Denied(reason="expirou")
fatos = ask Memoria(usuario).Recall(texto)     # síncrono
send Memoria(usuario).Remember(novos)          # assíncrono
```

O compilador recusa ciclos de `ask` (D33).

**Como está implementado (M8): `receive`.**

```
message Approval = Approved | Denied(reason: Text)

approval = receive Approval, timeout 3 days:
    on timeout: Denied(reason="ninguém respondeu")
```

- `receive Approval about proposta, timeout 3 days:` diz **sobre o que** é a mensagem. A espera só começa quando esse valor existe (o prazo não corre enquanto a proposta é escrita), e o valor fica gravado com a espera (`waits.jsonl`) e é mostrado quando a execução para, para quem vai responder. Sem `about`, a espera começa assim que nada a impede, mesmo antes de passos de que ela não depende.
- Só tipos declarados com `message` (`E0670`). `timeout` e `on timeout` são obrigatórios: uma execução nunca espera para sempre (`E0671`, `E0672`).
- **A execução para de verdade:** quando nada mais pode rodar, ela sai com o estado `waiting` (código 4). O prazo absoluto (agora + `timeout`) é gravado uma vez em `waits.jsonl`, no diretório da execução: vale mesmo se a máquina reiniciar.
- `calyx deliver <id> Approval '<json>'` confere a mensagem contra o tipo e a entrega ao `receive` mais antigo que espera por ela (`inbox.jsonl`, com a hora da entrega). Só para execuções que estão esperando, e só **antes do prazo**: o prazo é a hora em que a resposta tinha de chegar, não a hora em que a execução é retomada. Uma resposta que chega depois é recusada mesmo que ninguém tenha retomado a execução ainda, e o runtime confere a hora de novo ao tomar a mensagem.
- `calyx resume <id>` continua; `calyx tick` continua toda execução que recebeu mensagem ou cujo prazo venceu, e foi feito para rodar num agendador (cron): **não há servidor**. Vencido o prazo, o valor é o de `on timeout`.
- O que o `receive` recebeu (ou o valor de `on timeout`) vai para o diário: a retomada e o `replay` não precisam da mensagem de novo. Precisa de diário (não roda com `--no-journal`).

**Como está implementado (M6c):** `ask` e `send`.

- `ask` só para handlers que respondem; `send` só para os que mudam o estado (`E0656`); entidade e mensagem precisam existir (`E0655`). `send` pode ser uma linha sozinha.
- **Ordem dentro de uma execução:** mensagens à mesma entidade seguem a ordem do texto, como os empréstimos de sandbox: um `send` depois de toda mensagem anterior a ela, um `ask` depois de todo `send` anterior. A execução vê as próprias mudanças.
- **Diário:** a resposta de um `ask` e a confirmação de um `send` entram no diário; a retomada e o `replay` usam o diário, e o `replay` não manda nada.
- **Aviso `W0603` (atualização perdida):** um `send` cujo valor depende de um `ask` à mesma entidade. Outra execução pode mudar a entidade entre os dois; a conta deve ser feita num handler, sobre o estado atual (`next saldo = saldo + valor`). Não há aviso quando o handler da mensagem aplica uma mudança, não um valor novo: cada `next` que usa a mensagem usa também o valor atual do campo (`next notas = notas + [nota]`). Aí nada se perde, venha de onde vier o argumento.
- O `send` é aplicado durante a execução (a resposta é só a confirmação), não numa fila: a execução não espera nenhum outro efeito por causa dele.

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
- Dividir explicitamente entre ramos *(ainda não implementado)*:
  - `repo.fork(n)`: cópias isoladas (D13);
  - `repo.share(n)`: o mesmo repositório, com validação pelo conjunto de leitura a cada escrita (D13);
  - `orcamento.split(6 USD, 4 USD)` *(sintaxe provisória)*.

**Como está implementado (M6, sandbox):**

```
tool edit_file(box: edits Sandbox, path: Text, old: Text, new: Text) -> Text:
    effect sandbox

graph solve(issue: Text, repo: Sandbox) -> Text:
    fixed = edit_file(edits repo, "calc.py", "a", "b")
    tests = run_tests(reads repo)          # depois de `fixed`, sem `after`
```

- **Uma sandbox é um diretório.** Na linha de comando, `--repo caminho`. A execução trabalha numa **cópia**, em `.calyx/runs/<id>/sandboxes/repo/`; o original não muda. No fim, o `calyx` diz onde a cópia está.
- **Tools pedem a sandbox emprestada** num parâmetro `reads Sandbox` ou `edits Sandbox` (`E0646` sem isso), e recebem o caminho da cópia. Quem chama empresta do mesmo jeito: `reads repo` ou `edits repo` (`E0642` sem empréstimo, `E0643` com o modo errado). Uma tool que edita tem `effect sandbox` (`E0648`).
- **A ordem sai dos empréstimos.** Um passo que edita uma sandbox vem depois de todo passo anterior (na ordem do texto) que a usa; um passo que lê vem depois de todo passo anterior que a edita. Leituras entre si rodam em paralelo. Um subgrafo que recebe a sandbox conta como edição.
- **O compilador recusa** itens de um `for each` editando a mesma sandbox (`E0644`); duas partes de um passo que rodariam ao mesmo tempo, uma editando (`E0645`); e a sandbox como valor: guardada num passo, devolvida, mostrada a um prompt (`E0647`).
- **No runtime:** edições de uma sandbox rodam uma por vez, leituras ao mesmo tempo (também as tools de uma volta de um agente). Antes de cada chamada que edita, um snapshot; se a chamada falha, a sandbox volta a ele, e chamadas `effect sandbox` são repetidas em erros temporários a partir do mesmo estado.
- **Snapshots por conteúdo**, em `<sandbox>.snapshots/`: cada arquivo é guardado uma vez pelo SHA-256; um snapshot é um manifesto (caminho → hash). O hash do snapshot depois de cada edição vai para o diário com a resposta da tool. **Na retomada, a sandbox volta ao último snapshot do diário**: o que uma chamada interrompida fez é desfeito, e ela roda de novo.
- **Limites:** a sandbox não é isolamento do sistema operacional; uma tool que escreve fora do caminho que recebe não é impedida. Links simbólicos não são copiados. `fork`/`share` ainda não existem.

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
- **`write once`:** uma entrada `begin` é gravada (com `fsync`) antes da chamada. Se a execução cai entre o `begin` e o fim da chamada, o resultado é desconhecido: a retomada **não repete** a chamada por conta própria, e aplica a política `on_uncertain` da tool (ou a decisão `--uncertain` dada na retomada). Uma chamada tomada como feita entra no diário, então o replay não precisa de decisão.
- Uma linha cortada no meio por uma queda é descartada na retomada.
- **Esperas (`receive`):** o diretório da execução guarda `waits.jsonl` (o prazo de cada espera) e `inbox.jsonl` (as mensagens entregues). Uma execução que espera termina com o estado `waiting`.
- **Sandboxes:** a resposta de uma chamada que edita uma sandbox leva o hash do snapshot depois dela; a retomada põe cada sandbox de volta ao último snapshot do diário (seção 7.3).

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
| Inferência e restrição de efeitos; política de `write once` presente e coerente (`verify` com tool de leitura, `Unit` quando segue sem resposta) | D2 |
| Escrita sem chave de idempotência (aviso); agente sem tools `write once` | D2 |
| Precondições: tool com `checks`, campos e tipos do estado, só operadores | D29 |
| Empréstimo de sandboxes: modo certo, sem edições em paralelo, sandbox nunca como valor | D13, D26 |
| Entidades: handlers puros, `ask`/`send` para o tipo certo de handler, atualização perdida (aviso) | D15, D21, D33 |
| Uso de recursos afins e suas visões | D26 |
| Escritas externas sem ordem definida (aviso) | D2 |
| Terminação: limite em laços e rodadas, `decreases` em recursão | D5, D17 |
| Teto de custo vs. orçamento (laços multiplicam pelo limite) | D3 |
| Orçamento de contexto por caminho, com invariante quando há compactação | D3, D5 |
| Redutor presente para estado escrito em paralelo | D1 |
| Corridas: dois ou mais ramos do mesmo tipo, condição pura, `on none` presente; escrita num ramo (aviso) | D12 |
| Roteadores: dois ou mais modelos, política conhecida, verificação pura que recebe o tipo da resposta | D30 |
| Ciclos de `ask` | D33 |
| Regras de `respond` / `return` | D19 |
| Com `calyx check --tools`: a declaração de cada tool contra as anotações do servidor MCP dela (seção 11.2) | D2, D34 |

Mensagens de erro estruturadas, com **esperado**, **observado** e **local**, para que um agente de IA consiga corrigir sozinho.

---

## 11. Ferramentas

| Comando | Função |
|---|---|
| `calyx check` | Verifica o programa (meta: até 1 s), sem gerar código. Com `--tools`, sobe o servidor MCP de cada tool (do `calyx.toml`) e confere a declaração contra o que ele diz (seção 11.2) |
| `calyx build` | Gera um executável autocontido para um grafo: `calyx build arquivo.clyx [-o nome] [--graph g]`. É uma cópia do próprio `calyx` com o programa e o `calyx.toml` dentro (D35); não precisa de compilador C, nem de Calyx onde roda. Os parâmetros do grafo viram opções (`./nome --param valor`), e `./nome resume <id>`, `replay` e `runs` funcionam como no `calyx` |
| `calyx run` | Executa um grafo: `calyx run arquivo.clyx --param valor` (listas e registros em JSON, ou `@arquivo.json`). Chamadas independentes rodam em paralelo; `--deterministic` roda uma por vez; `--budget` troca o orçamento |
| `calyx fmt` | Formata o código |
| `calyx resume` | Continua uma execução interrompida ou que falhou: `calyx resume <id>`. Chamadas já no diário não são feitas (nem pagas) de novo. `--budget` aumenta um orçamento esgotado; `--uncertain done\|done=<resposta>\|retry\|failed` diz o que aconteceu com chamadas `write once` de resultado incerto |
| `calyx replay` | Reexecuta a partir de um diário, sem chamar modelos nem tools: `calyx replay <id>` |
| `calyx runs` | Lista as execuções, com estado (`finished`, `failed`, `interrupted`, `waiting`), chamadas e retomadas |
| `calyx deliver` | Entrega uma mensagem a uma execução que espera num `receive`: `calyx deliver <id> Approval Approved` |
| `calyx tick` | Retoma as execuções que receberam mensagem ou cujo prazo venceu; para rodar num agendador (cron) |
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
- **Novas tentativas:** erros temporários (`Timeout`, `RateLimit`, `Unavailable`, `Network`) de modelos são repetidos até 4 tentativas, esperando 1 s, 2 s e 4 s (o dobro para `RateLimit`), ou mais, se o provedor pedir (cabeçalho `Retry-After` ou "retry in N s" na mensagem, até 60 s). Tools repetem os erros listados em `retry_on`; tools `write` com `idempotency_key` repetem também os temporários; `write once` nunca repete sozinha: depois de `Timeout`, `Unavailable` ou `Network` aplica `on_uncertain` (D2).
- **Sandboxes:** o parâmetro emprestado recebe o caminho da cópia. O pedido ao I/O leva `"borrows": [{"param", "mode", "path"}]`; o I/O segura a trava da sandbox e tira os snapshots.
- **Contrato com a tool (MCP):** a chave de idempotência e as precondições vão no `_meta` da chamada `tools/call`, como `calyx/idempotency_key` (texto) e `calyx/requires` (lista de árvores: `{"state": campo}`, `{"value": v}`, `{"op", "l", "r"}` ou `{"op", "v"}`). Uma tool cujas precondições não valem responde com erro (`isError`) e texto começando com `PreconditionFailed:`, sem ter feito nada. Um servidor na frente de outro serviço repassa os erros temporários dele com o texto do erro começando com `Timeout:`, `Unavailable:`, `RateLimit:` ou `Network:`: o runtime trata como se o próprio servidor tivesse demorado ou caído (repete leituras e escritas com chave; numa `write once`, aplica o `on_uncertain`, porque a chamada pode ter acontecido). `examples/tools/fake_store.py` implementa o contrato.
- **A declaração contra as anotações do servidor.** Servidores MCP podem descrever cada tool com `annotations` (`readOnlyHint`, `idempotentHint`, `destructiveHint`). São dicas, e não dizem nada sobre chaves nem sobre o que fazer com um resultado incerto, então não substituem a declaração; mas podem contradizê-la. Na primeira chamada de cada tool, o runtime compara, e `calyx check --tools` faz o mesmo sem rodar:
  - `W0701`: tool declarada `read` que o servidor não diz ser somente leitura. Leituras são repetidas e reexecutadas à vontade.
  - `W0702`: `write` sem chave que o servidor não diz ser idempotente. O runtime a repete depois de falhas.
  - `E0701`–`E0703` (só no `check --tools`): tool sem servidor no `calyx.toml`, servidor que não sobe, servidor sem a tool.
  Só são julgadas as tools cujo servidor manda anotações: sem elas, os padrões do MCP (não é leitura, não é idempotente) marcariam toda leitura de um servidor que simplesmente não diz nada. As anotações não dizem se o serviço respeita a chave de idempotência: uma `write` com chave num serviço que a ignora passa sem aviso.
- **Saída de tools:** cortada em `max_output` (D16).
- **Falha:** se um passo falha depois das tentativas, a execução para com o grafo, o passo e o motivo, a menos que um `try` a capture (D11).

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
