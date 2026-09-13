import test from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { OpenCodeSessions, readPending } from "../opencode.mjs";
import { MetadataWriter, cleanOrigin, localEnvironment } from "../common.mjs";
import { T3Sessions, consumeRpc, threadPhase } from "../t3.mjs";

const info = (id = "ses_one") => ({ id, projectID: "project", directory: "/project", title: "Session", parentID: "ses_parent" });
const thread = (id = "thread") => ({ id, projectId: "project", title: "Thread", worktreePath: null,
  session: { providerName: "codex", status: "ready", activeTurnId: null }, latestTurn: null,
  hasPendingApprovals: false, hasPendingUserInput: false });

test("OpenCode whitelist excludes prompts, tool arguments and token traffic", () => {
  const rows = [];
  const state = new OpenCodeSessions({ directory: "/project", projectID: "project", publish: row => rows.push(row) });
  state.event({ id: "created", type: "session.created", properties: { info: { ...info(), secret: "DO_NOT_FORWARD" } } });
  for (let i = 0; i < 10000; i++) state.event({ type: "message.part.updated", properties: { text: "DO_NOT_FORWARD" } });
  state.event({ id: "wait", type: "permission.asked", properties: { sessionID: "ses_one", id: "req", metadata: "DO_NOT_FORWARD" } });
  assert.equal(rows.length, 2);
  assert.equal(rows[1].phase, "waiting");
  assert.equal(rows[0].parent_session_id, "ses_parent");
  assert.ok(!JSON.stringify(rows).includes("DO_NOT_FORWARD"));
});

test("OpenCode permissions, duplicate events and idle are distinct from session end", () => {
  const rows = [];
  const state = new OpenCodeSessions({ directory: "/project", publish: row => rows.push(row) });
  state.event({ type: "session.created", properties: { info: info() } });
  const waiting = { id: "a", type: "permission.asked", properties: { sessionID: "ses_one", id: "req" } };
  state.event(waiting); state.event(waiting);
  state.event({ id: "b", type: "permission.replied", properties: { sessionID: "ses_one", requestID: "req", reply: "once" } });
  state.event({ id: "c", type: "session.status", properties: { sessionID: "ses_one", status: { type: "idle" } } });
  assert.equal(rows.at(-1).phase, "idle");
  assert.equal(rows.at(-1).kind, "session.snapshot");
  state.event({ id: "d", type: "session.deleted", properties: { info: info() } });
  assert.equal(rows.at(-1).kind, "session.end");
  assert.equal(rows.at(-1).phase, "ended");
});

test("OpenCode startup history is not claimed attached; stale snapshots lose to native events", () => {
  const rows = [];
  const state = new OpenCodeSessions({ directory: "/project", publish: row => rows.push(row) });
  state.snapshot(info(), undefined);
  assert.equal(rows.length, 0);
  state.event({ type: "session.status", properties: { sessionID: "ses_one", status: { type: "busy" } } });
  state.snapshot(info(), { type: "idle" }, [], [], { expectedSequence: 0, attached: true });
  assert.equal(rows.at(-1).phase, "working");
  assert.equal(state.rows.get("ses_one").attached, false);
});

test("Unchanged TUI snapshots do not enqueue or advance visible state", () => {
  const rows = [];
  const state = new OpenCodeSessions({ directory: "/project", publish: row => rows.push(row) });
  for (let i = 0; i < 10000; i++) state.snapshot(info(), { type: "idle" }, [], [], { attached: true });
  assert.equal(rows.length, 1);
  state.disconnect(); assert.equal(rows.at(-1).phase, "unknown");
  assert.equal(rows.at(-1).source_health, "disconnected");
});

test("OpenCode pending snapshot uses the injected transport and detects unsupported coverage", async () => {
  let request;
  const rows = await readPending({ _client: { get: async value => { request = value; return { data: [{ id: "req" }] }; } } }, "permission");
  assert.equal(request.url, "/permission"); assert.equal(rows[0].id, "req");
  await assert.rejects(readPending({}, "permission"));
});

test("Slow consumer retains only bounded latest semantic lines", () => {
  class Sink extends EventEmitter { writes = []; write(line) { this.writes.push(line); return false; } }
  const sink = new Sink(); const writer = new MetadataWriter(sink);
  for (let i = 0; i < 10000; i++) writer.push({ engine: "codex", environment: "env", session_id: `session${i % 32}`, sequence: i });
  assert.equal(sink.writes.length, 1); assert.equal(writer.pending.size, 32);
  assert.equal(writer.push({ engine: "codex", environment: "env", session_id: "overflow" }), false);
  assert.ok([...writer.pending.values()].every(line => Buffer.byteLength(line.line) <= 8192));
  writer.close(); assert.equal(sink.listenerCount("drain"), 0);
});

