# Calyx

*Em português: [README.pt.md](README.pt.md).*

<p align="center"><picture><source media="(prefers-color-scheme: dark)" srcset="media/check_dark.gif"><img src="media/check.gif" width="640" alt="calyx check refuses a refund workflow that could pay twice; three lines fix it"></picture></p>

AI agents now send e-mails, move money and edit code. When one of those calls
times out, nobody knows whether it happened. Retry, and the customer is
refunded twice. Don't, and the refund is lost. Every framework leaves that
choice to you, and most code never makes it.

Calyx is a language for agent workflows in which **every tool declares its
effect**, and the compiler **refuses the program** until it says what happens
when a call's outcome is uncertain. You write the steps; Calyx derives the
parallelism, the retries and the recovery from crashes.

That's Calyx: Python-like syntax, a graph underneath, effects you can trust.

## Calyx BLOCKS duplicate effects, before running

A refund, then an e-mail. Looks fine:

```python
tool refund(order: Text, amount: Float) -> Unit:
    effect write

tool email(to: Text, body: Text) -> Unit:
    effect write once

graph handle_refund(order: Text, message: Text) -> Text:
    proposal = gemini(decide(order, message))
    paid = refund(order, proposal.amount)
    notice = email("ana@example.com", "refunded {proposal.amount}")
    return "done"
```

`calyx check` disagrees:

```
warning[W0601]: `write` tool without `idempotency_key`
- expected : `idempotency_key param`, so retries and resumed runs cannot apply it twice
- observed : no key: the runtime repeats the call after failures, so the tool itself must be idempotent

error[E0304]: `write once` tool needs a policy for uncertain outcomes
- expected : `on_uncertain verify(...)`, `on_uncertain pause` or `on_uncertain accept_loss`
- observed : no `on_uncertain` property

warning[W0602]: external writes without a defined order
- expected : `notice after paid` (or `paid after notice`), or a value one passes to the other
- observed : `paid` and `notice` both write outside the run and may run at the same time
```

Three lines fix it, and they are not optional:

```python
tool refund(request: Text, order: Text, amount: Float) -> Unit:
    effect write
    idempotency_key request               # retried or resumed: paid once

tool email(to: Text, body: Text) -> Unit:
    effect write once
    on_uncertain verify(email_sent(to, body))   # timed out? look before resending

graph handle_refund(request: Text, order: Text, message: Text) -> Text:
    proposal = gemini(decide(order, message))
    paid = refund(request, order, proposal.amount)
    notice = email("ana@example.com", "refunded {proposal.amount}")
    notice after paid
    return "done"
```

Errors are structured (`expected` / `observed` / location), so an AI agent can
read them and fix its own code. In short: the compiler demands the effect
contract that, in Python, only a careful programmer remembers to write.

## Calyx RESUMES without paying twice

Every finished call goes to a journal. Kill the process anywhere, and
`calyx resume <id>` continues where it stopped: no model call paid twice, no
refund sent twice. We killed the refund workflow (`kill -9`) at 6 points:

| System | Correct (of 6) | Duplicate refunds | Duplicate e-mails |
|---|---|---|---|
| **Calyx** | **6** | **0** | **0** |
| Temporal | 4 | 1 | 1 |
| LangGraph, `durability="sync"` | 4 | 1 | 1 |
| LangGraph, default | 3 | 2 | 1 |

With the care their docs recommend (3 extra lines each), Temporal and LangGraph
also get 6 of 6. The difference is the default, not the ceiling: in Calyx,
those lines are required. Details in
[`docs/evaluation/comparacao.md`](docs/evaluation/comparacao.md).

**On another machine.** With the journal in PostgreSQL
(`CALYX_DATABASE_URL`), machines share runs: if one dies mid-run, a
`calyx worker` on another takes the run over and finishes it from the
journal, and a run is never run by two machines at once.

**Against the real Stripe API** (test mode, [`bench/stripe/`](bench/stripe)):
a refund and a store credit, the process killed at 5 points. Stripe itself
counts one refund and one credit in all 5 cases. The same program without
the contracts: 3 of 5, one duplicate refund and one duplicate credit.

## Calyx is PARALLEL

No `async`, no `parallel`, no threads. Steps that don't depend on each other
run at the same time, critical path first:

