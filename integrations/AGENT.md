# Integrate Monohero into Codex or Claude Code

Read this instruction, then adapt the project's AGENTS.md (Codex) or CLAUDE.md
(Claude Code). Preserve existing rules. Ask the owner to choose the queue name
and the authorized workflow if these are not already explicit. Do not install
hooks, send messages, change deployment policy, or launch releases implicitly.

## Project configuration to establish

- Monohero executable on PATH (install: `cargo install --git https://github.com/xen/monohero --locked`).
- One explicitly started `monohero serve` per user, supervised by launchd/systemd
  or kept in a terminal. All worktrees use the same MONOHERO_HOME.
- An explicit queue name, e.g. `my-project-release` or `another-project-release`. Worktrees
  of the same project MUST share it. Different projects can use separate queues;
  projects sharing an exclusive resource can deliberately share one queue.
- `--cwd` is the working directory of an individual job, not the queue identity.
  Omit it to capture the directory from which submit runs. Different worktrees
  submit with the same queue name and distinct job keys; each command runs in
  its own captured directory. Keep queued worktrees unchanged until completion.
- An executable project workflow that completes the authorized operation and
  exits. It must stop on errors, propagate exit codes, and never detach protected
  work. Keep sync, checks, review, merge and deploy inside the same job when the
  project requires all of them to be exclusive. Never bypass approvals.

## Rule to add to the project

After explicit release authorization, prepare the release workflow, then submit
it ONCE with `monohero submit --queue PROJECT-release --key UNIQUE-RELEASE-ID --cwd
ABSOLUTE-WORKTREE -- /absolute/path/to/workflow`. Read the returned job ID and
end the turn, reporting only that it is queued. Do not run release checks before
the queue starts them. Do not poll status, tail progress, exchange coordinator
messages, or report percentages. Repeating the same submission key is safe only
with identical inputs. A new follow-up operation uses a new key and joins the tail.

The daemon executes the workflow even after this chat closes. It does not invoke
an LLM or consume model tokens. In a client that supports a genuinely blocking
background completion event, use `monohero wait ID` once and let the host deliver
the result; do not repeatedly poll the tool session. If the host requires polling,
end the turn and let the user resume later, or configure an explicitly approved
`--on-complete /absolute/executable` notification callback. That executable receives
JOB_ID and STATE as separate arguments. It is an ordinary local executable, NOT
an automatic instruction or message injection into any chat.

For an interactive workflow, the executable may launch a separately authorized
Codex/Claude CLI process with the task instructions. This costs tokens only when
that process runs; it does not resume the original UI chat. The executable must
wait for the whole operation to finish, not spawn a detached agent. Do not launch
a nested agent without owner authorization. A workflow that requires manual
judgment may stop as failed; inspect it and explicitly resolve the queue barrier.

When resumed by the user, inspect `monohero status --queue PROJECT-release` once
and read the completed job log. Failed/interrupted jobs block their queue. Inspect
external effects and surviving processes BEFORE `monohero resolve ID`. Resolve
acknowledges inspection, never retries. Submit subsequent work with a new key.
Queued jobs can be cancelled with `monohero cancel ID`; running jobs cannot.

## Limits to state honestly

Monohero serializes executable workflows. It cannot force unrelated commands or
other machines into its queue, guarantee zero tokens in a polling host, or wake
an arbitrary Codex/Claude chat. It does not turn a half-written release script
into a complete deployment system. The project supplies that workflow.
