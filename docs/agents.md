# Codex and Claude Code

Use [AGENT.md](../integrations/AGENT.md) for feedable project instructions.
Codex chat mode uses enqueue → native message → claim → work → finish.
Existing command jobs use notify → native message → acknowledge.

## Transport and acceptance

The selected CLI executes `codex queue --thread UUID --message TEXT`. The host
consumes the queued message and resumes the existing chat. No model settings or
permissions are overridden. No separate agent or conversation is created.

An exit-zero transport receipt only means message_queued. An explicit claim from
the destination confirms resumption and retains ownership until finish. Notices
require acknowledge and do not grant protected release ownership. Receipt JSON
files record these phases separately. Unacknowledged delivery has no automatic
expiry or blind retry.

A real idle Codex desktop chat was used for acceptance during development: the CLI
message started a new turn, which executed the requested diagnostic command and
returned its confirmation. Test actual queued-message consumption on every new
host. Do not assume a CLI receipt guarantees that all Codex versions/hosts wake.

## Why not exec resume or private desktop sockets

An open desktop chat may retain its writer while idle. `codex exec resume` then
fails with an active-writer error; starting another engine is not this handoff.
The desktop app's private tools pipe is not a standalone external API. Monohero
uses neither of these as a fallback, and does not bypass peer authorization or
inject GUI keystrokes. The existing host owns and continues its own conversation.

## Compatibility

New client commands submit ordinary internal Rust workers through the stable
command/wait protocol. They also work with a running 0.1.1 daemon. No active-job
restart or database migration is required. Use a stable versioned binary path.

All worktrees of a project share queue/home, with individual --cwd and job keys.
Observer queues never occupy the queue whose completion they await. Claimed chat
slots remain occupied until explicit finish, including corrections and browser
verification. Do no protected work after releasing a slot.

Claude command workflow mode remains available. Automatic Claude resumption is
not implemented; never silently replace it with a new session.

Sources: the installed `codex queue --help` and the official
[queue command implementation](https://github.com/openai/codex/blob/main/codex-rs/tui/src/session_queue_commands.rs).
