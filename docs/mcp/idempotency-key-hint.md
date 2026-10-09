# SEP-0000: Idempotency keys on `tools/call`, declared per tool

- **Status**: Draft (not yet submitted)
- **Type**: Standards Track
- **Created**: 2026-10-09
- **Author(s)**: Dalton Fontes (@daltonfontes)
- **Sponsor**: None (seeking sponsor)
- **PR**: to be opened

> **AI disclosure.** This proposal and its prototype were written with Claude
> (Anthropic) as a coding assistant, directed and reviewed by the author. Every
> number below comes from scripts in the
> [Calyx repository](https://github.com/daltonfontes/calyx) and can be
> rerun with the commands in [Reference Implementation](#reference-implementation).

## Abstract

Agents retry tool calls after timeouts. For a write that is not idempotent
(send an e-mail, create a charge), a timeout leaves the outcome uncertain:
retrying may apply it twice, not retrying may lose it. The standard remedy is
an idempotency key, which the service uses to deduplicate repeated requests.
MCP has no way for a client to send such a key, and no way for a server to
say whether a given tool honours one.

This SEP adds both: an optional `idempotencyKey` on `tools/call` (as proposed
in SEP-3182), and a per-tool annotation, `idempotencyKeyHint`, that tells the
client whether the server deduplicates calls to that tool by key. The
annotation is what lets a client choose, per tool, between retrying with the
key and verifying or asking a person instead. In a measurement on the LIMBO
benchmark's 22 tools, existing annotations flag 20 of 28 dangerous effect
declarations and none of the 8 that rely on a key the service ignores; with
`idempotencyKeyHint`, all 28, with no new false positives.

## Motivation

**Retries are where agents duplicate effects.** A tool call times out; the
request may or may not have been applied. LIMBO
([jaxblack/limbo-bench](https://github.com/jaxblack/limbo-bench)), a benchmark
that injects faults into six simulated services and checks what each one
actually did, finds that frontier models in a plain agent loop duplicate a
side effect in 20–26% of faulted episodes, and that the guarantee depends on
the tool contract rather than on the model: when every write accepts an
idempotency key, duplicates drop sharply. In our runs of LIMBO's 12 tasks as
fixed Calyx programs, a key on every write took exactly-once success from 76%
to 100%.

**MCP cannot express the contract.** The current `ToolAnnotations` have
`idempotentHint`: "calling the tool repeatedly with the same arguments will
have no additional effect". That describes the operation itself. Most writes
that matter are not idempotent in that sense (two calls to `send_mail` send
two e-mails) but can be made safe to retry by deduplicating on a key the
client supplies. There is no field to carry the key, and no annotation to say
the tool honours it.

**Support is per tool, not per server.** A server often fronts several
services, and only some of them honour a key. In LIMBO's native contract,
`billing_create_charge` honours a key, `mail_send` does not, and
`social_publish` honours it on one platform (Mastodon) and silently ignores it
on the others. A server-wide capability cannot say this.

**The cost of not knowing, measured.** Calyx is a workflow language in which
every tool declares its effect (`read`, `write` with an idempotency key, or
`write once` with a policy for uncertain outcomes), and its checker compares
those declarations with the server's annotations. We declared each of LIMBO's
22 tools each possible way and classed each declaration with LIMBO's internal
contracts (which the agent never sees). A declaration is *dangerous* if the
client may repeat a non-idempotent write.

| Dangerous declaration | Flagged with today's annotations | Flagged with `idempotencyKeyHint` |
|---|---|---|
| Non-idempotent write declared read-only | 10 of 10 | 10 of 10 |
| Non-idempotent write retried without a key | 10 of 10 | 10 of 10 |
| Retried with a key the service ignores | **0 of 8** | **8 of 8** |
| Safe declarations flagged (false positives) | 3 of 60 | 3 of 60 |

The third row is the mistake that caused the most duplicates in LIMBO, and
today's annotations have no vocabulary to catch it.

## Specification

### 1. The key on `tools/call`

`CallToolRequestParams` gains an optional field, as in SEP-3182:

```typescript
export interface CallToolRequestParams extends InputResponseRequestParams {
  name: string;
  arguments?: { [key: string]: unknown };
  /**
   * A client-chosen key identifying one logical operation. A client that
   * retries an operation MUST send the same key on every attempt, and MUST
   * NOT reuse a key for a different operation.
   *
   * 1 to 255 printable ASCII characters.
   */
  idempotencyKey?: string;
}
```

### 2. The annotation

`ToolAnnotations` gains:

```typescript
export interface ToolAnnotations {
  // ... existing hints ...

  /**
   * If true, the server deduplicates calls to this tool by `idempotencyKey`:
   * a call whose key matches an earlier call to the same tool, with the same
   * arguments, returns the earlier call's result without applying the effect
   * again.
   * If false, the server ignores `idempotencyKey` for this tool.
   * If absent, the client cannot assume either.
   *
   * (This property is meaningful only when `readOnlyHint == false`)
   */
  idempotencyKeyHint?: boolean;
}
```

### 3. Server behaviour, when `idempotencyKeyHint` is true

For calls to that tool from the same authenticated principal:

- A call with a key not seen before is executed normally, and its result is
  retained with the key.
- A call with a key already completed and the **same arguments** MUST return
  the retained result and MUST NOT apply the effect again.
- A call with a key already used with **different arguments** MUST fail with a
  JSON-RPC error (code to be assigned), without applying the effect.
- A call with a key whose first call is still running SHOULD wait for it and
  return its result; it MAY fail with an error the client can treat as
  "outcome still uncertain". It MUST NOT apply the effect a second time.
- Results SHOULD be retained for at least 24 hours. The server MUST NOT claim
  the hint if it cannot retain them for some period it documents.
- A server that fronts another service MAY satisfy this by forwarding the key
  to that service's own idempotency mechanism (for example, Stripe's
  `Idempotency-Key` header). It MUST NOT claim the hint for a tool whose
  service ignores the key, even partially (as with `social_publish` above): a
  tool whose support depends on its arguments either claims `false` or is
  split into tools that can each answer truthfully.

### 4. Client behaviour

- A client MAY attach `idempotencyKey` to any call. It SHOULD do so for every
  call to a tool with `idempotencyKeyHint: true` that it may retry.
- A client that retries after an uncertain outcome MUST reuse the key of the
  first attempt. The key SHOULD be derived from, or recorded in, state that
  survives a client crash: a key regenerated after a restart, or derived from
  a value that was not durably recorded (for example, a model's answer kept
  only in memory), defeats deduplication.
- When `idempotencyKeyHint` is false or absent and the tool is not
  idempotent (`idempotentHint` is not true), a client MUST NOT rely on the key
  to make a retry safe. It SHOULD instead verify whether the effect happened
  (with a read tool) or ask a person before retrying.
- As with all annotations, a client MUST treat `idempotencyKeyHint` as
  untrusted unless the server is trusted.

## Rationale

**A hint with a concrete client action.** The MCP blog's post on tool
annotations (2026-03-16) asks that a new annotation change some concrete
client action, and that hints "still be useful even if some servers get them
wrong". The action here is the choice between retrying a write
automatically (with its key) and verifying or asking first. A server that
wrongly claims `true` leaves the client where every retrying client is today;
one that wrongly claims `false` costs a verification or a question.

**Testable, unlike most hints.** Whether a tool honours a key can be checked
from outside: call it twice with the same key and arguments and count the
effect with a read tool. This makes the hint a candidate for a conformance
test (see Testing Plan), which `readOnlyHint` and `destructiveHint` are not.

**Per tool, answering SEP-3182's open question.** SEP-3182 proposed the key
field and a `tools.idempotency` server capability, and listed capability
granularity as unresolved. The data above (2 of LIMBO's write tools honour a
key, one of them only for some arguments) argues for per tool. A server-wide
capability could still be added as a summary; it is not enough alone.

**Not an extension of `idempotentHint`.** Folding key support into
`idempotentHint` would tell clients that sending the same e-mail twice is
harmless when it is only harmless with the key. Clients that retry on
`idempotentHint` without sending a key would duplicate effects.

**A field, not `_meta`.** The key could travel in `_meta` under a reserved
name (the Calyx prototype uses `_meta["calyx/idempotency_key"]` today). We follow
SEP-3182 in using a field, because the key is part of the request's meaning,
not metadata. The SEP-3182 discussion reported SDKs dropping each of them
(`_meta` in some, the new field in one), so SDK support is needed either way.
The annotation is independent of this choice.

**Prior art.** HTTP idempotency keys (the IETF `Idempotency-Key` header
draft; Stripe's API), Helland's "Idempotence Is Not a Medical Condition"
(ACM Queue, 2012), and durable-execution systems (Temporal, Restate) that
leave the idempotency of external effects to the programmer. LIMBO measures
the effect of key availability on agents directly.

## Backward Compatibility

Both additions are optional. A server that ignores `idempotencyKey` behaves
as today; since it does not claim `idempotencyKeyHint: true`, a conforming
client does not rely on it. A client that ignores the annotation behaves as
today. No existing behaviour changes.

## Security Implications

- **Cross-principal replay.** A server MUST scope keys to the authenticated
  principal (and to the tool), so that guessing or reusing another client's
  key cannot return that client's result.
- **Key contents.** Keys SHOULD be opaque (random, or a hash). They MUST NOT
  carry secrets or personal data, since servers store them and may log them.
- **Resource use.** Retained results cost storage; servers SHOULD bound the
  retention window and the number of keys per principal.
- **Untrusted servers.** A malicious server can claim the hint and apply
  every call anyway. This is no worse than today, where clients that retry
  have no assurance at all; clients that need guarantees should rely on
  trusted servers or verify effects.

## Reference Implementation

The [Calyx repository](https://github.com/daltonfontes/calyx) has a client
and two servers that implement the behaviour (with the key in `_meta` for
now; renaming it to the field is mechanical):

- **Client.** The Calyx runtime takes the key from values recorded in its
  journal and syncs the journal to disk before the call, so a retry or a
  resume after a crash sends the same key (`runtime/src/exec.c`); a write
  declared without a key and without an uncertainty policy gets a warning
  (`W0601`). Its checker reads the annotation and warns (`W0703`) when a write is
  declared with a key on a tool whose server says it ignores keys
  (`runtime/rs/annotations.rs`, run by `calyx check --tools`). The per-call
  protocol is proved at-most-once in Lean (`formal/Effects.lean`), including
  a counterexample showing the duplicate when the key comes from a model
  answer that was not synced before the call.
- **A server whose hint holds by construction.** `calyx serve` exposes a
  Calyx program's graphs as MCP tools and derives their annotations from
  the program: a graph that writes gets `idempotencyKeyHint: true`, and a
  call with a key is tied to one journaled run, so the same key returns
  that run's answer, or resumes it after a crash, and never starts a second
  run (`compiler/calyx-cli/src/serve.rs`, tests in
  `compiler/calyx-cli/tests/serve.rs`).
- **Servers.** `examples/tools/fake_store.py` deduplicates refunds by key.
  `bench/limbo/adapter.py` is an MCP server in front of LIMBO's services; with
  `LIMBO_KEY_HINT=1` it declares `idempotencyKeyHint` per tool, from which
  services behind it honour a key.
- **Measurement.** `python bench/limbo/declarations.py --limbo <limbo-bench>`
  reproduces the "today" column of the table above
  (`bench/results/limbo_declarations.json`); adding `--key-hint` produces the
  `idempotencyKeyHint` column (`bench/results/limbo_declarations_keyhint.json`).

- **Conformance probe.** `calyx check --tools --probe` runs steps 1, 2 and
  the count of the Testing Plan below against any tool configured with a
  `probe` (a setup tool, the arguments, a read tool that counts effects).
  Against Stripe in test mode (`bench/stripe/probe.sh`,
  `bench/results/stripe_probe.txt`), it passes the server above (one refund
  for two calls with the same key) and flags a version of it that stops
  forwarding the key while still claiming `idempotencyKeyHint: true` (two
  refunds, error `E0704`).

## Testing Plan

A conformance test for a server that claims `idempotencyKeyHint: true` on a
tool, given a read tool that counts its effects:

1. Call the tool with key `k` and arguments `a`; record the result `r`.
2. Call it again with `k` and `a`; expect `r`, and one effect.
3. Call it with `k` and arguments `b ≠ a`; expect an error, and still one
   effect.
4. Call it with a new key `k'` and `a`; expect a second effect.
5. Start a call with `k''` and, before it returns, call again with `k''`;
   expect one effect.

## Open Questions

1. The error code for a key reused with different arguments, and whether an
   in-flight duplicate should wait or fail.
2. The minimum retention window, and whether servers should advertise theirs.
3. Interaction with long-running calls (tasks) and multi-round calls
   (SEP-2322): does each round carry its own key?
4. Whether a server-wide summary capability is still useful next to the
   per-tool hint.

## Acknowledgments

SEP-3182 (Request Idempotency) proposed the key field and raised the
granularity question this SEP answers. LIMBO's authors published the
benchmark, its fault injector and its tool contracts, without which the
measurement above would not exist.