```python
graph research(topic: Text) -> Text:
    effect read                         # this graph can never write to the world
    limits threads 8, budget 2 USD

    plan = gemini(split_topic(topic))
    answers = for each q in plan.questions:     # no `parallel`, no `async`
        gemini(summarize(q, web_search(q)))
    return gemini(write_report(topic, answers))
```

With a real model (Gemini), median of 3 runs:

| Questions | Calyx | asyncio, by hand | LangGraph | Python, sequential |
|---|---|---|---|---|
| 5 | **5.1 s** | 5.2 s | 7.3 s | 10.6 s |
| 10 | **6.1 s** | 6.4 s | 8.1 s | 17.3 s |

Calyx ties hand-written asyncio. The gain is not writing the parallelism.

## Calyx SERVES your workflows over MCP

`calyx serve` turns a program into an MCP server: each graph is a tool any
MCP client can call (Claude Desktop, an IDE, another agent), with the
journal, parallelism and effect contracts of `calyx run`. Its annotations
come from the program: a graph that only reads is `readOnlyHint`; one that
writes is `idempotencyKeyHint`, and keeps the promise by construction. A
call with an idempotency key is tied to one run: the same key returns that
run's answer, or resumes it if it died, and never runs it twice.

```json
{ "mcpServers": { "refunds": {
    "command": "calyx", "args": ["serve", "/path/to/refund.clyx"] } } }
```

## Calyx checks FAST

**Target:** check any program in under 1 second, so an agent can check after
every edit. **Status:** the 155-line customer-service example
([`atendimento.clyx`](examples/atendimento.clyx)) checks in **0.5 ms**.
The checker and runtime compile to native code; `calyx` is one ~3 MB binary.

## Calyx is PROVEN, within limits

The effect rules come with three theorems: a `write once` call happens **at most
once**; finished by `verify` or a person, **exactly once**; and **nothing done is
redone** on resume. For one call, they are proved in Lean
([`formal/Effects.lean`](formal/Effects.lean), checked in CI). For whole
programs, a bounded model checker tries every combination of up to 2 faults
and 2 crashes, and finds a counterexample for each hypothesis dropped.
Formalizing found two bugs in the runtime, both fixed.

