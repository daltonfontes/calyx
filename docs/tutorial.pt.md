# Tutorial: um agente de atendimento, do zero até rodar

*In English: [tutorial.md](tutorial.md).*

Em quatro passos você monta o agente de atendimento de uma loja. Ele
consulta pedidos, responde clientes, reembolsa pedidos quebrados sem nunca
pagar duas vezes e faz um relatório de vários pedidos de uma vez. Todos os
programas estão em [`examples/tutorial/`](../examples/tutorial), e o CI
confere e roda cada um, então o que está aqui funciona.

Você precisa da `calyx` ([instalação](../README.pt.md#como-começar)) e do
`python3` (as tools do tutorial são uma loja falsa escrita em Python). Sem
chave de API: todo passo roda com `--fake-models`, que responde com valores
inventados do tipo certo. Clone o repositório e entre na pasta:

```bash
git clone https://github.com/daltonfontes/calyx && cd calyx/examples/tutorial
```

Se você usa o VS Code, instale a extensão
([calyx-vscode.vsix](https://github.com/daltonfontes/calyx/releases/latest/download/calyx-vscode.vsix),
com "Install from VSIX..."): os erros abaixo aparecem enquanto você escreve. Os programas estão em inglês, como os nomes da linguagem.

## 1. Uma tool, um tipo, um prompt, um grafo

[`1_lookup.clyx`](../examples/tutorial/1_lookup.clyx):

```python
type Order:
    id: Text
    customer: Text
    total: Float
    refunded: Float
    status: Status

tool get_order(id: Text) -> Order:
    effect read
    max_output 500 tokens

prompt answer(order: Order, question: Text) -> Text:
    """
    You answer a store's customers. Their order: {order}
    Their question: {question}
    Answer in two sentences.
    """

graph help(order: Text, question: Text) -> Text:
    found = get_order(order)
    return gemini(answer(found, question))
```

- Uma **tool** é um servidor MCP, um programa em qualquer linguagem; o
  `calyx.toml` diz que comando o inicia. O **efeito** dela faz parte do
  tipo: `read` não muda nada, então a Calyx pode repetir a chamada depois
  de um timeout.
- Um **prompt** é um modelo de texto tipado. A resposta é conferida contra
  `Text`, ou contra qualquer tipo que você der.
- Um **grafo** é o workflow. Você escreve os passos; a Calyx roda ao mesmo
  tempo os que são independentes.

```bash
calyx check 1_lookup.clyx
calyx run 1_lookup.clyx --fake-models --order A100 --question "Where is my order?"
```

O `calyx check` é o compilador. Apague a linha `effect read` e rode de
novo: a Calyx recusa uma tool que não diz o que faz com o mundo (`E0302`).

## 2. Um agente

[`2_agent.clyx`](../examples/tutorial/2_agent.clyx) dá a tool ao modelo e
deixa que ele decida o que consultar:

```python
skill refunds:
    description "How to handle a refund request: what to check, and the limits."
    """
    1. Look up the order. Refund only delivered orders.
    2. Never more than the total minus what was already refunded.
    3. Up to 100 without approval; above that, say a person will decide.
    """

graph help(message: Text) -> Text:
    limits budget 1 USD
    answer = agent gemini:
        tools [get_order]
        skills [refunds, delays]
        max_turns 8
        task support(message)
        compact with summarize at 20000 tokens
        on turn_limit: final_answer
        on stuck: final_answer
    return answer
```

O modelo escolhe; o programa põe os limites:

- `max_turns 8` e `limits budget 1 USD`: o agente não roda para sempre nem
  gasta mais do que você permite. Quem garante é a Calyx, não o prompt.
- `on turn_limit` e `on stuck` são obrigatórios: se o agente repetir a
  mesma chamada, ou acabar as voltas, você disse o que acontece.
- `skills`: o modelo vê só a descrição de cada skill e carrega o texto
  quando precisa, então o contexto dele leva só o que ele usa.
- `compact with summarize`: passando de 20 000 tokens, a conversa é
  resumida pelo seu prompt `summarize`, e o agente segue do resumo.

```bash
calyx run 2_agent.clyx --fake-models --message "My order A100 arrived broken"
```

O trace (no stderr) mostra cada volta e cada chamada de tool. O modelo
falso sempre chama a primeira tool e depois responde; com um modelo de
verdade (passo 5), o trace também mostra `skill refunds` quando o agente a
carrega.

## 3. Dinheiro, pago uma vez

Um agente que paga é onde a maioria dos frameworks deixa a parte difícil
para você: a chamada de reembolso dá timeout, e ninguém sabe se ela
aconteceu. Em [`3_refund.clyx`](../examples/tutorial/3_refund.clyx), o
modelo só propõe, e o programa paga:

```python
tool refund(request: Text, order: Text, amount: Float) -> Unit:
    effect write
    idempotency_key request
    checks OrderState

tool email(to: Text, subject: Text, body: Text) -> Unit:
    effect write once
    timeout 10 s
    on_uncertain verify(email_sent(to, subject))

graph refund_request(request: Text, order: Text, message: Text) -> Text:
    found = get_order(order)
    proposal = gemini(decide(found, message))

    paid = refund(request, order, proposal.amount):
        requires state.status == Delivered
        requires state.refunded + proposal.amount <= state.total

    body = gemini(reply(found, proposal.amount))
    notice = email(found.email, "Refund of order {order}", body)
    notice after paid

    return "refunded {proposal.amount} to {found.email}"
```

- `effect write` com `idempotency_key request`: a loja recebe a mesma chave
  toda vez que a chamada é feita, repetida ou retomada, e paga uma vez.
  Apague essa linha, e o `calyx check` avisa (`W0601`).
- `requires`: a loja confere essas condições no pedido como ele está **na
  hora de pagar**, não como estava quando o modelo o leu.
- `email` é `write once`: nunca se repete sozinho. Se a resposta se
  perder, a Calyx primeiro pergunta a `email_sent`. Apague o
  `on_uncertain`, e o `calyx check` recusa o programa (`E0304`).
- `notice after paid`: o e-mail só sai depois do reembolso. Sem isso, o
  `calyx check` avisa que as duas escritas não têm ordem (`W0602`).

Agora derrube a execução. `CALYX_CRASH_AFTER=3` mata o processo (como um
`kill -9`) logo depois que a terceira chamada chega ao diário, ou seja,
logo depois do reembolso:

```bash
CALYX_CRASH_AFTER=3 calyx run 3_refund.clyx --fake-models --request R2 --order A100 --message "broken"
calyx runs                         # a execução está lá, sem terminar
calyx resume <id> --fake-models    # "3 taken from the journal"
```

A execução retomada tira do diário o pedido, a proposta e o reembolso, e
faz só as duas chamadas que faltavam: o texto do e-mail e o e-mail. A loja
tem um pagamento para `R2`:

```bash
python3 -c "import json; print(json.load(open('.calyx/fake_store.json'))['payments'])"
```

Rode o mesmo comando de novo com `--request R2`: é a mesma chave, então a
loja não paga nada a mais.

## 4. Vários de uma vez

[`4_many.clyx`](../examples/tutorial/4_many.clyx) faz um relatório de
vários pedidos:

```python
def tally(lines: List[Line]) -> Map[Text, Nat]:
    delivered = len([l for l in lines if l.status == Delivered])
    other = len([l for l in lines if l.status != Delivered])
    return {"delivered": delivered, "other": other}

graph status_report(orders: List[Text]) -> Report:
    lines = for each id in orders:
        found = get_order(id)
        text = gemini(note(found))
        Line(order = id, status = found.status, note = text)
    return Report(lines = lines, counts = tally(lines))
```

- `for each` roda os itens ao mesmo tempo, e os passos de cada item em
  ordem. A lista volta na ordem de `orders`, seja qual for o que terminou
  primeiro.
- `def` é a camada pura: sem efeitos, sem recursão, sempre termina. Mapas,
  listas por compreensão e `len` ficam aí.

```bash
calyx run 4_many.clyx --fake-models --orders '["A100", "A200", "A300"]'
```

## 5. Com um modelo de verdade

Defina a chave e tire o `--fake-models`:

```bash
export GEMINI_API_KEY=...
calyx run 2_agent.clyx --message "My order A100 arrived broken"
```

Modelos `gemini-*`, `gpt-*` e `nvidia/*` já são conhecidos. Qualquer
provedor com API compatível com a da OpenAI (OpenRouter, Groq, Ollama...)
vai no `calyx.toml`, e os preços também, para o `limits budget` contar:

```toml
[providers.local]
url = "http://localhost:11434/v1"
key_env = "OLLAMA_KEY"
models = ["llama"]

[prices."gemini-3.5-flash-lite"]   # USD por milhão de tokens
input = 0.10
output = 0.40
```

## 6. Para onde ir depois

- **Como tool de outros agentes:** `calyx serve 2_agent.clyx` serve cada
  grafo como uma tool MCP, para o Claude Desktop, uma IDE ou outro agente.
  Com `--http`, por HTTPS, para chamadores com token.
- **Uma pessoa no meio:** `receive` espera, por dias se for preciso, uma
  aprovação, e `calyx deliver` a entrega
  ([`examples/approval.clyx`](../examples/approval.clyx)).
- **Em produção:** o diário no PostgreSQL, o `calyx worker` para terminar o
  que uma queda deixou, telemetria, atualizações que não perdem execuções:
  o [guia de operação](operacao.md).
- **Todo o resto:** a [spec](spec/calyx.md), com cada verificação e cada
  código de erro.
