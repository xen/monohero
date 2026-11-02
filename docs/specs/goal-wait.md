# Explicit goal waiting

Provide opt-in `enqueue --park-goal`. The user authorizes pausing this exact
existing active goal until its queued chat grant. Save its identity, objective,
budget and lifecycle timestamp before changing state; preserve usage accounting.
Use public app-server goal get/set RPC only, never start/resume a model session.
Queue the native grant before restoring active, so goal continuation cannot race
ahead of its grant. Changed, absent or explicitly stopped goals must not be
resumed. Failed dispatch and cancelled jobs stay visibly paused until explicit
`unpark-goal`; no automatic retry, timeout release or replacement goal.

Without the flag all current behavior is unchanged. Independent work can keep
its goal active. Test unchanged goal restoration, user edits, cancellation/manual
recovery, idempotent submission, RPC errors and ordinary no-goal handoff.
Do not enable on live goals until the user selects the waiting policy.
