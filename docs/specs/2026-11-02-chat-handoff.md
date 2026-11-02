# Existing-chat handoff

## Goal
A queued chat resumes by exact UUID when granted its slot. It explicitly claims,
works and finishes. Existing command jobs can notify and resume their owners.

## Design
Keep the command/wait wire protocol. Enqueue submits a Rust chat worker as an
ordinary job, including on a running 0.1.1 daemon. On grant it opens a private
control socket and invokes `codex queue --thread UUID --message TEXT`. The host
resumes its own conversation. Separate message acceptance from actual claim.
Finish requires a matching prior claim. Notices run in independent observer
queues, block on command completion and wait for destination acknowledge.
Serialize grants to the same thread across queues. Persist timestamped receipts.

## Boundaries
Human authorization per destination. No new chats, extra CLI agents, private
socket access, GUI keystrokes, permission/model overrides, polling or silent
retries. Do not terminate the running service. Open desktop threads retain their
writer: exec resume is not an interchangeable transport. Verify real idle-chat
wakeup on the selected host. No automatic Claude resumption in this increment.

## Verification
Retain command-mode tests. Add black-box transport stub tests for FIFO, claims,
matching owner, exact destination, missing claim, delivery errors, notices after
success/failure and cross-queue same-thread exclusion. Run fmt/clippy/tests/release.
Live acceptance must show a new turn in an authorized idle destination and actual
execution, not just an accepted CLI message.
