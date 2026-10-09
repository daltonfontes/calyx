// Calyx: the paper. Build with `typst compile paper/calyx.typ paper/Calyx.pdf`
// (Typst 0.15). Every number comes from docs/evaluation/ and bench/results/.

#set document(
  title: "Calyx: A Compiler That Demands the Effect Contract in Agent Workflows",
  author: "Dalton Fontes",
)
#set page(
  paper: "us-letter",
  margin: (x: 1.7cm, top: 2cm, bottom: 2.2cm),
  columns: 2,
  numbering: "1",
)
#set columns(gutter: 0.8cm)
#set text(font: "Libertinus Serif", size: 9.5pt, lang: "en")
#set par(justify: true, leading: 0.52em, spacing: 0.75em)
#set heading(numbering: "1.1")
#show heading.where(level: 1): set text(size: 11pt)
#show heading.where(level: 2): set text(size: 9.5pt)
#show heading: set block(above: 1.1em, below: 0.6em)
#show raw: set text(font: "DejaVu Sans Mono", size: 7pt)
#show raw.where(block: false): set text(size: 8pt)
#set table(stroke: none, inset: (x: 3pt, y: 2.2pt))
#show table: set text(size: 8pt)
#show figure.caption: set text(size: 8pt)
#show figure: set block(breakable: false)
#set figure(gap: 0.5em)

#let rule = table.hline(stroke: 0.5pt)
#let thick = table.hline(stroke: 0.8pt)

#place(top + center, scope: "parent", float: true)[
  #align(center)[
    #text(size: 16pt, weight: "bold")[Calyx: A Compiler That Demands the \ Effect Contract in Agent Workflows]
    #v(0.6em)
    #text(size: 10.5pt)[Dalton Fontes]
    #v(0.1em)
    #text(size: 9pt)[#link("https://github.com/daltonfontes/calyx")]
  ]
  #v(0.6em)
  #block(width: 100%, inset: (x: 1.2cm))[
    #set text(size: 8pt)
    #set par(justify: true)
    *AI disclosure.* Calyx was built by the author with Claude (Anthropic): the
    author set its goals and approved each step; Claude wrote most of the
    compiler and runtime, ran the experiments and wrote this paper from the
    repository, which the author reviewed. Four related works (marked in the
    references) could only be read from their abstracts.
  ]
  #v(0.8em)
]

#heading(numbering: none, outlined: false)[Abstract]

LLM agent workflows call models and tools whose effects leave the run: they
pay, send e-mail, edit repositories and wait for people. Agent frameworks
such as LangGraph and durable execution engines such as Temporal resume an
interrupted run, but leave the contract of each effect to the programmer:
whether it may be repeated, under which key, and what to do when nobody
knows whether it happened. When that code is missing, the result is a
second payment, a repeated e-mail, a lost update.

Calyx is a workflow language in which every tool declares its effect
(`read`, `write` with an idempotency key, or `write once` with a policy for
uncertain outcomes), and the compiler derives parallelism and checks from
the dependency graph. A program that can duplicate an effect, lose an
update or leave two writes unordered is rejected or warned about before it
runs; the runtime records every call in a journal and resumes without
redoing work. We state the guarantees as three theorems under five explicit
hypotheses and check them exhaustively on a bounded model, which also
produces a counterexample for each hypothesis dropped.

Against LangGraph and Temporal on the same workflows, Calyx gets every
crash, wait and concurrency scenario right by default; the baselines get
them right only with hand-written care. On LIMBO, an external benchmark of
duplicated side effects, Calyx programs match the best frontier models
under the tools' native contracts (76% exactly-once success versus 74–79%)
and reach 100% when every write accepts a key. The guarantee lives in the
tool contract; Calyx makes the contract mandatory.

= Introduction

A support agent receives a refund request, proposes an amount, a person
approves it, the agent pays and e-mails the customer. If the process dies
after the payment is made but before the result is recorded, does the
resumed run pay again? If it dies with the e-mail in flight, does it send it
again? If the approval arrives after the deadline but before anyone resumes
the run, does it count?

In current frameworks these questions have answers, but the answer is code
the programmer writes, or forgets to write, with nothing to remind them. We
measured it (§6): with LangGraph's defaults, the crash that Calyx survives
produces two payments; with Temporal, which records every step in a durable
history, an effect in flight at the time of the crash is repeated.

