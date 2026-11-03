# Task: Monitor chats and manage queues

## Goal
A native GPUI Kit dashboard displays queues, chat state and reasons for waiting;
operators reorder jobs, cancel jobs, clear queues/history and remove queues.
A native monitor detects stopped/blocked/input-waiting chats without model calls
and releases safely revocable grants instead of blocking unrelated jobs.

## Context
- src/main.rs: SQLite persistence, Unix control protocol, per-queue scheduler.
- src/chat.rs: exact-thread dispatch, actual claim/finish, native application wake.
- src/goal.rs: public local Codex app-server RPC.
- tests/queue.rs and tests/chat.rs: durable FIFO and grant ownership boundaries.

## Current Behavior
Accepted messages can wait for a chat indefinitely. Failed jobs retain a barrier;
there is no persisted manual order, attention state or dashboard management.

## Target Shape
- Persist manual queue order, queue settings, observations and attention reasons.
- Per-queue blocked policy: attention list or held at the tail, selectable by user.
  A held tail entry is not dispatched repeatedly while the blocker remains.
- Poll chats in a native process using explicit read-only Codex RPC. Runtime
  flags are authoritative only on the connected owning host; notLoaded from a
  separate reader is not evidence that an active chat stopped. Unavailable
  status stays explicit. Retained turn/goal state is labelled separately.
- New managed grants require a fenced wrapper for protected commands. Deferral
  first forbids new commands, waits for inherited resource locks, then revokes
  the grant. Unclaimed grants can be revoked directly. Legacy claimed grants
  require owner yield; no unsafe inference from inactivity alone.
- Resuming creates a new job/grant at the tail, so a stale invitation cannot
  claim a different lease. Preserve original deduplication identity and audit.
- Dashboard uses the private Unix control socket. A macOS menu-bar icon opens
  the GPUI window; closing the window keeps the icon and service alive. UI
  shutdown is separate from queue-service shutdown. Refresh/polling is native
  machine work, never a model heartbeat.

## Guard Rails
Keep explicit approvals/review, daemon ownership locks and failure barriers.
No automatic permission approval, protected command interruption by timer,
force-release, hidden fallback, blind job retry or direct private app transport.
No currently running job is silently converted to managed mode. Upgrade the
live daemon only after a controlled drain; preserve all existing jobs and keys.

## Iterations
1. Persisted order/settings and fenced deferred/cancelled states.
2. Chat observations and configurable attention/tail policies.
3. Native GPUI dashboard and CLI controls over the same daemon API.
4. Integration tests, native window/tray checks, reviewed publication and controlled
   local upgrade; demonstrate a blocked chat allowing the next job to proceed.

## Verification
- Default FIFO; explicit reorder changes only eligible pending order atomically.
- Reorder cannot promote running, completed or another queue's jobs.
- Deferral waits for protected children and stale grants/commands are rejected.
- Both blocked policies avoid repeated dispatch while input remains missing.
- Unknown/stale/unreachable chat status never frees a claimed legacy job.
- UI supports filtering/selecting queues, per-queue policy, order editing,
  cancellation, clearing and deletion with current server state/error feedback.
- Private Unix-socket permissions remain enforced. Arbitrary job text is
  rendered as text and no shell command or log secrets are exposed by default.
- Closing and reopening the GPUI window through the macOS tray preserves jobs.
- Each queue independently selects attention-list or held-tail policy, as
  explicitly requested by the user.
- Restart preserves queue order, held/deferred work, settings and dedup keys.
- fmt, clippy, full tests and release build; actual macOS window/tray acceptance.

UI framework: gpui-kit.com / gpui-kit 0.9.0, explicitly selected by the user.
