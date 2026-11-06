# Security

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private reporting instead:
[Report a vulnerability](https://github.com/daltonfontes/calyx/security/advisories/new).
Say what you found, how to reproduce it, and what it lets someone do. You
should get an answer within a week. If that link does not work for you,
open an issue that says only that you have a security report, with no
details, and you will be given a private way to send it.

Only the latest release is supported: fixes go into a new release, not into
older ones.

## What counts

A vulnerability is a way to break a guarantee Calyx states, for example:

- an effect applied twice, or lost, where the program's contract says it
  cannot be (`write` with `idempotency_key`, `write once` with
  `on_uncertain`), outside the hypotheses in the README's limitations;
- a call that goes out although its tool's policy forbids it;
- a delivery accepted from a sender that `calyx.toml` does not allow, or a
  `calyx serve --http` caller reaching a graph it was not given;
- a tool reaching outside its sandbox through Calyx itself, a crash or
  memory error in the runtime from a crafted journal, IR or model answer;
- secrets (API keys, tokens) written to the journal, the trace or telemetry.

## What does not

These are documented limits, not vulnerabilities:

- A sandbox is a copy of a directory, not an isolation boundary. Tools run
  with the permissions of whoever runs Calyx, so a tool that runs code
  (tests, scripts) runs it with those permissions. Run agents over
  untrusted code inside a container or a VM.
- `calyx serve --http` without `tls_cert` and `tls_key` is plain HTTP: on
  an address other than 127.0.0.1 it warns when it starts, and tokens cross
  the network in clear text.
- A service that ignores idempotency keys can apply a write twice; Calyx
  cannot see inside the service (`calyx check --tools --probe` tests it).
- What a model decides to do with the tools it is given is bounded by the
  program (its tools, policies and limits), not by Calyx guessing intent.