The thesis of this work is that *the contract of an effect belongs in the
tool's declaration, and the compiler should demand it*. The runtime
mechanisms already exist: per-call journals (Temporal @temporal, Restate
@restate), idempotency keys @helland2012idempotence @stripe_idempotent and
verify-before-retry @mansoor2026verified. There is also independent evidence
that exactly-once behaviour depends on the tool contract rather than the
model: LIMBO @li2026limbo finds that offering an idempotency key on every
write cuts duplicates sharply, and that for failures a read-back cannot
resolve (late commits, redelivery) only the contract helps. What is missing
is something that *requires* the contract.

Our contributions:

+ A language for agent workflows (§3) whose execution graph is implicit in
  data dependencies, and in which every tool declares its effect and what to
  do with an uncertain outcome.
+ Compiler checks (§4) that use those declarations to reject programs that
  can duplicate an effect, lose an update, leave writes unordered, wait
  forever, or repeat a payment on every turn of a loop; and a formal core
  with three theorems, checked exhaustively on a bounded model.
+ A runtime (§5) with a per-call journal that resumes without redoing work
  and carries the idempotency key to the tool's server. The mechanisms are
  not new; what is new is a compiler that demands the declarations they rely
  on.
+ An evaluation (§6) against LangGraph and Temporal, with the full crash
  matrix, deadlines, shared memory, runtime cost, a real-bug study with an
  unfavourable result, and an external benchmark (LIMBO).

= The Problem

The bugs we target are not business-logic bugs but *state* bugs: what
happens to effects when a run is parallel, interrupted, retried or
concurrent. @tab-cat lists seven categories, taken from the issue study of
§6.6.

#figure(
  table(
    columns: (auto, 1fr),
    align: left,
    thick, [*Category*], [*Example*], rule,
    [Repeated effect], [A retried task pays again],
    [Concurrent write], [Two parallel branches write the same field],
    [Lost update], [Two runs read a balance, add, write back],
    [Wait], [An approval with no deadline; an answer applied twice],
    [Agent loop], [The agent repeats the same call until the limit],
    [Effect order], [The confirmation e-mail goes out before the payment],
    [Recovery], [Resuming redoes work or restores inconsistent state],
    thick,
  ),
  caption: [State bugs in agent workflows.],
) <tab-cat>

What these bugs share is that the program does not say what needs saying:
that the payment cannot be repeated without a key, that the e-mail needs a
rule for when nobody knows whether it went out, that the e-mail comes after
the payment. In Python this information has nowhere to live where a tool can
check it.

= The Language

A Calyx program declares models, tools, typed prompts and graphs. The syntax
resembles Python; the semantics is dataflow.

#figure(
  ```python
  tool refund(request: Text, order: Text,
              amount: Float) -> Unit:
      effect write
      idempotency_key request
      checks OrderState

  tool email(to: Text, subject: Text,
             body: Text) -> Unit:
      effect write once
      timeout 10 s
      on_uncertain verify(email_sent(to, subject))

  graph handle_refund(request: Text, order: Text,
                      message: Text) -> Text:
      found = get_order(order)
      proposal = gemini(decide(found, message))

      paid = refund(request, order, proposal.amount):
          requires state.status == Delivered
          requires state.refunded + proposal.amount
                   <= state.total

      body = gemini(reply(found, proposal.amount))
      notice = email(found.email, "Refund {order}", body)
      notice after paid

      return "refunded {proposal.amount}"
  ```,
  caption: [The refund in Calyx. No line says what runs in parallel or what
    to do after a crash.],
) <fig-refund>

*Implicit graph.* Each `name = value` is a step, and a step depends on the
steps it uses. The compiler orders steps by dependency and runs independent
ones in parallel (in @fig-refund, `reply` may run alongside `refund`).
`after` adds an order without data.

*Declared effects.* A tool is `read`, `write` or `write once`. A `write`
may be repeated if it has an idempotency key. A `write once` is never
repeated by itself and must say what to do when its outcome is uncertain:
`pause` (a person decides when resuming), `accept_loss`, or `verify(f(...))`,
where `f` is a `read` tool that finds the call. `f` may return `Bool`, or a
list of what the call made, found again; then the first item becomes the
call's answer (the id of the ticket created, say). A `write once` that
applies a list of items one by one declares `batch items`; its `verify`
returns the items already applied, and the runtime resends only the rest.
`requires` sends preconditions to the tool's server, which checks them in
the same step as the effect.

