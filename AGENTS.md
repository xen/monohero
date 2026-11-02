# Monohero development

- Keep execution local and explicit; do not install services or edit other projects without a request.
- Queues are FIFO by committed submission order. Separate names are independent.
- Never add timed status polling to the scheduler or blocking wait.
- Submission is durable before acknowledgement. Do not retry failed jobs or replay callbacks automatically.
- Preserve the failure/interruption barrier and ownership lock inherited by protected children.
- Agent documentation must distinguish process waiting from conversation resumption. Do not promise zero model tokens from a host that polls tool sessions.
- Run cargo fmt --check, cargo clippy --all-targets -- -D warnings, cargo test --locked and cargo build --release --locked before publishing.