test("Only explicit safe environment is inherited; credential-bearing origins are rejected", () => {
  assert.deepEqual(localEnvironment({ PATH: "/bin", XDG_RUNTIME_DIR: "/run/user/1", ANTHROPIC_API_KEY: "secret", OPENAI_API_KEY: "secret" }),
    { PATH: "/bin", XDG_RUNTIME_DIR: "/run/user/1" });
  for (const url of ["http://host.example", "https://user:secret@host.example", "https://host.example/?token=secret", "file:///tmp/a"])
    assert.throws(() => cleanOrigin(url));
  assert.equal(cleanOrigin("http://127.0.0.1:1234"), "http://127.0.0.1:1234");
});

test("T3 snapshot waits for synchronization, whitelists metadata, and does not infer provider IDs", () => {
  const rows = []; const state = new T3Sessions({ environment: "env", publish: row => rows.push(row) });
  state.item({ kind: "snapshot", snapshot: { snapshotSequence: 8, projects: [{ id: "project", workspaceRoot: "/project", scripts: ["DO_NOT_FORWARD"] }],
    threads: [{ ...thread(), messages: [{ text: "DO_NOT_FORWARD" }], session: { ...thread().session, resumeCursor: "DO_NOT_FORWARD" } }] } });
  assert.equal(rows.length, 0);
  state.item({ kind: "synchronized" });
  assert.equal(rows[0].session_id, "thread"); assert.equal(rows[0].environment, "env");
  assert.ok(!JSON.stringify(rows).includes("DO_NOT_FORWARD"));
  assert.ok(!("navigation" in rows[0])); assert.equal(rows[0].capabilities.open_browser, true);
});

test("T3 native sequence rejects duplicates and delayed updates; reconnect is visibly uncertain", () => {
  const rows = []; const state = new T3Sessions({ environment: "env", publish: row => rows.push(row) });
  state.item({ kind: "thread-upserted", sequence: 10, thread: thread() }); state.item({ kind: "synchronized" });
  state.item({ kind: "thread-upserted", sequence: 11, thread: { ...thread(), hasPendingApprovals: true } });
  state.item({ kind: "thread-upserted", sequence: 10, thread: thread() });
  assert.equal(rows.at(-1).phase, "waiting"); assert.equal(state.cursor, 11);
  state.disconnect(); assert.equal(rows.at(-1).source_health, "disconnected");
  state.item({ kind: "synchronized" }); assert.equal(rows.at(-1).phase, "waiting");
});

test("T3 Effect JSON wire uses chunk acknowledgements and rejects terminal/unknown frames", () => {
  const sent = []; const state = new T3Sessions({ environment: "env", publish: () => {} });
  consumeRpc(JSON.stringify([{ _tag: "Chunk", requestId: "shell", values: [{ kind: "synchronized" }] }, { _tag: "Pong" }]),
    { sessions: state, send: row => sent.push(row) });
  assert.deepEqual(sent, [{ _tag: "Ack", requestId: "shell" }]);
  assert.throws(() => consumeRpc('{"_tag":"Exit","requestId":"shell","exit":{"_tag":"Failure"}}', { sessions: state, send: () => {} }));
  assert.throws(() => consumeRpc('[]'.repeat(3 * 1024 * 1024), { sessions: state, send: () => {} }));
});

test("T3 turn outcome does not claim the interactive session ended", () => {
  assert.equal(threadPhase({ ...thread(), latestTurn: { state: "completed" } }), "idle");
  assert.equal(threadPhase({ ...thread(), session: { status: "interrupted" } }), "idle");
  assert.equal(threadPhase({ ...thread(), backgroundLiveness: "working" }), "working");
});


test("T3 relabel at full backpressure capacity delivers the latest provider without another event", () => {
  class Sink extends EventEmitter { writes = []; write(line) { this.writes.push(JSON.parse(line)); return false; } }
  const sink = new Sink(); let state;
  const writer = new MetadataWriter(sink, { onReady: () => state.retryPending() });
  state = new T3Sessions({ environment: "env", publish: row => writer.push(row) });
  state.project({ id: "project", workspaceRoot: "/project" });
  state.item({ kind: "synchronized" });
  for (let i = 0; i < 32; i++) state.thread(thread(`t${i}`));
  state.thread({ ...thread("t0"), session: { providerName: "claude", status: "ready" } });
  for (let i = 0; writer.pending.size && i < 100; i++) sink.emit("drain");
  assert.equal(writer.pending.size, 0);
  assert.equal(sink.writes.filter(row => row.session_id === "t0").at(-1).engine, "claude");
  writer.close();
});

