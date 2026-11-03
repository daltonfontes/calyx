# Changelog

Calyx follows [semantic versioning](https://semver.org) loosely while it is
below 1.0: a minor version (0.x.0) may change the language or the CLI, a
patch version (0.x.y) does not. Runs started by one version are resumed by
the binary of that version, which the installers keep (see
[docs/operacao.md](docs/operacao.md), section 4). Every release, with its
binaries, is on the [releases page](https://github.com/daltonfontes/calyx/releases).

## 0.5.2 (2026-11-03): security

- `calyx serve --http`: a caller could reach the server's own files. It
  could pass any directory of the server (`/root`, `/etc`) as a `Sandbox`,
  and its agent would read it; and a text like `"@/path/file.json"` given
  for a list or record parameter was read from the server's disk. Now a
  caller lends only directories inside the `sandboxes` its
  `[serve.callers.NAME]` lists (none by default), and arguments go to the
  run as JSON, never as `@file`. Over stdio nothing changes. Upgrade if you
  run `serve --http`.
- `calyx serve` gives `Sandbox`, `Nat`, `Date`, `Duration`, `Money` and the
  items of lists their JSON Schema types.

## 0.5.1 (2026-11-03): public beta

- The README says what state Calyx is in, adds a first program that needs no
  clone (`examples/readme/hello.clyx`), and lists the security limits: a
  sandbox is a copy, not an isolation; `serve --http` needs a TLS proxy;
  remote MCP servers take a static token.
- New: `CHANGELOG.md`, `SECURITY.md`, `CONTRIBUTING.md`, issue templates.
- New example: `examples/revisor.clyx`, a code reviewer agent that can only
  read the repository, and ends with a typed report.

## 0.5.0 (2026-10-28)

- `calyx prune --older-than DAYS [--dry-run]` deletes the runs that ended
  (finished or failed), and, with the journal in PostgreSQL, the blobs only
  they used (blobs younger than `CALYX_PRUNE_BLOB_MARGIN_S`, one hour by
  default, are kept).
- Windows: parallel `receive`s could lose a wait, because the C library
  writes in append mode as a seek and a write. Appends now take a lock.

## 0.4.1 (2026-10-28)

- The TLS stack updated for its security advisories; `cargo audit` runs in
  CI on every push and every week.

## 0.4.0 (2026-10-27)

- Windows, natively (no WSL): `calyx.exe`, `install.ps1`, and the fixes
  Windows showed (CRLF sources, non-UTF-8 MCP output, run ids from paths).

## 0.3.9 (2026-10-27)

- A production rehearsal (Stripe in test mode, Gemini, a signed approval,
  PostgreSQL, a `kill -9`, the worker), and the three things its first run
  found.

## 0.3.8 (2026-10-12)

- `calyx worker` runs each run in a process of its own: a long run no
  longer holds up the others (`--max`, 16 by default), and runs that cannot
  resume back off.
- Signed deliveries were refused about 1 time in 8 (a float that did not
  round-trip); fixed.

## 0.3.7 (2026-10-11)

- Policies (D36): rules every call of a tool must meet, checked before it
  goes out, whoever chose the call.
- Telemetry: OpenTelemetry traces and metrics for every run.
- Remote MCP servers over Streamable HTTP, with tokens from the environment.
- Signed deliveries: only allowed senders can answer a `receive`.
- `calyx serve --http`, with callers and tokens, each limited to its graphs.

## 0.3.6 (2026-10-10)

- Structured tool returns: only the declared fields are kept and checked.
- `unordered`, several MCP calls at once per server, `race first N`, `fork`,
  recursion with `decreases`, and `resource`.

## 0.3.5 (2026-10-10)

- Sagas: a race undoes what its losing branches wrote (`compensate`).
- Several machines: the journal, entities and `receive` in PostgreSQL, with
  TLS.

## 0.3.0 to 0.3.4 (2026-10-02 to 2026-10-09)

- Effect rules mechanized in Lean 4; `calyx check --tools` compares each
  declaration with the MCP server's annotations; batch writes that resume
  only what is missing; README and paper in English.

## 0.2.0 (2026-10-01)

- `rounds` and `race` (M9); model routers (M10).

## 0.1.0 (2026-10-01)

- Safe external writes (`write`, `write once`, `on_uncertain`, `requires`),
  sandboxes, entities, the pure layer, and durable `receive`.

## 0.0.1 (2026-10-01)

- Parallelism derived from the graph, agents, loops, `match`, `try`, a
  single binary and `calyx build`.
