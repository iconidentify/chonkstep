# Chonk Agents

A local session service for Chonkstep. The
[Agent Sessions plugin](../../omarchy/plugins/chonkstep.agents/README.md) places
the session list and navigation in Omarchy's menu bar. The broker and provider
adapters run outside the compositor. The dock frontend has been retired.

Start Codex, Claude Code, or OpenCode normally after one-time setup. Their
native hooks or TUI plugin report session activity. Authentication, prompts,
history, permission decisions, and model selection stay in the provider.

## Install and enable

Runtime dependencies: Python 3.11+, PyGObject, GTK 4, and the provider's
own CLI. Building also requires Cargo and npm. T3 requires Node.js 22+; the build
installs its pinned websocket dependency with lifecycle scripts disabled.

```sh
bash examples/chonk-agents/install.sh
python3 ~/.local/share/chonkstep/dockapps/chonk-agents/chonk-agents.py setup
bash omarchy/plugins/chonkstep.agents/install.sh
```

Start the installed broker in a terminal in the graphical session:

```sh
python3 ~/.local/share/chonkstep/dockapps/chonk-agents/chonk-agents.py serve
```

For subsequent sessions, append its absolute command to your existing Chonkstep
`autostart` array, preserving other entries:

```toml
autostart = [["python3", "/home/me/.local/share/chonkstep/dockapps/chonk-agents/chonk-agents.py", "serve"]]
```

The broker needs the session's display and desktop IPC environment. The
installer retains the existing service path for compatibility with provider
hooks. Upgrading the service does not require a compositor restart. Restart
the broker when ready; connected native adapters reconcile after reconnecting.

Create a convenience link if one does not already exist:

```sh
ln -s ~/.local/share/chonkstep/dockapps/chonk-agents/chonk-agents.py ~/.local/bin/chonk-agents
chonk-agents doctor
```

`setup [codex claude opencode]` is repeatable, creates backups, and preserves
unrelated hooks and configuration. Codex's existing completion notifier is
chained. `uninstall [providers...]` removes only managed adapter configuration
and restores that notifier when it is still managed. These commands do not
remove provider sessions or authentication.

Start or resume a new CLI instance to load changes. Codex can ask you to review
new hook definitions; use its normal review screen or `/hooks`. Setup does not
manufacture hook trust. Codex can first report its attachment when you submit a
prompt, including after resume; the ready screen alone is not an attachment
event. Claude reports its resumed session immediately. Existing OpenCode `tui.jsonc` configuration currently
requires manual plugin registration; setup refuses to overwrite it. See the
[adapter module example](adapters/README.md#opencode-11829).

## Sessions and navigation

Click **Agent Sessions** in the Omarchy bar. Its badge counts connected sessions;
the panel shows working and waiting counts and retains idle/history rows. Each row separates the
latest observed activity from source health. A completed turn becomes idle; closing an interactive
session is a separate event. Abrupt process/adapter exit becomes disconnected
with unknown activity. Saved metadata never restores a fabricated working state.

**Open terminal** resolves the actual native Wayland window and, for tmux,
offers explicit attached-client choices and selects the verified pane.
**Open session** additionally selects the retained native OpenCode session and
waits for its route acknowledgement. Destinations are revalidated when clicked.
Codex and Claude actions open their verified terminal or tmux pane; they do not
select a different conversation inside the CLI.
Shared process IDs, missing terminals, stale sockets, and ambiguous window
bindings do not produce guessed navigation. Terminal tabs and remote hosts
without a verified binding remain outside this qualification.

Claude Stop is tentative because another hook may continue work. A permission
hook means a request was observed; respond in the provider. Claude exposes no
generic interrupt hook for cancelling a streaming model response outside a tool.
That status remains last-observed until another hook or process exit arrives.
There are no approval or arbitrary-command RPCs in the broker.

The sessions window also offers a read-only tracked diff against HEAD. It can
include changes that predate the agent, omits untracked files, and caps output
at 128 KiB. External diff, text conversion, clean/process filters, and filesystem
monitor helpers are disabled. Work runs off the UI thread with a timeout.

## T3 Code

The server adapter is pinned to T3 Code **0.0.40**. Give it an explicit private
access-token file with `orchestration:read`; credentials are not discovered from
another application's files. The URL is the browser/server origin:

```sh
chonk-agents connect t3 --url http://127.0.0.1:3773 --token-file /absolute/private/t3-token
chonk-agents doctor
```

Remote origins require HTTPS. The adapter subscribes to authenticated thread
metadata, reconnects with sequence reconciliation, and reports disconnected
state when transport is lost. **Open thread in browser** asks the browser to open
the thread route derived from authenticated metadata and the configured origin;
it does not confirm that the browser loaded the page. Native T3 desktop thread
activation is not advertised.
T3 does not expose a native provider session association in this contract, so a
T3 thread and independently observed CLI thread remain distinct.

T3 is not installed on the development machine; the adapter is ready for a future
explicit connection. Protocol tests exercise a real local HTTP/websocket fixture;
an authenticated deployment still needs
connection qualification. Other T3 server versions are refused until reviewed.
See [the adapter contracts and source references](adapters/README.md).

## Older connected-run interface

The explicit one-shot wrappers remain available:

```sh
chonk-agents run codex 'Review this repository'
chonk-agents run claude 'Explain the test failures'
chonk-agents run opencode 'Review this change'
chonk-agents run grok 'Review this change' --model xai/YOUR_MODEL_ID
```

These make real provider requests using existing permissions/authentication.
Grok uses OpenCode's xAI provider. Choose an available model in your account;
there is no separately qualified native Grok CLI adapter. Prefer normal
interactive sessions for lifecycle recovery and session navigation. Legacy run
rows retain their original last-reported status semantics.

## Protocol and validation

The private same-UID `SOCK_SEQPACKET` endpoint is
`$XDG_RUNTIME_DIR/chonk-agents/events.sock`. Schema 2 carries bounded native
session/turn identity, semantic activity, parent identity, source incarnation,
capabilities, and verified process metadata. It excludes prompts, tool arguments,
results, transcript paths, and credentials. At most 32 native session attachments
and 32 legacy run rows are retained; every combined snapshot is limited to 64 KiB.

`session` submits schema-2 metadata; `snapshot` reads it; `subscribe` receives
changes; `watch` monitors connection lifetime. Native helpers fail open with a
100 ms hard deadline and neutral provider output (`{}` for Codex Stop/SubagentStop,
otherwise silent). OpenCode uses one persistent bridge process,
coalesces unchanged metadata, and replays after a broker restart. The Omarchy plugin updates
from bounded session snapshots.
No per-token compositor work or model calls are introduced.

```sh
python3 -m unittest discover -s examples/chonk-agents/tests -q
CHONK_AGENTS_GTK_TEST=1 python3 -m unittest discover -s examples/chonk-agents/tests -p 'test_gtk_*.py' -q
cargo test --manifest-path examples/chonk-agents/hook-helper/Cargo.toml --locked
npm test --prefix examples/chonk-agents/adapters
```

The optional GTK checks use private headless Weston. Full architecture,
qualification, performance evidence, and limits are recorded in
[agent sessions](../../docs/agent-sessions.md).