On [LIMBO](https://github.com/jaxblack/limbo-bench), an external benchmark of
duplicated side effects (205 faulted episodes), Calyx programs reach **76%**
exactly-once success with the tools as they are (frontier models: 74–79%),
and **100%, zero duplicates** when every write accepts a key.

# Get Started

### 1. Install:

```bash
curl -fsSL https://raw.githubusercontent.com/daltonfontes/calyx/main/install.sh | sh
```

One binary, no dependencies. Linux and macOS (x86_64 and ARM); on Windows, use WSL.

### 2. Tell your agent to use Calyx:

Add this to your `AGENTS.md`:

```
When writing agent workflows:
- write them in Calyx (.clyx); the spec is docs/spec/calyx.md
- declare every tool's effect: `read`, `write` with `idempotency_key`,
  or `write once` with `on_uncertain`
- run `calyx check <file>` after every edit, and fix every error
- try it with `calyx run <file> --fake-models` before using real models
```

### 3. Run it (from a clone of this repo, for the examples):

```bash
calyx run examples/refund.clyx --fake-models --request R1 --order A100 --message "arrived broken"
export GEMINI_API_KEY=...           # or any OpenAI-compatible provider
calyx run examples/research.clyx --topic "solar energy in Brazil"
calyx runs                          # list runs; `calyx resume <id>` continues one
calyx build examples/research.clyx  # a standalone executable, nothing to install
```

Tools are [MCP](https://modelcontextprotocol.io) servers, written in any
language and declared in `calyx.toml`. `calyx check --tools` compares each
declaration with what the server says about itself.

# Examples

### Syntax == Python, steps == a graph

Each `name = ...` is a step. Values don't change, and the order comes from the
data, not from the lines. See `research` above.

### Waiting for a person == `receive`

The run stops, writes its state to disk and exits; no server. Days later,
`calyx deliver <id> Approval Approved` and `calyx resume <id>` continue it.

```python
message Approval = Approved | Denied(reason: Text)

graph approve(request: Text) -> Text:
    proposal = gemini(propose(request))
    approval = receive Approval about proposal, timeout 3 days:
        on timeout: Denied(reason="nobody answered in 3 days")
    return match approval:
        case Approved: "approved: {proposal}"
        case Denied(reason): "denied ({reason}): {proposal}"
```

### Agents == bounded loops

An agent is a step like any other, and every loop has a limit the compiler
checks.

```python
graph ask(question: Text) -> Text:
    draft = agent gemini:                       # the ReAct loop, bounded
        tools [web_search]
        max_turns 6
        task investigate(question)
        on turn_limit: final_answer
        on stuck: final_answer

    return loop answer = draft, max 2:          # every loop has a limit
        match gemini(review(question, answer)):
            case Approved:
                done answer
            case Rejected(feedback):
                next gemini(improve(answer, feedback))
        on limit: last
```

### Races undo the loser == `compensate` (a saga)

Two strategies race; the loser may already have paid. A tool that declares
how it is undone gets undone, once, before the race finishes, also after a
crash ([`saga.clyx`](examples/saga.clyx)):

```python
tool charge(request: Text, amount: Float) -> Unit:
    effect write
    idempotency_key request
    compensate uncharge(request)        # a keyed write that undoes it

graph main(request: Text) -> Text:
    how = race first:
        card: by_card(request)          # charges, then a slow check
        wallet: by_wallet(request)      # faster: wins
        on none: fail "no way to pay"
    return "paid by {how}"              # the card's charge was undone
```

The full programs behind these snippets are in
[`examples/readme/`](examples/readme), checked in CI. More, from sandboxes
for code agents to per-user memory, debates and model routers, in
[`examples/`](examples).

# References

- Paper: [Calyx: A Compiler That Demands the Effect Contract in Agent Workflows](paper/Calyx.pdf) (in Portuguese: [Calyx-pt.pdf](paper/Calyx-pt.pdf)).
- Formalization: [Effects.lean](formal/Effects.lean), the effect rules and their proofs, in Lean; [formal.md](docs/paper/formal.md) and the bounded checker [model.py](bench/formal/model.py).
- Spec: [calyx.md](docs/spec/calyx.md), the language, every check and every error code.
- Evaluation: [docs/evaluation/](docs/evaluation), vs. Python, LangGraph and Temporal, real bugs, and LIMBO.
- Benches: [bench/](bench), every script behind the numbers above, with the data in `bench/results/`.
- Demo: [make_check_gif.py](media/make_check_gif.py) records the GIF above from the real `calyx check` output.
- MCP proposal: [idempotency-key-hint.md](docs/mcp/idempotency-key-hint.md), idempotency keys on `tools/call`, declared per tool, with a prototype and its measurement.
- Design: [docs/discovery/](docs/discovery), the 35 design decisions and the research behind them.
- Em português: [README.pt.md](README.pt.md), and a project overview with every milestone and folder in [visao-geral.md](docs/visao-geral.md).

# Limitations

```
- Calyx is new (v0.3). Expect bugs and breaking changes.
- The spec, docs and examples are mostly in Portuguese; the paper is in English.
- The guarantees rest on stated hypotheses: the service honours the idempotency
  key, and `verify` reads fresh state. Calyx cannot check a service that ignores
  keys at run time; `calyx check --tools` catches wrong effects, ignored keys
  from servers that send `idempotencyKeyHint` (an annotation we propose to
  MCP, docs/mcp/idempotency-key-hint.md), and with `--probe` tests the key
  for real in a service's test environment.
- `on_uncertain accept_loss` can lose the effect. That is what it means.
- The Lean proofs cover one call; whole programs get a bounded check. Both
  models are written by hand, not extracted from the C runtime.
- Agents cannot call `write once` tools.
- Several machines share runs only through the journal in PostgreSQL;
  programs with entities, `receive` or sandboxes still run on one machine.
  The database connection has no TLS yet.
- Tools only as MCP servers over stdio; models only via OpenAI-compatible APIs.
- The pure layer has no recursion, by design (every program must terminate).
- No language server, debugger or REPL.
- Most experiments use fake models; the parallelism and recovery ones were
  repeated with Gemini. The baselines and bug corpus were written by Calyx's
  author; LIMBO's tasks and grader were not.
- No Windows (WSL works).
```

**CALYX IS YOUNG. EXPECT BUGS AND [REPORT THEM](https://github.com/daltonfontes/calyx/issues).**

# Credits

Calyx is created by [Dalton Fontes](https://github.com/daltonfontes), who
conceived and directed it, read the related work and reviewed every change.
The code, the experiments and the paper were written with Claude (Anthropic)
as a coding assistant.

# License

[MIT](LICENSE).
