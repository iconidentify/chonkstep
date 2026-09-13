# Normal interactive agent sessions

Implemented and reviewed on 2026-09-12. Chonk Agents observes ordinary Codex,
Claude Code, and OpenCode sessions through provider-native metadata interfaces.
T3 Code has a separately configured server adapter. Use
[the installation guide](../examples/chonk-agents/README.md) for setup.

## Architecture

The [Agent Sessions plugin](../omarchy/plugins/chonkstep.agents/README.md) lives
in Omarchy's top bar and uses its theme and panel controls. Agent identity,
lifecycle, provider transport, persistence, and
navigation run outside the compositor. Adding a theme requires no provider
adapter changes. This integration does not modify the installed compositor.

The schema-2 session store distinguishes application, engine, native session,
turn, child/parent relationship, runtime incarnation, activity, turn outcome,
source health, and observation confidence. It rejects delayed retired turns,
stale incarnations, duplicate events, dead/reused PIDs, and oversized packets.
A native Stop can be tentative; a turn completion leaves an interactive session
idle. Process end and missing final evidence are distinct states.

No identity is inferred from shared directories or window titles. T3 identity
is environment/thread, independent of engine changes. A merge with provider
hooks requires an explicit native association; the current T3 stream supplies
none. The two sources therefore remain separate. Codex's notify-only ephemeral
title-generator threads are ignored: completion notifications update only an
attachment established by a native hook from the same process incarnation.

Native Codex/Claude hooks use a standalone Rust helper. It whitelists metadata,
validates the native ancestor process, and makes a bounded local enqueue. It
never writes a permission decision, prints provider content, or fails the agent.
Codex's previous legacy notifier is invoked independently of observer success.
Setup preserves other handlers, backs up changes, and rolls back partial writes.
New Codex definitions use the provider's own review/trust UI.
Codex can deliver the initial attachment hook at the first prompt, including
after resume; a ready screen alone does not establish an observed attachment.
Claude's qualified resume path reports immediately.

OpenCode's TUI companion reuses the host Solid runtime and observes current,
resumed, previously selected, and explicitly associated child session metadata.
It reads native message IDs/roles only; no message parts or model context enter
the adapter. Pending permissions/questions and transport events are coalesced.
One persistent Python bridge carries metadata to the broker. It observes broker
socket lifetime, so even an idle attachment replays after a broker restart.

T3 uses the pinned authenticated Effect RPC shell subscription. It validates
the environment/version, obtains an ephemeral websocket ticket, acknowledges
bounded metadata chunks, and reconciles sequence catch-up before declaring
live state. The explicit token needs read scope only. Redirects are disabled,
websocket/HTTP/queue sizes are bounded, and diagnostics omit credentials.

The broker keeps at most 32 native attachments and 32 legacy rows within a 64 KiB
snapshot. Local queues are bounded and preserve retirement before replacement;
capacity limits are surfaced as degraded coverage. Linux pidfds report process
exit without polling. A coalescing worker writes private metadata atomically.
After restart, saved activity is unknown until fresh evidence arrives.

Navigation uses Chonkstep's existing native Hyprland-compatible IPC. It verifies
native window PID, process start identity, compositor socket identity, and lock
state before focusing a literal live address. tmux choices verify the server,
client, pane PID, and TTY. OpenCode additionally verifies a private TUI socket,
source incarnation, known session ID, and correlated acknowledgement after the
actual route changes. Codex and Claude destinations identify the terminal or
tmux pane, without selecting a different conversation inside that CLI. T3
advertises **Open thread in browser**, derived from authenticated thread metadata
and the configured origin; browser page loading is not acknowledged. GTK actions use
workers, cancellation guards, stable keyed rows, and explicit retry state.

## Qualification and remaining limits

