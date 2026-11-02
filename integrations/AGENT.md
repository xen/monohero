# Integrate Monohero into Codex or Claude Code

Preserve project instructions, release approvals and checks. Choose one shared
queue name per exclusive resource; all worktrees use the same Monohero home.
Separate projects can use separate queues. Sending to a specific chat requires
the human owner's authorization. Install Monohero and run one explicitly
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

On grant, Monohero sends a user-visible message to that exact chat through
`codex queue`. The host resumes its existing conversation and permission/model
settings; no extra CLI agent or private desktop API is started. Delivery acceptance
alone is NOT proof the chat resumed. The actual destination must first execute:

```sh
monohero claim JOB_ID --thread "$CODEX_THREAD_ID"
```

The grant prompt includes the exact executable and arguments. If claim fails,
STOP without performing protected work. After claim, hold the slot through all
protected sync/check/review/merge/deploy/verification work, including corrections.
After ALL authorized work is complete, execute:

```sh
monohero finish JOB_ID --thread "$CODEX_THREAD_ID"
```

On failure/blocker append `--failed` and explain. Do not finish merely to
acknowledge the grant. Do not enqueue recursively into your own queue. Do no
protected work after finish; only the final user report remains. The worker
holds the slot until explicit matching finish. No timer expires it behind the
chat's back. Monohero serializes its grants to the same thread across queues.

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
separately, with message ID and timestamps. A chat that never claims its grant
keeps the slot; a notice that is never acknowledged remains pending. Verify a
real idle-chat wakeup when installing on a new Codex host. Never label an accepted
CLI message as confirmed resumption. Unsupported hosts fail or remain visibly
unacknowledged; no silent replacement session or alternate transport is used.

Failed/interrupted jobs stop their queue. Inspect external effects, the owner's
actual chat and surviving processes before `monohero resolve JOB_ID`. Resolve
acknowledges inspection, never retries. New work gets a new key and joins the
tail. `monohero cancel JOB_ID` cancels queued jobs only. Never retry uncertain
message delivery blindly. Choose an explicit CLI with `--codex /absolute/path`.

## Claude Code and limits

Command workflow mode remains available to Claude Code. Automatic Claude chat
resumption is not implemented. Monohero serializes cooperating work on one local
host; commands outside it are not protected. Waiting/dispatch use no LLM; the
resumed destination chat consumes tokens for its authorized work. No hooks,
permission bypasses, new sessions or coordinator agents are installed implicitly.
