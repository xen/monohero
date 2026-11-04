# Accumulate requests before a chat claims its turn

## Goal
For one queue and exact chat UUID, retain one FIFO position until the chat claims it. Subsequent submissions made before that claim add their individually identified instructions and worktree paths to the same grant. Submissions after claim receive the next FIFO position. Different queues and chats remain independent.

## Invariants
- Every acknowledged request is durable and individually deduplicated by queue/key. A reused key with different arguments fails explicitly.
- A database transaction serializes submission and claim: no request can be acknowledged into a grant after that grant's claim snapshot.
- The claim response returns all accumulated instructions and worktrees in submission order; the delivered invitation instructs the chat to read that response before work.
- Existing running jobs and legacy worker protocols remain unchanged. Never silently mutate previously submitted commands, cancel jobs, preempt work, or replay uncertain callbacks.
- Goal pause/restore retains its existing explicit authorization and identity checks. An incompatible parked-goal request must fail explicitly rather than overwrite the goal.
- No scheduler or blocking-wait polling. Archive, cancellation, deferral, and failure retain auditable constituent submissions.

## Verification
- Concurrent enqueues for one queue/chat return one job and preserve every unique request once; retry with changed content fails; other queue/chat stays separate.
- Requests submitted before dispatch, while dispatch is in progress and after message delivery but before claim are returned by claim. A simultaneous enqueue/claim is assigned exactly once to either the claimed job or a new one.
- Existing legacy jobs and existing wait/command wire messages remain usable; restart preserves requests and FIFO order. Test cancellation, deferral, same-thread cross-queue grants, and parked-goal behavior.
- A queued or delivered invitation never counts as a claim; new work after claim does not join already protected work.
