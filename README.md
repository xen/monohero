# Monohero

Local FIFO queues for exclusive agent workflows. One job works; the others wait
without asking a model for status. Written in Rust. macOS and Linux.

Give each project its own queue, or deliberately share a queue when projects
compete for the same resource. All worktrees use the same service and queue name.

## Install

Requires Rust 1.81+, a C compiler (bundled SQLite), and Git.

```sh
cargo install --git https://github.com/xen/monohero --locked
monohero serve
```

Keep the service in a dedicated terminal or use a user service; see
[service setup](docs/service.md). It does not auto-start or modify your machine's
startup configuration. Default state: `~/.local/state/monohero`. Override with
`MONOHERO_HOME` or `--home /absolute/short/path` on **every** command. Keep the path
short enough for Unix socket limits (about 100 bytes, platform dependent).

## Use

In another terminal, submit an executable workflow:

```sh
monohero submit --queue otkuda-release --key release-2026-11-02-001 \
  --cwd /absolute/path/to/worktree -- /absolute/path/to/release.sh
```

This returns a JSON job ID immediately. The service owns the job; ending the
chat or disconnecting the submitter does not cancel it. FIFO means the order in
which submissions are committed by the service. Different queues run concurrently:

```sh
monohero submit --queue another-project-release --key release-001 \
  --cwd /absolute/path/to/another-project -- /absolute/path/to/release.sh
monohero status --queue otkuda-release
monohero wait 1
```

`status` is a single snapshot. `wait` blocks on a socket until terminal state,
prints the job result and exits 0 only for success. Neither service scheduling nor
wait uses a periodic status loop. Job output goes to private `ID.log` files in the
state directory, not the chat. No percentages or coordinator messages.

A queue/key pair uniquely identifies a submission, including after restart.
Repeating identical inputs returns the same ID; different inputs with the same
key are rejected. Use a new key for a follow-up operation; it joins the tail.
Keys identify submitted arguments, not file contents: don't edit a queued script
or its worktree. Prefer a committed immutable workflow and exact revision inputs.

```sh
monohero cancel 3   # queued jobs only
monohero resolve 2  # after inspecting a failed/interrupted job
```

Failed and interrupted jobs block their queue. `resolve` acknowledges inspection,
retains the log and exit code, and lets later jobs proceed. It never retries.
Unknown IDs and invalid transitions are explicit errors. There is no force-unlock.

## Give this to your agent

> Read https://github.com/xen/monohero/blob/main/integrations/AGENT.md and integrate
> Monohero into my project's Codex/Claude instructions. Use queue `my-project-release`.
> Preserve existing release approvals and checks. Prepare the executable workflow;
> do not release anything until I authorize it.

The same instruction is available locally with `monohero guide`. See the complete
[agent integration guide](integrations/AGENT.md), plus
[Codex and Claude examples](docs/agents.md).

**Monohero runs commands, not conversations.** It does not wake arbitrary UI chats.
For quiet operation submit a complete workflow and end the turn. A host may attach
one blocking `wait` if it supports completion events without model polling.
An explicitly configured `--on-complete /absolute/executable` can notify a user or
an integration. It receives the job ID and state; callbacks are best effort,
recorded separately, and never retried automatically. They run after command execution,
not on cancellation or recovery. No messages are sent by default.
The next job in that queue waits for the callback to exit too; keep callbacks short.

## Execution and crash semantics

- Commands are executed directly, without a shell; use `/bin/sh script.sh` explicitly
  if needed. Standard input is closed. Environment and PATH come from the service,
  not from the submitting chat. Restart the service to change its environment.
- The service and jobs run as your local user. The socket and journal are private,
  but any program running as that user is trusted. This is not a sandbox. Do not
  pass secrets in arguments; load them in the workflow from private configuration.
- Successful submission means the job has been committed to SQLite. Pending jobs
  survive a service restart. A lost reply can be retried with the same key.
- A running job at crash time becomes interrupted on restart, even if external work
  completed. Its queue is blocked until inspection. Nothing retries automatically.
- Child processes inherit the daemon ownership lock. A replacement service refuses
  to start while a surviving child holds it. Wait for it to finish; inspect external
  effects before resolving. Do not daemonize protected work, close inherited lock
  descriptors, or exit a wrapper while its background children still work.
- A callback interrupted by a crash is recorded as interrupted; it is not replayed.
  Job completion is durable before callback invocation. There is no exactly-once
  guarantee for external side effects or notifications.
- One service, one local host. Separate homes or machines are independent and do
  not exclude one another. Monohero does not prevent commands run outside it.
- Jobs have no automatic timeout. A hung command/callback keeps its queue occupied.
  Inspect the process and terminate it deliberately; failure then blocks the queue.
- Logs and completed records are retained; no automatic deletion or rotation.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Black-box tests cover FIFO, parallel queues, deduplication, blocking wait,
failure barriers, cancellation, persistence, surviving children and callbacks.
See [the design](docs/specs/2026-11-02-monohero.md). MIT licensed.
