# Integrate Monohero into Codex or Claude Code

Preserve project instructions, release approvals and checks. Choose one shared
queue name per exclusive resource; all worktrees use the same Monohero home.
Separate projects can use separate queues. Sending to a specific chat requires
the human owner's authorization. Approval to release work in this existing chat
includes enqueueing it and delivering its grant back to the same chat; do not
ask again merely to select `enqueue` instead of a command workflow. Continuing
the same chat does not create a nested agent. A project's previous command-only
instruction is superseded only when its human owner authorizes this policy.
Other chats, new tasks, first release approval and goal pausing retain their
own authorization requirements. Install Monohero and run one explicitly
configured `monohero serve` under a user service.

## Codex: resume this chat when its turn comes

Requires a Codex CLI with `queue --thread UUID --message TEXT` and the Codex
host's queued-message consumption enabled. Register the EXACT existing local
thread UUID, never a name, --last, a fork or a new session. CODEX_THREAD_ID
identifies the current chat in Codex. Submit ONCE:

```sh
monohero enqueue --queue my-project-release --key UNIQUE-RELEASE-ID \
  --thread "$CODEX_THREAD_ID" --message "Continue the authorized release and cleanup."
```

After the returned job ID, END THE TURN. Do not poll status, report percentages,
or exchange coordinator messages. `--cwd` is optional: it captures the current
worktree for that job, not a separate queue. Worktrees of one project use the
same queue and different keys. Keep queued worktrees available and unchanged.
Before claim, repeated submissions from the same exact chat to the same queue
share the original FIFO position. Each request keeps its own key, message and
worktree. Requests submitted after claim receive a new position; different
queues are independent. Deferred groups remain deferred until resumed.

On grant, Monohero sends a user-visible message to that exact chat through
`codex queue`. The host resumes its existing conversation and permission/model
settings; no extra CLI agent or private desktop API is started. Delivery acceptance
alone is NOT proof the chat resumed. The actual destination must first execute:

```sh
monohero claim JOB_ID --thread "$CODEX_THREAD_ID"
```

Read the `requests` array in the claim response and account for every message
in order using its recorded worktree. If the running service predates request
accumulation, a repeated pending submission fails explicitly until the service
can be updated; do not create a duplicate as a workaround.
For inspection after interruption or deferral, `monohero requests JOB_ID`
shows the recorded requests without resubmitting or replaying them.

The grant prompt includes the exact executable and arguments. If claim fails,
STOP without performing protected work. After claim, hold the slot through all
protected sync/check/review/merge/deploy/verification work, including corrections.
After ALL authorized work is complete, execute:

```sh
monohero finish JOB_ID --thread "$CODEX_THREAD_ID"
```

On failure/blocker append `--failed` and explain. Do not finish merely to
acknowledge the grant. Do not enqueue recursively into your own queue. Do no
protected work after finish; only the final user report remains. A claimed slot
requires explicit matching finish, unless a managed grant is safely revoked as
described below. Monitoring can defer an invitation that remains unclaimed past
the configured grace period. Monohero serializes its grants to the same thread
across queues.

## Existing goals while waiting

An active Codex goal can schedule continuation turns independently of Monohero.
Ending a turn alone does not guarantee silent waiting. By default Monohero
leaves all goals unchanged; a goal may continue independent development.

Only when the human explicitly authorizes pausing that goal for its queue wait,
add `--park-goal` to `enqueue`. This requires an existing active goal. Monohero
uses public app-server goal RPC without starting an inference session, saves
the exact goal identity and budget, pauses it, and submits the chat job. After
native message acceptance it restores that unchanged goal to active. Claim and
finish are still mandatory. The objective, budget and accumulated usage stay
intact. It never creates a replacement goal or bypasses limits.

The CLI must support app-server goal get/set RPC. A changed goal, lifecycle
timestamp or status causes visible refusal rather than an automatic overwrite.
After cancellation or dispatch failure the goal stays paused. Inspect both the
job and `HOME/goals/THREAD.json`, then explicitly restore an unchanged wait:

```sh
monohero unpark-goal --thread "$CODEX_THREAD_ID"
```

This command refuses queued/running jobs; cancel the wait first. Uncertain RPC
results retain a `parking`/`restoring` receipt for manual inspection. Never retry
such a lifecycle change blindly. Never use this flag to suppress useful goal
work or pause a user's goal without their instruction. Verify actual host
accounting/lifecycle behavior before enabling this mode in a project. The host
must keep the lifecycle timestamp stable while paused; otherwise restoration
refuses explicitly. The public API has no compare-and-set revision: simultaneous
manual goal edits during the get/set RPC window cannot be made atomic by
Monohero. Avoid editing a goal while explicitly changing its queue wait state.