*Other constructs*, each with its own compiler rules: `loop` and `rounds`
with a mandatory bound; `for each` (fan-out); `race` between strategies,
cancelling the losers; `agent` (the ReAct loop @yao2023react), which only
gets tools without irreversible effects; `entity`, state shared across runs
with pure handlers; `receive`, a durable wait for an outside message with a
mandatory deadline; and a model router that tries the cheapest model first.

= What the Compiler Checks

The checks use three things: the type of each value, the effect of each
step and the dependency graph. @tab-checks lists those about state.

#figure(
  table(
    columns: (auto, 1fr),
    align: left,
    thick, [*Code*], [*Rejects or warns about*], rule,
    [`E0304`], [`write once` with no policy for uncertain outcomes],
    [`W0601`], [`write` without an idempotency key],
    [`W0602`], [Two external writes with no defined order],
    [`W0603`], [A `send` to an entity computed from an `ask` to it, when the handler stores a new value (lost update)],
    [`W0604`], [A race branch that writes outside the run],
    [`W0605`], [A `write once` in a loop whose arguments do not change between turns],
    [`E0640`], [An agent given a `write once` tool],
    [`E0671`], [`receive` without a deadline],
    [`E0637`], [A `batch` that is not a list, or not verified],
    [`W0701/2`], [A declaration the tool's MCP server contradicts (`check --tools`)],
    thick,
  ),
  caption: [State checks (partial; the specification lists all).],
) <tab-checks>

Several came from the evaluation: `W0605` from the real-bug study (§6.6),
`E0637` and the list form of `verify` from LIMBO (§6.7), and `W0701/2` from
the observation that a wrong declaration is the weakest point (§6.8).

== What the rules guarantee

We formalize a core of the language: calls to models, `read`,
`write key p`, unkeyed `write`, and `write once π`; a journal split into
what is on disk and what only the operating system holds; transport faults
(lost request, lost response); and process or machine crashes. Resuming is
running the program again with the same journal. The rules are:

- *R0.* A call whose journal entry is `done(κ, v)` returns `v` without being
  made.
- *R1.* A read or model call is made, retrying temporary errors, and
  recorded without syncing.
- *R2.* A keyed write syncs the journal, then sends with key `k = ⟦p⟧`,
  retrying with the same key, and records `done` synced.
- *R3.* A `write once` records `begin(κ)` synced, sends, and on an
  uncertain error applies its policy *D1*: accept, verify (done if found,
  resend if not), or pause for a person.
- *R4.* On resume, `begin(κ)` without `done` is uncertain and goes to D1.

The theorems hold under five hypotheses: (H1) the journal honours `fsync`;
(H2) a service declared with a key honours it; (H3) `verify` is fresh: it
sees every committed write, and no request in flight commits after it
reads; (H4) the transport delivers each request at most once; (H5) a person
resuming a pause decides according to what happened.

*T1 (at most once).* Under H1–H5, for any sequence of transport faults,
process or machine crashes and resumes, each `write once` and each keyed
write is applied at most once per journal key.

*T2 (exactly once on completion).* If the run finishes, each keyed write and
each `write once` with `verify` or `pause` was applied exactly once (at most
once with `accept_loss`).

*T3 (nothing done is redone).* A call with `done` on disk is never sent
again, and resuming uses its recorded answer. Only H1 is needed.

The proofs are paper sketches: T1 follows from the `begin` record reaching
the disk before any send, so every resend passes through D1, which resends
only when nothing was applied (H3, H5); for keyed writes, R2's sync makes
the key a function of durable state, so it is the same in every attempt,
and H2 does the rest. We also check the theorems exhaustively on a bounded
executable model of the rules (@tab-model): a seven-call program with a
model, keyed writes (one whose key comes from the model's answer),
`write once` with each policy and a read, under every combination of up to
two transport faults and two crashes, with every resume.

#figure(
  table(
    columns: (1fr, auto, auto),
    align: (left, right, right),
    thick, [*Case*], [*Runs*], [*Violating*], rule,
    [Hypotheses hold, process crashes], [53,166], [*0*],
    [Hypotheses hold, machine crashes too], [217,928], [*0*],
    [No sync before a keyed write (bug fixed)], [257,488], [18,500],
    [`begin` not on disk, machine crashes], [268,360], [49,346],
    [Service ignores keys (no H2)], [53,166], [24,524],
    [Stale read-back, late commit (no H3)], [101,662], [11,248],
    [Transport redelivers (no H4)], [95,556], [24,856],
    thick,
  ),
  caption: [Bounded exhaustive check of T1–T3 (`bench/formal/model.py`).],
) <tab-model>

