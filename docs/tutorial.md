# Tutorial: a support agent, from nothing to running

*Em português: [tutorial.pt.md](tutorial.pt.md).*

In four steps you build a store's support agent: it looks up orders,
answers customers, refunds broken orders without ever paying twice, and
reports on many orders at once. Every program is in
[`examples/tutorial/`](../examples/tutorial), and CI checks and runs each
one, so what you read here works.

You need `calyx` ([install](../README.md#get-started)) and `python3` (the
tools of the tutorial are a fake store written in Python). No API key: every
step runs with `--fake-models`, which answers with made-up values of the
right type. Clone the repository and go to the folder:

```bash
git clone https://github.com/daltonfontes/calyx && cd calyx/examples/tutorial
```

If you use VS Code, install the extension in
[`editors/vscode`](../editors/vscode): the errors below show up as you type.

## 1. A tool, a type, a prompt, a graph

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

- A **tool** is an MCP server, a program in any language. `calyx.toml` says
  which command starts it. Its **effect** is part of its type: `read`
  changes nothing, so Calyx may retry it after a timeout.
- A **prompt** is a typed template. The answer is checked against `Text`,
  or against any type you give it.
- A **graph** is the workflow. You write steps; Calyx runs the independent
  ones at the same time.

```bash
calyx check 1_lookup.clyx
calyx run 1_lookup.clyx --fake-models --order A100 --question "Where is my order?"
```

`calyx check` is the compiler. Remove the `effect read` line and run it
again: Calyx refuses a tool that does not say what it does to the world
(`E0302`).

## 2. An agent

[`2_agent.clyx`](../examples/tutorial/2_agent.clyx) gives the model the tool
and lets it decide what to look up:

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

The model chooses; the program sets the bounds:

- `max_turns 8` and `limits budget 1 USD`: the agent cannot run forever, or
  spend more than you allow. Calyx enforces both, not the prompt.
- `on turn_limit` and `on stuck` are required. If the agent repeats the
  same call, or runs out of turns, you said what happens.
- `skills`: the model sees only each skill's description, and loads the
  text when it needs it, so its context carries only what it uses.
- `compact with summarize`: past 20 000 tokens, the conversation is summed
  up by your prompt `summarize`, and the agent goes on from the summary.

```bash
calyx run 2_agent.clyx --fake-models --message "My order A100 arrived broken"
```

The trace (on stderr) shows each turn and each tool call. The fake model
always calls the first tool and then answers; with a real model (step 5),
the trace also shows `skill refunds` when the agent loads it.

## 3. Money, paid once

An agent that pays is where most frameworks leave the hard part to you: a
refund call times out, and nobody knows whether it happened. In
[`3_refund.clyx`](../examples/tutorial/3_refund.clyx) the model only
proposes, and the program pays:

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

- `effect write` with `idempotency_key request`: the store sees the same
  key every time the call is made, retried or resumed, and pays once.
  Remove that line, and `calyx check` warns you (`W0601`).
- `requires`: the store checks these on the order as it is **when it
  pays**, not as it was when the model read it.
- `email` is `write once`: never repeated on its own. If its answer is lost,
  Calyx first asks `email_sent`. Remove `on_uncertain`, and `calyx check`
  refuses the program (`E0304`).
- `notice after paid`: the e-mail goes out only after the refund. Without
  it, `calyx check` warns that the two writes have no order (`W0602`).

Now crash it. `CALYX_CRASH_AFTER=3` kills the process (as a `kill -9`
would) right after the third call reaches the journal, that is, right after
the refund:

```bash
CALYX_CRASH_AFTER=3 calyx run 3_refund.clyx --fake-models --request R2 --order A100 --message "broken"
calyx runs                         # the run is there, unfinished
calyx resume <id> --fake-models    # "3 taken from the journal"
```

The resumed run takes the order, the proposal and the refund from the
journal, and makes only the two calls left: the e-mail's text and the
e-mail. The store has one payment for `R2`:

```bash
python3 -c "import json; print(json.load(open('.calyx/fake_store.json'))['payments'])"
```

Run the same command again with `--request R2`: the same key, so the store
pays nothing new.

## 4. Many at once

[`4_many.clyx`](../examples/tutorial/4_many.clyx) writes a report on many
orders:

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

- `for each` runs the items at the same time and the steps of each item in
  order. The list comes back in the order of `orders`, whatever finished
  first.
- `def` is the pure layer: no effects, no recursion, always ends. Maps,
  list comprehensions and `len` live there.

```bash
calyx run 4_many.clyx --fake-models --orders '["A100", "A200", "A300"]'
```

## 5. With a real model

Set a key and drop `--fake-models`:

```bash
export GEMINI_API_KEY=...
calyx run 2_agent.clyx --message "My order A100 arrived broken"
```

Models named `gemini-*`, `gpt-*` and `nvidia/*` are known. Any provider
with an OpenAI-compatible API (OpenRouter, Groq, Ollama...) goes in
`calyx.toml`, and so do prices, for `limits budget` to count:

```toml
[providers.local]
url = "http://localhost:11434/v1"
key_env = "OLLAMA_KEY"
models = ["llama"]

[prices."gemini-3.5-flash-lite"]   # USD per million tokens
input = 0.10
output = 0.40
```

## 6. Where it goes next

- **As a tool for other agents:** `calyx serve 2_agent.clyx` serves each
  graph as an MCP tool, for Claude Desktop, an IDE or another agent.
  `--http` serves it over HTTPS to callers with tokens.
- **A person in the loop:** `receive` waits, for days if need be, for an
  approval, and `calyx deliver` gives it
  ([`examples/approval.clyx`](../examples/approval.clyx)).
- **In production:** the journal in PostgreSQL, `calyx worker` to finish
  what a crash left, telemetry, upgrades that keep runs: the
  [operations guide](operacao.md) (in Portuguese).
- **Everything else:** the [spec](spec/calyx.md), with every check and
  every error code.
