# Native session adapters

These adapters observe native session metadata outside the compositor. They never
submit prompts, approve permissions, read transcripts or inspect provider auth
configuration. `node` 22+ is required for the standalone T3 adapter; this machine
has Node 26.2.0 through mise, with no `/usr/bin/node`.

## OpenCode 1.18.29

The normal interactive adapter is the **TUI companion**. It observes the actual
selected/resumed session through OpenCode's host Solid state, retaining up to 32
sessions selected in that TUI and explicitly created child sessions whose native
parent is already attached, so their background activity remains visible. A
new selection retires an idle former attachment with an explicit disconnect;
capacity overflow is visibly degraded. When all 32 retained sessions are active,
further sessions are not observed until a slot becomes eligible.
Startup history is not reported as attached. No timer polls TUI state. Unchanged
semantic state sends no event, and token/tool payloads are not serialized. Native
session errors remain visible through idle until the next activity boundary.
User-turn IDs come from `message.updated` metadata (`role`, `id`, `sessionID`)
and, on resume, at most 128 cached message metadata records; text/parts are never
read. A native abort is an interrupted turn, not a terminated session. Cache
absence reports unknown; only a native deletion establishes session end.

This qualification covers local interactive TUI processes. Remote `opencode
attach` is unqualified: the public TUI API's `ready` flag records bootstrap, and
its silently reconnecting SSE transport exposes no connection-health hook. It
must not be interpreted as proof that a remote provider remains connected.

The installer should create a module outside the server's auto-scanned plugin
directory and add its absolute file URL to the native `tui.json` `plugin` list:

```js
import { createTuiPlugin } from "/absolute/installed/adapters/opencode-tui.mjs";
export default {
  id: "chonk-agents",
  tui: createTuiPlugin({
    bridgeCommand: ["/resolved/python3", "/absolute/installed/chonk-agents.py", "stream", "opencode"],
  }),
};
```

OpenCode installs its host module resolver before loading TUI plugins, so the
dynamic `solid-js` import uses the application's existing reactive runtime. Do
not bundle another copy of Solid. Changes require a new TUI instance.

Each attachment owns a private Unix activation socket under
`$XDG_RUNTIME_DIR/chonk-agents/opencode-PID-UUID.sock`. The directory is owned by
the user with mode 0700; the socket is 0600. UUID equals the emitted incarnation.
Up to four peers may send one bounded newline-delimited request:

```json
{"action":"select_session","session_id":"ses_...","request_id":"request-uuid","incarnation":"attachment-uuid"}
```

Only a retained native session can be selected. A successful response echoes
`request_id` and `session_id` with `ok:true` after the reactive route matches.
There is a two-second deadline. The broker must independently verify socket peer
PID and process start identity before using this advertised binding. Observation
still works if socket setup fails, with `open_session` omitted.

`opencode.mjs` also exports `createPlugin({bridgeCommand})`, a server-plugin
alternative for serve/headless contexts. It uses native event IDs, status,
permission/question events, bounded startup reads and the new `dispose` hook.
Its injected legacy SDK lacks pending-request list methods; the isolated
`readPending` helper uses the verified v1.18.29 `_client.get` transport, preserving
its authenticated/in-process request routing without inspecting credentials.
Unsupported startup snapshots report degraded coverage. The server plugin cannot
identify the selected terminal session and never advertises exact TUI navigation.
Do not install both observers for the same normal TUI attachment: use the TUI
companion by default and the server alternative for separately qualified hosts.

One Python bridge child is started when metadata first appears. The bridge keeps
at most 32 latest states plus 32 bounded retirement transitions, observes pipe
backpressure and retries a failed child
with exponential backoff capped at 30 seconds. No retry timer runs with a healthy
child. Draining a blocked pipe immediately retries deferred observations; no later
session event is required. The bridge inherits only display/terminal/runtime variables, not provider
API keys. Disposal ends the pipe and bounds child shutdown to 100 ms.

