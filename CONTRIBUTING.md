# Contributing

Thanks for trying Calyx. Bug reports are the most useful contribution right
now: Calyx is young, and every report from a real program finds something
its own tests did not.

## Reporting a bug

[Open an issue](https://github.com/daltonfontes/calyx/issues/new/choose)
with the smallest `.clyx` program that shows it, the command you ran, what
you expected and what happened, and `calyx --version`. If a run went wrong,
the run's journal (`.calyx/runs/<id>/journal.jsonl`) helps a lot; remove
anything private from it first. Security problems go through
[SECURITY.md](SECURITY.md) instead.

## Building

You need Rust (stable) and a C compiler.

```bash
make build      # the compiler, the runtime, target/release/calyx
make test       # every test
make lint       # rustfmt and clippy, as CI runs them
```

The PostgreSQL tests run when `CALYX_TEST_DATABASE_URL` points to a
database they may use (`postgresql://user@127.0.0.1:5432/postgres`).

## Where things are

The [project overview](docs/visao-geral.md) has the folder of each part;
the short version: `compiler/` is the compiler and the CLI (Rust),
`runtime/src/` the interpreter (C), `runtime/rs/` its I/O layer (Rust), and
`docs/spec/calyx.md` the language, every check and every error code.

## Changing Calyx

- Open an issue first for anything larger than a fix, so we can agree on
  the design before you write it. The design decisions, and why, are in
  [docs/discovery/03-decisoes.md](docs/discovery/03-decisoes.md).
- A change to the language updates the spec (`docs/spec/calyx.md`) and
  comes with tests: a program in `tests/programs/` with its `.expected`
  diagnostics (`UPDATE_EXPECT=1 cargo test -p calyx-check` writes them), and
  an end-to-end test in `compiler/calyx-cli/tests/` if it changes a run.
- If a change makes an example compile to a different program,
  `tests/versions.rs` fails: runs started by the previous version could not
  be resumed by this one. That needs a new minor version, and the pinned
  hashes updated with `CALYX_UPDATE_HASHES=1 cargo test -p calyx-cli --test versions`.
- Bugs found by the fuzzers go into `runtime/fuzz/corpus/*/` as
  `regression-*` files.
- The docs are mostly in Portuguese, the code and its comments in English.
  Either language is fine in issues and pull requests.