The last three rows are, in the model, exactly the three causes of
duplicates LIMBO measured on Calyx (§6.7). Writing rule R3 with H1 explicit
also exposed two holes in the runtime, now fixed: a `write once` went out
even if its `begin` record failed to reach the disk, and a keyed write did
not sync the journal first, so after a machine crash a model asked again
could produce a different key (the third row).

= The Runtime

The compiler emits an intermediate representation in JSON that a runtime
written in C executes, with an I/O layer in Rust (model APIs, MCP clients,
sandboxes).

*Per-call journal.* Every call has a stable key, its place in the realized
graph (step, loop turn, `for each` item), and every answer goes to an
append-only journal. Before any external write, the journal is synced to
disk; a `write once` also records `begin` first, so finding `begin` without
an answer means the outcome is uncertain and the tool's policy decides.

*The key reaches the server.* The idempotency key and the preconditions
(`requires`) travel to the tool's server in the MCP call's metadata, so even
a repetition caused by a runtime bug does not pay twice. A server in front
of another service reports that service's temporary errors with an error
text starting `Timeout:` or `Unavailable:`, and a `write once` then applies
its policy instead of failing.

*Entities.* An entity's state lives in a file, changed by one message at a
time per key (`flock`), with the id of each applied message stored with it:
a message is never applied twice, even if the run dies between the entity
applying it and the journal recording it.

*Waits.* A `receive` records its absolute deadline once; the run exits
(state `waiting`) and a scheduler (`calyx tick`, run from cron) resumes it
when the message arrives or the deadline passes. The arrival time is
recorded, and a late answer does not count even if nobody resumed the run
yet (§6.3).

*Declarations against servers.* MCP servers may annotate tools with hints
(`readOnlyHint`, `idempotentHint`) @mcp2025spec. On each tool's first call,
and in `calyx check --tools` without running, Calyx compares them with the
declaration and warns on a contradiction (§6.8).

= Evaluation

We ask: is derived parallelism good (Q1)? Does the compiler catch state bugs
Python does not (Q2)? Is recovery correct without hand-written code (Q3)? Is
the runtime's cost negligible? The baselines are sequential Python,
hand-written asyncio, LangGraph 1.2.12 @langgraph (default `durability` and
`sync`) and Temporal (Python SDK 1.34.0, server 1.32.0) @temporal. All run
against the same fake world: models with fixed latency and the same MCP
tools (a store with payments and e-mail). "Manual care" is the code each
system's documentation recommends and a careful programmer writes:
idempotency keys, checking before resending, a deadline kept in state. Two
experiments were repeated with a real model (Gemini). Code, data and the
commands to rerun everything are in the repository (`bench/`).

== Parallelism and cost (W1, E2)

A fan-out of N questions (search + summary each) and a report, at most 8
calls at once. With every model call taking 1 s, Calyx stays within 30–50 ms
of the theoretical bound (⌈N/8⌉+1 s), hand-written asyncio within 80–95 ms
and LangGraph about 0.8 s behind (mostly start-up). With the real model
(@tab-w1) the picture holds: Calyx ties asyncio. Without latency, Calyx's
cost is linear up to 10#super[5] items, 0.12–0.16 ms per item, with the
journal costing up to 14%; asyncio is about 5× cheaper per item (Calyx makes
a real MCP call per item) and LangGraph reaches 7 ms per item and grows.

#figure(
  table(
    columns: (1fr, auto, auto),
    align: (left, right, right),
    thick, [*System*], [*N = 5*], [*N = 10*], rule,
    [*Calyx*], [*5.1 s*], [*6.1 s*],
    [Python asyncio (by hand)], [5.2 s], [6.4 s],
    [LangGraph], [7.3 s], [8.1 s],
    [Python, sequential], [10.6 s], [17.3 s],
    thick,
  ),
  caption: [W1 with `gemini-3.5-flash-lite`, median of 3 runs. N is small
    because of the test key's rate limit.],
) <tab-w1>

