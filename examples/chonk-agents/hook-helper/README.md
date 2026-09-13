# Native hook emitter

Build with `cargo build --release --offline --manifest-path Cargo.toml`.
This independent workspace produces `target/release/chonk-agent-hook`.

`chonk-agent-hook codex` and `chonk-agent-hook claude` read one JSON hook
payload from stdin. `chonk-agent-hook codex-notify '<json>'` accepts Codex's
existing notification argument shape. Setup and broker integration live in the
parent agent example; this helper never modifies provider configuration.

The Linux helper enforces a 100 ms process deadline, reads at most 1 MiB, and
makes one nonblocking seqpacket connection/send without waiting for a response.
Malformed input, unavailable/full transport, missing native ancestor, or other
observation failures produce exit zero. Codex Stop/SubagentStop receive neutral
`{}` output; other supported invocations are silent. There is no diagnostic
output, transcript access, provider request, permission decision, or prompt
content in the emitted metadata.

The schema-2 event contains native session/turn IDs, optional subagent ID,
monotonic observation time, owning runtime PID and `/proc` start tick, project
path/name, generic tool name, and available terminal/tmux identifiers. Codex
uses `turn_id`; Claude uses its optional `prompt_id`. Runtime discovery checks
up to 16 same-UID ancestors using `/proc/exe`, skipping shell/mise wrappers.
No T3 association is guessed. Terminal identifiers still require broker-side
validation before activation.

`Stop` means `turn.stopped` with `tentative: true`; another hook can continue
the turn. SubagentStop is likewise tentative. Only Codex's notify event maps to
`turn.complete`. SessionStart preserves `source`, including `compact`, so the
broker can retain activity during compaction. PermissionRequest/notifications
report `approval.requested`, not proof that a prompt remains pending. Claude
tool failure does not end the session. Its tool-interrupt indication is observed,
but its native hook API has no generic Interrupt event for model-streaming
interruptions. Delayed idle notifications are ignored. The helper reports
observations; the broker owns reconciliation, liveness, and ordering policy.

Run `cargo test --offline`. Tests cover metadata exclusion, lifecycle distinctions,
provider-specific IDs, malformed/oversized input, missing EOF, actual seqpacket
delivery without a reply, and native ancestor identification using harmless local
shell fixtures. They do not contact a provider or read conversation history.
