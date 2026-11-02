# Codex and Claude Code

Copy the project-specific portion of [AGENT.md](../integrations/AGENT.md) into
AGENTS.md or CLAUDE.md after choosing a queue and an executable workflow. You can
feed that file's URL directly to your agent. `monohero guide` prints it offline.
Do not overwrite existing project instructions. Installing Monohero does not change
any project's instructions, hooks, permissions or release authorization.

## Detached jobs: recommended

Prepare a script that performs the entire authorized operation, exits nonzero on
failure and waits for all protected children. Submit once, keep the JSON ID, end
the agent turn. No waiting chat needs to stay active. The service starts the next
job automatically. On user resumption, read one status snapshot and the log.

```sh
monohero submit --queue my-project-release --key task-42-attempt-1 \
  --cwd /absolute/worktree -- /absolute/worktree/bin/release.sh
```

The script must include every exclusive step, including validation of the intended
Git head and approvals. Submitting only `make test` does not protect a subsequent
merge performed outside the job. Never submit a script that recursively submits
to and waits on its own queue: that deadlocks.

## Interactive reasoning

Monohero cannot hand a lock to an arbitrary paused UI conversation. If a release
needs an agent to interpret review findings, either stop the workflow for explicit
human resolution, or authorize a script that runs the relevant CLI agent and waits
for it. Put the exact task in a file, make the wrapper propagate exit status, and
have the CLI stay attached until the entire operation is finished. Choose CLI
flags according to the installed version's help; do not disable permission checks.
This creates a separate CLI session and may use model tokens once its turn starts.

## Blocking wait and completion callbacks

`monohero wait ID` is a single blocking subscription. It emits no intermediate
progress. A client with event-driven background tool completion can await it.
A client that repeatedly asks the model to poll tool status still spends tokens;
Monohero cannot change that client's runtime. Prefer detached submission there.

An optional completion executable can notify a human or invoke an explicitly
approved integration. It receives two arguments, ID and terminal job state, and
runs from the job's working directory. It is never interpreted by a shell.
Do not send chat messages, inject terminal keystrokes or launch another agent unless
that action is separately authorized. Callback failures do not change job success;
`status` includes `callback_state`. Callbacks are not replayed after a crash and do not run for cancelled or
recovered/interrupted jobs.

## Hooks are not universal wakeups

Codex and Claude have distinct hook lifecycles. A hook executes in response to a
host event; it is not a general API for resuming a particular dormant conversation.
This release deliberately installs no Stop hooks and claims no automatic UI resume.
Do not attach a long queue wait to every Stop event: it can block unrelated turns
and create repeated continuations. Use the explicit workflow submission above.

Official references checked while writing this integration (2026-11-02):

- [Codex hooks](https://learn.chatgpt.com/docs/hooks)
- [Claude Code hooks](https://code.claude.com/docs/en/hooks)