Measuring cost also found a bug in Calyx: an agent's step was re-evaluated
from its first turn on every answer, a cost cubic in the number of turns.
Keeping the agent's progress between evaluations fixed it (400 turns: from
70 s to 1.5 s); what remains is quadratic and comes from the chat protocol,
which sends the whole conversation every turn.

== Recovery with external effects (W2)

The refund of @fig-refund, with the process killed at six points: after each
recorded step, and with each effect in flight (the store paid or sent, the
answer had not arrived). Correct is one payment, one e-mail and no model
call redone.

#figure(
  table(
    columns: (1fr, auto, auto),
    align: (left, center, center),
    thick, [*System*], [*Correct*], [*With care*], rule,
    [*Calyx*], [*6/6*], [—],
    [Temporal], [4/6], [6/6],
    [LangGraph `durability="sync"`], [4/6], [6/6],
    [LangGraph default], [3/6 (2 model calls redone)], [6/6],
    [Python, no checkpoint], [2/6], [6/6],
    thick,
  ),
  caption: [W2, recovery after `kill -9`. Identical, case by case, with the
    real model.],
) <tab-w2>

Between steps, Temporal and LangGraph `sync` are right, like Calyx. With an
effect in flight, no per-step record helps, not even Temporal's history: the
effect happened and the answer was lost, and the activity is retried after
its timeout, as it should be. Only the effect's contract avoids the
duplicate (a key the provider honours, or checking before resending), and
in the baselines it is optional. With the real model the counts are the
same; Calyx resumes in at most 2.7 s, Temporal in 11–16 s with default
timeouts.

== Waiting for a person with a deadline (W3)

The refund with human approval and a deadline, the process stopped during
the wait, in six scenarios.

#figure(
  table(
    columns: (1fr, auto, auto, auto),
    align: (left, center, center, center),
    thick, [*Scenario*], [*Calyx*], [*LangGraph*], [*Temporal*], rule,
    [Approved], [✓], [✓], [✓],
    [Deadline passes, all stopped], [✓], [waits forever], [✓],
    [Answer sent twice], [✓], [✓], [✓],
    [Late answer, after refusal], [✓], [pays], [✓],
    [Late answer, before resume], [✓], [pays], [pays],
    [On-time answer, resumed later], [✓], [✓], [✓],
    thick,
  ),
  caption: [W3. With manual care, LangGraph and Temporal get 6/6.],
) <tab-w3>

LangGraph has no deadline for an `interrupt()`. Temporal has one, but with
no worker running at the deadline, the timer and the late signal reach the
next worker together and the SDK delivers the signal first. *Calyx had the
same bug*, found by this measurement and fixed (the delivery time is
recorded and checked against the deadline); before the fix it got 5/6.

== Shared memory (W7)

A conversation turn that adds facts to a user's memory, with simultaneous
runs and crashes between storing the memory and recording it. Calyx gets
3/3. LangGraph's `Store` loses updates with 20 simultaneous runs (in 3 of 6
rounds, losing 1 to 3 conversations), stores again after a crash and resume,
and repeats 30 facts when 10 runs all crash and resume; with manual care
(one item per fact, keyed by message id) it gets 3/3.

== Bugs before running (Q2)

A corpus of 54 state bugs, each a Calyx program with its expected outcome in
its header: the compiler catches 35, 2 cannot be written at all, the runtime
catches 11 and 6 escape; 48 of 54 never cause damage. Of the 16 ported to
typed Python + LangGraph, pyright and mypy catch 2 of the 14 that Calyx
rejects, and LangGraph stops 3 when running, 2 of them after the damage.
The corpus and the ports were written by the compiler's author, which is the
main threat to this result. A kit for an independent port (E4) is in
`bench/e4_porting/`; it has not been run yet.

== Real bugs

To avoid depending on bugs written by the author, we searched the issues of
LangGraph, CrewAI and AutoGen @wu2023autogen with eight searches fixed
before reading the results, and classified each report by what Calyx would
do with the same workflow, taking the class least favourable to Calyx when
in doubt. Of 79 issues, 44 qualified.

