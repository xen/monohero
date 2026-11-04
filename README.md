# Monohero

Local FIFO queues for exclusive agent workflows. One job works; the others wait
without asking a model for status. Written in Rust. macOS and Linux.

Give each project its own queue, or deliberately share a queue when projects
compete for the same resource. All worktrees use the same service and queue name.

## Use

Copy and send this to your agent:

```text
Read https://github.com/xen/monohero/blob/main/integrations/AGENT.md and set up Monohero for this project's Codex/Claude instructions, choosing a unique queue name based on the project name. Preserve existing release approvals and checks.
```

The same instruction is available locally with `monohero guide`. See the complete
[agent integration guide](integrations/AGENT.md), plus
[Codex and Claude examples](docs/agents.md).

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

## Desktop dashboard

The native desktop app uses [GPUI Kit](https://gpui-kit.com/). Build it separately
so the command-line service retains its Rust 1.81 requirement. The desktop app
requires Rust 1.99.1+, Xcode with Metal tools and macOS 15+ on macOS:

```sh
MACOSX_DEPLOYMENT_TARGET=15.0 cargo +1.99.1 build --release --locked --manifest-path desktop/Cargo.toml
monohero dashboard --ui "$PWD/desktop/target/release/monohero-ui"
```

On macOS the menu-bar icon opens the queues window. Closing or hiding that window
keeps the app available in the menu bar; quitting the dashboard leaves the queue
service running. `monohero dashboard --hidden` starts with the window hidden.
Install `monohero-ui` beside the CLI to omit `--ui`. Both use the same `--home`.
Use `./scripts/bundle-macos.sh` to create `dist/Monohero.app`; this is an ad-hoc
signed local bundle, not an Apple-notarized distribution.

The dashboard shows independent project queues, chat observations, failed jobs and work directories. Move waiting jobs up/down, pause admission, resume
deferred jobs at the tail, cancel pending work, clear history or delete queues.
Destructive actions require confirmation. Clearing keeps durable job keys and
audit records, preventing an old submission from running again accidentally.
Running command workflows cannot be force-cancelled. Failed and interrupted jobs remain visible for inspection.

### Blocked chats and monitoring

Choose a policy per queue: an attention list (manual resume) or a held position at
the tail. A held chat cannot be granted again until new evidence shows it ready.

```sh
monohero policy --queue my-project-release --blocked attention --unclaimed-after 180
monohero configure-monitor --codex /absolute/path/to/codex --interval 15
```

Monitoring makes read-only native RPC requests; it never invokes a model or sends
periodic status prompts. The default Codex history connection observes persisted
turn/goal metadata. It cannot prove the desktop runtime's live approval/input
state. Configure `--proxy [--socket /absolute/public/control.sock]` only for the
owning host's public app-server proxy. Connection errors stay visible as unknown;
Monohero does not switch to a different connection or use private desktop sockets.

When monitoring confirms that a Codex chat is archived, Monohero cancels its
waiting and deferred jobs. For a running managed grant, cancellation first drains
protected commands; legacy claimed work still requires its owner's explicit yield.
If the archive list is unavailable, no chat is inferred to be archived. Worktrees,
job records and failure details are retained.

New chat grants require protected commands to run through
`monohero run JOB_ID --thread CHAT_UUID -- COMMAND ARGS`. Deferral first stops new
commands, then waits for inherited command locks to drain. A failed protected
command preserves ownership until the owner inspects effects and explicitly
yields. Existing legacy claimed grants require owner cooperation and are never
revoked based on inactivity. Future native workers launched by the updated
executable adopt this fencing protocol; stored submission arguments stay intact.

After a controlled service upgrade, explicitly adopt pending native workers from
an older version with `monohero adopt-workers --from /absolute/old/monohero`.
This preserves their job IDs, order, immutable arguments and saved goal keys;
only future matching native workers use the upgraded executable and fencing.
Running grants are unaffected. Inspect all queues and stop the old service only
when no job or completion callback is running. Never restart over protected work.

Queue management is also available through `reorder`, `defer`, `resume`,
`pause-queue`, `resume-queue`, `clear` and `inspect`; consult each command's `--help`.

## Command-line usage

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

Failed and interrupted jobs remain in history and do not block later jobs. `resolve` acknowledges inspection and retains the log and exit code. It never retries.
Unknown IDs and invalid transitions are explicit errors. There is no force-unlock.

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
failure handling, cancellation, persistence, surviving children and callbacks.
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

## View queues

```sh
monohero queue                            # unfinished jobs in every queue
monohero queue --queue my-project-release # one project's FIFO queue
monohero queue --all                      # include completed history
monohero queue --queue my-project-release --json
```

The readable snapshot shows the current job, waiting positions, failures,
worktree directories and destination chat IDs. Positions are per queue; different
projects remain independent. Failed/interrupted jobs explicitly show why a queue
cannot advance. Viewing never claims, resolves or retries a job. It reads once
and exits; there is no automatic refresh. `status` retains its existing JSON format.

Release approval and queue mode are separate decisions: once the human has
approved releasing this chat's work and using native chat handoff, agents should
enqueue and resume the same chat without asking again about the mechanism.
Projects should replace stale command-only rules in their AGENTS.md and release
protocol together. Queueing never expands the approved work or waives checks.

## Wake unloaded Codex app chats

Some desktop hosts store `codex queue` messages without loading an inactive
chat. In that case a grant stays `message_queued`, no agent claims it, and later
jobs correctly remain blocked. Opening the existing chat's native link can let
the app consume that already queued message.

On a macOS desktop with Codex installed, explicitly enable native URL opening:

```sh
monohero configure-wake --opener /usr/bin/open
```

After accepting each chat message, Monohero invokes that executable once with
`codex://threads/EXACT_UUID`. This opens/selects the existing chat and can bring
the app forward. It creates no new chat or second prompt, changes no permissions,
and never claims or finishes the job. An actual `claim` is still required.
The configuration is shared by all queues in the same Monohero home. Future
workers read it when dispatching, including previously queued jobs that use the
updated CLI. Running workers are not changed or restarted.

The integration was verified on a macOS desktop by waking a previously unloaded
chat and observing its actual claim. Other hosts must verify URL handling and
queued-message consumption; successful URL opening alone proves neither.
An opener failure is explicit: the accepted message ID remains recorded, the job
fails and blocks its queue, and nothing is resent or retried automatically.
Without `app-wake.json`, URL opening is disabled. Remove that file from the
shared Monohero home to disable it again. Do not enable a periodic status loop.