## Existing command workflows: resume their owner on completion

Submit the COMPLETE executable workflow, then register its exact owner:

```sh
monohero submit --queue my-project-release --key UNIQUE-RELEASE-ID -- ./release.sh
monohero notify JOB_ID --thread "$CODEX_THREAD_ID"
```

Then end the turn. An independent observer blocks on completion without polling
and sends the same chat the actual terminal result. The notification supplies:

```sh
monohero acknowledge NOTIFICATION_JOB_ID --thread "$CODEX_THREAD_ID"
```

Execute it to confirm actual receipt, then inspect the saved evidence and report
the result. A notice grants NO release slot; new protected work must rejoin its
project queue. Observers never occupy the release queue they wait on.

## Verification and recovery

`monohero status --queue NAME` reads one snapshot. Private ID.log files contain
execution logs. HOME/chats/JOB_ID.json records message_queued, claimed and completed
separately, with message ID and timestamps. With monitoring enabled, an unclaimed
invitation can be deferred after its configured grace period; a notice that is
never acknowledged remains pending. Verify a
real idle-chat wakeup when installing on a new Codex host. Never label an accepted
CLI message as confirmed resumption. Unsupported hosts fail or remain visibly
unacknowledged; no silent replacement session or alternate transport is used.

Failed/interrupted jobs remain visible but do not stop their queue. Inspect
external effects, the owner's actual chat and surviving processes before any
follow-up. `monohero resolve JOB_ID` records inspection, never retries. New work
gets a new key and joins the tail. `monohero cancel JOB_ID` cancels queued jobs only. Never retry uncertain
message delivery blindly. Choose an explicit CLI with `--codex /absolute/path`.

## Claude Code and limits

Command workflow mode remains available to Claude Code. Automatic Claude chat
resumption is not implemented. Monohero serializes cooperating work on one local
host; commands outside it are not protected. Waiting/dispatch use no LLM; the
resumed destination chat consumes tokens for its authorized work. No hooks,
permission bypasses, new sessions or coordinator agents are installed implicitly.

## Inspect queues manually

Use `monohero queue` for a readable snapshot of all unfinished queues, or
`monohero queue --queue PROJECT-RESOURCE` for one project. It shows FIFO waiting
positions, running jobs, blocked jobs, worktrees and destination chat IDs.
`--all` includes history; `--json` returns structured selected jobs. It exits
after one read. Do not turn this command into a model polling loop.

## Native desktop wake

If this Codex desktop host leaves unloaded chats at `message_queued`, configure
its supported native URL opener once: on macOS, `monohero configure-wake
--opener /usr/bin/open`. The command explicitly enables opening/selecting the
existing destination chat after accepted delivery, without another message.
This can bring the app forward. There is no automatic retry, alternate session
or bypass of claim/finish. Verify actual destination claim on the installed
host; opening a URL is not evidence of task execution. Opener failure records
the already accepted message as a failed job without retrying it. Later jobs can proceed. Queued
workers using the updated CLI read the shared setting when they dispatch;
currently running workers remain unchanged.

## Managed grants and blocked chats (0.4+)

Use one project queue across its worktrees. The native dashboard is
`monohero dashboard`; closing its window leaves the macOS menu-bar icon and queue
service available. Queue policy can put blocked chats in an attention list or
hold them at the tail. Native monitoring reads metadata, without model calls.
Persisted history is labelled separately from an attached owning runtime;
unknown/unavailable status is not permission to release claimed work.

A managed invitation includes the exact `monohero run JOB_ID --thread CHAT_UUID
-- COMMAND ARGS` prefix. Run every protected shell, test, Git, review, merge and
deployment operation under that prefix. Wait for its children to finish. After
revocation, stale commands fail: stop and wait for a fresh invitation, rather
than bypassing the wrapper. The wrapper does not provide release authorization
or replace project checks.

Before asking the user for input that prevents progress, finish protected
children and yield with `monohero defer JOB_ID --thread CHAT_UUID --reason TEXT`.
This is also required for legacy claimed grants, which cannot be safely revoked
from an inactivity signal. If a protected command failed, inspect its effects
before yielding; automated revocation retains the grant until that inspection.
A resumed grant gets a new job identity at the tail. Never reuse the old claim.