#figure(
  table(
    columns: (1fr, auto),
    align: (left, right),
    thick, [*Class*], [*Issues*], rule,
    [Bug in the framework itself (neutral)], [29],
    [Calyx avoids it at runtime], [5],
    [Cannot be written in Calyx], [2],
    [*Calyx lets it through*], [*8*],
    [*Compiler catches it*], [*0*],
    thick,
  ),
  caption: [44 real bugs from LangGraph, CrewAI and AutoGen issues.],
) <tab-real>

The compiler caught none. Issues report the framework misbehaving, not the
programmer's mistake: whoever forgets the idempotency key does not open an
issue, they find the double payment in production. Public issues do not
measure Q2 well, and they do not confirm it. The study did find a real gap
(a `write once` inside a retry loop pays on every turn), which became
warning `W0605`; the counts above are for Calyx before the study.

== An external benchmark: LIMBO

LIMBO @li2026limbo injects faults into six simulated services and checks, in
a ledger, what each one actually did. We wrote its 12 tasks as Calyx
programs, declaring the tools only from the documentation an agent sees, and
ran them on its E2 grid (205 episodes in which the fault fired) with LIMBO's
own fault injector and grader, unchanged. There is no model: each task is a
fixed program, so what is compared is recovery. The models' numbers are the
ones LIMBO publishes.

#figure(
  table(
    columns: (1fr, auto, auto, auto),
    align: (left, right, right, right),
    thick, [], [*EOS*], [*Dup.*], [*TS*], rule,
    [*Calyx, native contract*], [*76%*], [*24%*], [*100%*],
    [3 frontier models, vanilla], [74–79%], [20–26%], [99.5–100%],
    [Best contract-aware harness], [77%], [23%], [100%],
    [Outcome oracle], [88%], [12%], [100%],
    [*Calyx, key on every write*], [*100%*], [*0%*], [*100%*],
    thick,
  ),
  caption: [LIMBO E2 grid. EOS: exactly-once success; Dup.: episodes with a
    duplicate; TS: task success.],
) <tab-limbo>

Under the native contract (only payments and one social platform accept a
key), Calyx matches the best models and does not beat them. All its
duplicates come from two fault modes no client can fix without a key:
transport redelivery (74% for every system, the oracle included) and late
commits, where the write is still in flight when the read-back looks (71%;
the best harness has 69%). Where a read-back resolves the fault, Calyx never
duplicated. When every write accepts a key, duplicates vanish by
construction: the key is in the tool's declaration, not in the model's
choice at each call. This supports, from the outside, LIMBO's conclusion
that the guarantee lives in the tool contract; Calyx's contribution is to
make that contract written, checked and the same in every run, at no token
cost.

LIMBO found four gaps in Calyx, all fixed before these numbers: `verify`
could only say whether a write happened, not return what it made (now it
can, which avoided a duplicate in 40 of the 205 episodes); a tool server
could not report that the service behind it timed out; a batch write cut in
half could not finish (now `batch`); and a person resuming a pause could not
give the tool's answer (now `--uncertain done=<answer>`). The batch now
verifies instead of pausing, which costs something: under a late commit it
duplicates like every other verified write.

== Wrong declarations

The weak point of the approach is the declaration itself: if the programmer
declares an effect wrongly, the compiler believes it. We declared each of
LIMBO's 22 tools each possible way (`read`, unkeyed `write`, keyed `write`,
`write once`) and classified each declaration with LIMBO's internal
contracts. A declaration is dangerous if the runtime may repeat a
non-idempotent write.

#figure(
  table(
    columns: (1fr, auto),
    align: (left, right),
    thick, [*Dangerous declaration*], [*Warned*], rule,
    [Non-idempotent write declared `read`], [10 of 10],
    [Non-idempotent write as unkeyed `write`], [10 of 10],
    [Keyed `write` on a service that ignores keys], [*0 of 8*],
    thick,
  ),
  caption: [What MCP annotations catch. 3 of 60 safe declarations were also
    warned (idempotent writes declared `read`).],
) <tab-decl>

The annotations catch treating a write as a read and forgetting the key.
They miss the mistake that mattered most in LIMBO, trusting a key the
service ignores, because MCP annotations have no vocabulary for idempotency
keys @mcp2025spec. An annotation saying "this tool honours an idempotency
key" would close the gap.

== Writing a real workflow