test("T3 live active row retires an idle slot in delivery order and marks omitted coverage", () => {
  class Sink extends EventEmitter { writes = []; write(line) { this.writes.push(JSON.parse(line)); return false; } }
  const sink = new Sink(); const writer = new MetadataWriter(sink);
  const state = new T3Sessions({ environment: "env", publish: row => writer.push(row) });
  state.project({ id: "project", workspaceRoot: "/project" }); state.item({ kind: "synchronized" });
  for (let i = 0; i < 32; i++) state.thread(thread(`t${i}`));
  state.thread({ ...thread("active33"), hasPendingApprovals: true });
  assert.equal(state.rows.size, 32); assert.equal(state.rows.get("active33").phase, "waiting");
  for (let i = 0; writer.pending.size && i < 100; i++) sink.emit("drain");
  const retired = sink.writes.findIndex(row => row.session_id === "t0" && row.kind === "session.disconnect");
  const replacement = sink.writes.findIndex(row => row.session_id === "active33");
  assert.ok(retired >= 0 && replacement > retired);
  assert.equal(sink.writes[replacement].source_health, "degraded");
  writer.close();
});

test("TUI selection 33 retires an idle attachment; all-active overflow is explicitly degraded", () => {
  const rows = []; const state = new OpenCodeSessions({ directory: "/project", publish: row => rows.push(row) });
  for (let i = 0; i < 33; i++) { state.selectedId = `s${i}`; state.snapshot(info(`s${i}`), { type: "idle" }, [], [], { attached: true }); }
  assert.equal(state.rows.size, 32); assert.ok(state.rows.has("s32"));
  assert.ok(rows.find(row => row.session_id === "s0" && row.kind === "session.disconnect"));
  assert.equal(rows.at(-1).source_health, "degraded");
  for (const row of state.rows.values()) row.phase = "working";
  state.selectedId = "overloaded"; state.snapshot(info("overloaded"), { type: "busy" }, [], [], { attached: true });
  assert.equal(state.rows.size, 32); assert.ok(rows.at(-1).source_health === "degraded");
});

test("OpenCode error outcome survives idle snapshots and clears only with next activity", () => {
  const rows = []; const state = new OpenCodeSessions({ directory: "/project", publish: row => rows.push(row) });
  state.snapshot(info(), { type: "idle" }, [], [], { attached: true });
  state.event({ type: "session.error", properties: { sessionID: "ses_one", error: { message: "PRIVATE" } } });
  state.snapshot(info(), { type: "idle" }, [], [], { attached: true });
  assert.equal(rows.at(-1).phase, "error");
  state.event({ type: "session.status", properties: { sessionID: "ses_one", status: { type: "idle" } } });
  assert.equal(rows.at(-1).phase, "error");
  state.snapshot(info(), { type: "busy" }, [], [], { attached: true });
  assert.equal(rows.at(-1).phase, "working");
  assert.ok(!JSON.stringify(rows).includes("PRIVATE"));
});


test("An error delivered before native idle is not erased by the previous busy snapshot", () => {
  const rows = []; const state = new OpenCodeSessions({ directory: "/project", publish: row => rows.push(row) });
  state.snapshot(info(), { type: "busy" }, [], [], { attached: true });
  state.event({ type: "session.error", properties: { sessionID: "ses_one" } });
  state.snapshot(info(), { type: "busy" }, [], [], { attached: true });
  assert.equal(rows.at(-1).phase, "error");
  state.snapshot(info(), { type: "idle" }, [], [], { attached: true });
  state.snapshot(info(), { type: "busy" }, [], [], { attached: true });
  assert.equal(rows.at(-1).phase, "working");
});


test("T3 turn outcomes remain separate from persistent interactive phase", () => {
  const rows = []; const state = new T3Sessions({ environment: "env", publish: row => rows.push(row) });
  state.project({ id: "project", workspaceRoot: "/project" }); state.item({ kind: "synchronized" });
  for (const outcome of ["completed", "interrupted", "error"]) {
    state.thread({ ...thread(), latestTurn: { turnId: "turn", state: outcome } });
    assert.equal(rows.at(-1).phase, "idle"); assert.equal(rows.at(-1).turn_outcome, outcome);
    assert.equal(rows.at(-1).turn_id, "turn");
  }
});

