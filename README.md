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
monohero submit --queue my-project-release --key release-2026-11-02-001 \
  --cwd /absolute/path/to/worktree -- /absolute/path/to/release.sh
```

This returns a JSON job ID immediately. The service owns the job; ending the
chat or disconnecting the submitter does not cancel it. FIFO means the order in
which submissions are committed by the service. Different queues run concurrently:

```sh
monohero submit --queue another-project-release --key release-001 \
  --cwd /absolute/path/to/another-project -- /absolute/path/to/release.sh
monohero status --queue my-project-release
monohero wait 1
```

### Worktrees and `--cwd`

`--queue` chooses the shared queue. `--cwd` chooses the directory where **this
job's command** runs; it does not create or select a queue. If omitted, Monohero
captures your current directory when you submit the job. The service can be
started from any directory.

For two worktrees of one project, use the same queue name and submit from each
worktree. Each command runs in its own worktree, one after the other:

```sh
cd /absolute/path/to/worktree-a
monohero submit --queue my-project-release --key task-a -- ./bin/release.sh

cd /absolute/path/to/worktree-b
monohero submit --queue my-project-release --key task-b -- ./bin/release.sh
```

Equivalently, pass `--cwd /absolute/path/to/worktree-a` from any directory.
Relative executable paths such as `./bin/release.sh` resolve inside that job's
working directory. All clients must connect to the same service using the same
`MONOHERO_HOME` (or its shared default). Keep each worktree available and unchanged
until its job finishes. Use different keys for different worktree jobs.

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

Command mode runs executable workflows. For existing Codex conversations, use the
explicit enqueue/finish and notify modes described below. It does not wake arbitrary UI chats.
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

## Resume an existing Codex chat (0.2)

Queue the chat itself instead of preparing a shell workflow:

```sh
monohero enqueue --queue my-project-release --key task-42 \
  --thread "$CODEX_THREAD_ID" --message "Continue the authorized release."
```

End the turn after submission. On grant, Monohero sends a message through the
Codex CLI's `queue --thread UUID --message TEXT`. The Codex host continues the
same conversation. No separate CLI agent, new chat or coordinator model is started.

The destination executes `monohero claim JOB_ID --thread UUID`, performs all
protected work, then `monohero finish JOB_ID --thread UUID`. A queued message is
only transport acceptance: the claim is confirmation that the chat resumed.
There is no automatic expiry. Wrong-thread claims/finishes are rejected. Missing
claims remain visibly unacknowledged instead of pretending delivery succeeded.
No protected work is allowed after finish. Grants to the same thread are also
serialized across independent project queues.

`monohero notify JOB_ID --thread UUID` independently waits for a command job and
messages its owner on completion. The destination runs the supplied `acknowledge`
command, inspects evidence and continues authorized follow-up. A notice grants no
release slot. These commands work with a running 0.1.1 service through the stable
command protocol; active jobs need not be killed or restarted. Keep versioned
worker executables available unchanged while queued jobs reference them.

Installations must verify that their Codex host consumes queued messages and
starts idle chats. Unsupported hosts leave an explicit unacknowledged receipt;
Monohero never substitutes a new session. Automatic Claude resumption is outside
this release. See [agent instructions](integrations/AGENT.md).


## Explicit goal waiting (0.3)

An active Codex goal may continue scheduling turns while its release waits.
When the user explicitly asks to pause that goal during queue waiting, use:

```sh
monohero enqueue --queue my-project-release --key task-43 \
  --thread "$CODEX_THREAD_ID" --park-goal \
  --message "Continue the authorized release."
```

The existing goal is paused, then restored after the grant message is queued.
Its objective, budget and accumulated usage are preserved. The destination still
claims and finishes its slot. Changed goals are never intentionally overwritten;
uncertain lifecycle changes stop for inspection. A cancelled/failed job leaves
its goal paused until explicit `unpark-goal`. Without this opt-in, goals are
unchanged and may continue independent work. See [goal lifecycle and recovery](integrations/AGENT.md#existing-goals-while-waiting), including host timestamp
requirements and the public API's lack of atomic compare-and-set revisions.