A customer-support example (typed triage, an agent with read-only tools,
refunds with approval and deadline, customer memory) ran with Gemini on all
three paths and exposed two language problems, both fixed: the wait for
approval started before the proposal existed, and there was no way to show
the person what they were approving (now `receive ... about`). It also
exposed a limit the language does not address: the model promised actions
it did not take.

= Threats to Validity

*Same author on both sides.* The baselines, the bug corpus and the LIMBO
programs were written by Calyx's author. LIMBO's tasks, faults, grader and
published baselines are by others, and the LIMBO adapter only uses LIMBO's
public tools; the independent port (E4) is pending. *Paper proofs.* T1–T3
have proof sketches and a bounded exhaustive check of a hand-written model,
not a mechanized proof or a model extracted from the code. *Bugs found in
Calyx.* Several Calyx bugs were found by measuring and fixed before the
numbers reported; we say where. *Wrong declarations.* The check against MCP
annotations only works for servers that send them, and misses ignored keys.
*Models.* Most experiments use fake models; W1 and W2 were repeated with
Gemini at small N, and in W2 the model always proposed the same amount, so
the risk of a different answer after a crash was not exercised. *Setting.*
One machine, no real network; versions are pinned. *Real-bug study.* Issues
read from their summaries, one classifier, a sample limited by GitHub
search.

= Related Work

*Duplicated effects in agents.* LIMBO @li2026limbo measures where the
exactly-once guarantee should live and finds it depends on the tool
contract. Other studies count idempotency violations under retries
@gopnalswamy2026idempotencybench and evaluate recovery from ambiguous tool
outcomes @sun2026didithappen; a tool wrapper with post-condition
verification, idempotency keys and verify-before-retry reduces duplicates
@mansoor2026verified, the same mechanism as Calyx's `verify` and keys,
offered as an optional library. Calyx starts from the same conclusion and
makes the contract mandatory and compiler-checked.

*Transactions and runtime enforcement.* GoEX @patil2024goex argues for undo
and damage confinement; SagaLLM @chang2025sagallm, Atomix
@mohammadi2026atomix, Cordon @chen2026cordon and Agentic Transaction
@sun2026agentictx provide transactional or compensating execution;
AgentRewind @zhuang2026agentrewind checkpoints and rewinds agents;
AgentSpec @wang2026agentspec enforces user rules at runtime. These are
runtime mechanisms; Calyx asks the program to declare what they need and
checks it before running.

*Static analysis and calculi for agents.* Recent work analyses agent
programs written in existing frameworks for structural properties
@agentproof2026 and non-termination @ialscan2026, gives a typed calculus for
agent composition @lambdaA2026, or tracks information flow in LLM programs
@garby2026llmbda. Calyx checks a different property, the contract of
external effects, which existing frameworks do not write down anywhere; its
formal core could extend such a calculus with journals and crashes.

*Frameworks and durable execution.* ReAct @yao2023react interleaves
reasoning and acting in a model-driven loop, which Calyx offers as a bounded
construct; AutoGen @wu2023autogen organizes agents as conversations; DSPy
@khattab2024dspy optimizes prompts of typed pipelines; LangGraph @langgraph
makes the graph explicit and checkpoints each step. Temporal @temporal and
Restate @restate journal each step and resume without redoing it; in both,
the idempotency of an external effect is the programmer's responsibility.
Calyx ties Temporal on recovery when the care is written, and differs by
requiring it.

= Conclusion

Measured against LangGraph and Temporal on crashes, waits and concurrency,
the result is consistent: the baselines are right when the programmer writes
the care, and Calyx requires it to be written. On an external benchmark,
Calyx ties the best models when tools do not accept keys and reaches zero
duplicates when they do, which supports the claim that the guarantee lives
in the contract and that requiring it is worthwhile. The runtime's cost is
small and its parallelism matches hand-written asyncio. The weakest part of
the evidence is the one that matters most for the thesis: whether the
compiler catches, before running, the bugs real programmers make. The
author's corpus says yes; public issues say neither yes nor no; an
independent port is the next step.

*Availability.* Calyx, the experiments and their data are in the
repository #link("https://github.com/daltonfontes/calyx"), whose `bench/`
directory has the scripts that produce every number in this paper.

#set text(size: 7.5pt)
#bibliography("refs.bib", style: "association-for-computing-machinery", title: "References")