test("OpenCode native turn metadata excludes assistant/prompt payloads and abort keeps session alive", () => {
  const rows = []; const state = new OpenCodeSessions({ directory: "/project", publish: row => rows.push(row) });
  state.snapshot(info(), { type: "busy" }, [], [], { attached: true });
  state.event({ type: "message.updated", properties: { info: { role: "user", sessionID: "ses_one", id: "turn-one", parts: ["PRIVATE"] } } });
  state.event({ type: "message.updated", properties: { info: { role: "assistant", sessionID: "ses_one", id: "assistant", text: "PRIVATE" } } });
  assert.equal(rows.at(-1).turn_id, "turn-one");
  state.event({ type: "session.error", properties: { sessionID: "ses_one", error: { name: "MessageAbortedError", message: "PRIVATE" } } });
  assert.equal(rows.at(-1).phase, "idle"); assert.equal(rows.at(-1).turn_outcome, "interrupted");
  assert.equal(rows.at(-1).kind, "session.snapshot"); assert.ok(!JSON.stringify(rows).includes("PRIVATE"));
});

test("Saturated T3 resnapshots and removals drain in broker admission order without later events", () => {
  const broker = new Map();
  class Sink extends EventEmitter {
    writes = []; violations = [];
    write(line) {
      const row = JSON.parse(line); this.writes.push(row);
      if (!broker.has(row.session_id) && broker.size >= 32) {
        const removable = [...broker].find(([, prior]) => prior.kind === "session.disconnect");
        if (!removable) this.violations.push("replacement preceded retirement");
        else broker.delete(removable[0]);
      }
      broker.set(row.session_id, row); return false;
    }
  }
  const sink = new Sink(); let state;
  const writer = new MetadataWriter(sink, { onReady: () => state.retryPending() });
  state = new T3Sessions({ environment: "env", publish: row => writer.push(row) });
  for (let sequence = 1; sequence <= 3; sequence++) {
    state.item({ kind: "snapshot", snapshot: { snapshotSequence: sequence, projects: [{ id: "project", workspaceRoot: "/project" }],
      threads: Array.from({ length: 32 }, (_, i) => thread(`${sequence}-${i}`)) } });
    state.item({ kind: "synchronized" });
    assert.ok(writer.pending.size <= 64); assert.ok(state.rows.size <= 32);
    assert.ok((state.pendingSnapshot?.size ?? 0) <= 32);
  }
  state.item({ kind: "thread-removed", sequence: 4, threadId: "3-31" });
  for (let i = 0; writer.pending.size && i < 400; i++) sink.emit("drain");
  assert.deepEqual(sink.violations, []);
  assert.equal(writer.pending.size, 0); assert.equal(state.pendingSnapshot, null);
  assert.equal(state.rows.size, 31); assert.ok([...state.rows.keys()].every(id => id.startsWith("3-")));
  for (let i = 0; i < 31; i++) assert.equal(broker.get(`3-${i}`).phase, "idle");
  assert.ok(!broker.has("3-31") || broker.get("3-31").kind === "session.disconnect");
  writer.close();
});


test("Reselection while retirement is pending cannot strand another admission at broker capacity", () => {
  const broker = new Map(Array.from({ length: 32 }, (_, i) => [`a${i}`, { kind: "session.snapshot" }]));
  class Sink extends EventEmitter {
    blocked = true; violations = [];
    write(line) {
      const row = JSON.parse(line);
      if (!broker.has(row.session_id) && broker.size >= 32) {
        const victim = [...broker].find(([, value]) => value.kind === "session.disconnect");
        if (!victim) this.violations.push(row.session_id); else broker.delete(victim[0]);
      }
      broker.set(row.session_id, row); return !this.blocked;
    }
  }
  const sink = new Sink(); const writer = new MetadataWriter(sink);
  const row = (session_id, kind = "session.snapshot") => ({ engine: "opencode", environment: "fixture", session_id, kind });
  writer.push(row("a31"));
  writer.push(row("a0", "session.disconnect")); writer.push(row("b"));
  writer.push(row("a1", "session.disconnect")); writer.push(row("a0"));
  sink.blocked = false; sink.emit("drain");
  assert.deepEqual(sink.violations, []); assert.equal(writer.pending.size, 0);
  assert.equal(broker.get("b").kind, "session.snapshot"); assert.equal(broker.get("a0").kind, "session.snapshot");
  writer.close();
});


test("T3 never assigns a previous turn outcome to a new active turn", () => {
  const rows = []; const state = new T3Sessions({ environment: "env", publish: row => rows.push(row) });
  state.project({ id: "project", workspaceRoot: "/project" }); state.item({ kind: "synchronized" });
  state.thread({ ...thread(), session: { providerName: "codex", status: "running", activeTurnId: "new" },
    latestTurn: { turnId: "old", state: "completed" } });
  assert.equal(rows.at(-1).turn_id, "new"); assert.equal(rows.at(-1).phase, "working");
  assert.equal(rows.at(-1).turn_outcome, "unknown");
});