| Application | Implemented path | Recorded qualification |
| --- | --- | --- |
| Codex CLI 0.154.0 | Native lifecycle/tool/permission/interrupt/subagent hooks plus completion notify | Normal and successive prompts, shell tool, and resume retaining the native ID with a new process after the next prompt; two tmux clients discovered separately and the chosen Foot window/pane activated. Hidden title helper behavior verified in exact source. |
| Claude Code 2.1.259 | Native lifecycle, tools, permission/notification, elicitation, stop/failure, subagents | Normal interactive prompt, shell tool, and immediate resume attachment using existing account/permissions; process metadata and tentative stop observed. |
| OpenCode 1.18.29 | Native local TUI companion, selected/resumed session metadata and exact route socket | Plugin loaded in the installed CLI and completed a normal prompt with native turn ID; its private socket returned from the home screen to the existing QA session and acknowledged the route. Detailed evidence accompanies the installation report. |
| T3 Code 0.0.40 | Authenticated server shell subscription, explicit token, existing-thread browser route | Exact source contract plus real local HTTP/websocket protocol fixtures. T3 is not installed on this machine; an authenticated connection remains to be qualified when it is installed. |

CLI qualification does not certify the Codex desktop app, IDE extensions,
arbitrary terminal tabs, remote tmux, or remote OpenCode attach. OpenCode attach's
public TUI API does not expose SSE disconnect health. Setup currently refuses an
existing OpenCode `tui.jsonc`; its TUI plugin can be registered manually without
rewriting that file. Unsupported bindings offer no guessed exact-session action.

Claude has no generic hook for a user interrupt during streaming outside tool
execution. Its display is explicitly last-observed until fresh evidence arrives.
Permission hooks can be resolved by other hooks; they are not authoritative
approval queues. Native OpenCode/T3 snapshots provide stronger pending-state
coverage. Turn outcomes are never treated as a certification of generated work.

The code and adversarial tests cover ordering, source/broker reconnect, abrupt
process exit, capacity pressure, setup rollback, existing notifiers, malformed
input, FIFO/symlink rejection, private persistence, stale/locked navigation,
multiple tmux clients, and GTK cancellation/focus stability. Source and fixture
coverage is recorded separately from live provider checks. Full interactive
permission, compaction, and subagent scenarios are not universally certified.

## Performance

Production GPU rendering is the acceptance target, per the user's direction.
The installed RTX 3090 compositor binary is unchanged by this integration. The
preceding theme acceptance measured 60.2 fps across all theme variants; see
[modern themes](modern-themes.md) for that rendering work and its software cost.
The agent integration has no compositor-side provider code, polling, or token
stream. Tile rendering ignores broker revision changes when its actual visible
state is unchanged; the existing theme invalidation path still redraws colors.

The native helper's measured median enqueue cost is 1.86 ms (95th percentile 2.20 ms), including
shell startup. Its hard 100 ms deadline bounds exceptional stalls. Metadata
microbenchmarks exercise unchanged callbacks, transitions, blocked queues, and
idle transport; these are adapter costs, not provider/model latency or a new GPU
benchmark. See the local installation report for raw results and final test
counts. A 10.008-second installed idle check produced zero snapshot changes and
zero recorded CPU ticks in the broker, tile, and OpenCode bridge (10 ms tick
resolution). The final suites passed 79 Python/GTK, 12 native-helper, and
28 adapter tests. llvmpipe theme blur remains more expensive than classic rendering; that
previously measured software cost is documented and is not the acceptance gate.

## Evidence and source contracts

Local review/install artifacts are under
`/home/chrisk/src/chonkstep-engineering-artifacts/2026-09-12/agent-sessions/`.
They include native navigation evidence, provider event checks, hook/adapter
benchmarks, qualification notes, and rollback locations.

Primary references: [Codex hooks](https://learn.chatgpt.com/docs/hooks),
[Claude hooks](https://code.claude.com/docs/en/hooks),
[OpenCode/T3 pinned contracts](../examples/chonk-agents/adapters/README.md),
and the Codex 0.154
[temporary title request](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/tui/src/temporary_structured_request.rs).