Source qualification: OpenCode tag `v1.18.29`, commit
`16747470f976aca3d362ad730bcd3fe82ecc2c9a`, especially
[`plugin/src/tui.ts`](https://github.com/anomalyco/opencode/blob/16747470f976aca3d362ad730bcd3fe82ecc2c9a/packages/plugin/src/tui.ts),
[`plugin/index.ts`](https://github.com/anomalyco/opencode/blob/16747470f976aca3d362ad730bcd3fe82ecc2c9a/packages/opencode/src/plugin/index.ts)
and the host's
[`plugin/tui/runtime.ts`](https://github.com/anomalyco/opencode/blob/16747470f976aca3d362ad730bcd3fe82ecc2c9a/packages/opencode/src/plugin/tui/runtime.ts).
The locally cached plugin/SDK package 1.4.3 is not the version reference.

## T3 Code 0.0.40

Install the pinned, lockfile-verified pure-JS transport dependency:

```sh
npm ci --omit=optional --ignore-scripts --no-audit --no-fund
```

Run `node /absolute/installed/adapters/t3.mjs` under the main broker's supervisor.
It emits schema-2 bounded NDJSON on stdout; stderr contains generic diagnostics.
Configuration is explicit:

- `CHONK_T3_URL`: server origin, without path, query, fragment or userinfo.
  Remote origins require HTTPS; HTTP is accepted only for loopback.
- `CHONK_T3_ACCESS_TOKEN`: explicitly provided T3 access token, or
  `CHONK_T3_TOKEN_FILE`: explicitly provided owned regular file with no group/other
  permissions. The adapter does not discover or scrape existing credentials.

The server token needs `orchestration:read`. The adapter checks the public
environment descriptor/version, obtains a short-lived websocket ticket using
the bearer header, and connects to `/ws?wsTicket=...`. Tickets/tokens are never
included in output rows or errors. HTTP redirects and websocket redirects are
disabled. Websocket compression is disabled, frames are capped at 4 MiB before
assembly, fragment counts are bounded, and HTTP bodies are bounded too.

The Effect JSON RPC stream is `orchestration.subscribeShell`. Chunk ACKs are sent
after whitelisting; reconnect uses `afterSequence` and an explicit `synchronized`
marker. A disconnect makes every retained row uncertain immediately. No stale
working state is advertised as connected before catch-up completes. A lightweight
30-second transport keepalive detects half-open connections; it neither queries
thread state nor causes a compositor update. Retry backoff is capped at 30 seconds.

At most 32 thread rows and 128 project paths are retained. Oversized catalogs
report degraded coverage; active/waiting threads are preferred in a snapshot and
can replace idle rows during a live update. One whitelisted active replacement
may wait for a saturated retirement queue to drain. The metadata pipe reserves
32 current snapshots and 32 ordered retirement transitions, each at most 8 KiB.
A full resnapshot may stage at most 32 additional whitelisted desired rows while
old retirements drain; repeated resnapshots replace that staging table. Turn
outcome is reported separately from the persistent thread's activity.
The public stream does not expose native provider session IDs. Rows therefore
retain T3 environment/thread identity and do **not** merge with provider hooks.
Exact navigation is advertised as **Open thread in browser**; the broker derives
and validates `/<encoded environment>/<encoded thread>` against the configured
browser origin. No URL supplied by an event should become an arbitrary launcher.
The existing desktop activation protocol only opens workspaces and is not used
as an existing-thread action.

Wire/source qualification is pinned to T3 commit
`cfeaca41ae27bdf2c203158d378c87c7308fea2a`, server/desktop package 0.0.40:
[`orchestration.ts`](https://github.com/pingdotgg/t3code/blob/cfeaca41ae27bdf2c203158d378c87c7308fea2a/packages/contracts/src/orchestration.ts),
[`auth.ts`](https://github.com/pingdotgg/t3code/blob/cfeaca41ae27bdf2c203158d378c87c7308fea2a/packages/contracts/src/auth.ts),
[`desktopAppActivation.ts`](https://github.com/pingdotgg/t3code/blob/cfeaca41ae27bdf2c203158d378c87c7308fea2a/packages/contracts/src/desktopAppActivation.ts).
Other server versions fail closed until their contract is qualified. An endpoint
changing its native environment identity requires an adapter restart; this keeps
blocked retirements associated with their original owner until they drain.

## Evidence and limits

`node benchmark.mjs` measures five 100,000-callback samples for ignored traffic,
unchanged metadata, status transitions and blocked queues, plus a two-second idle
interval. These measurements do not establish native provider or compositor cost.

`npm test` runs local state, bounded-queue, native Unix navigation and real local
HTTP/websocket protocol fixtures. These tests use synthetic credentials and
metadata; they make no provider/model requests. They establish adapter behavior,
not live-provider acceptance. T3 was not found installed on this machine during
this review. Normal live sessions, all provider lifecycle scenarios and a user's
authenticated T3 deployment require separate recorded qualification before any
claim of universal application support.
