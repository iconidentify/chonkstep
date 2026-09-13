import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { connect } from "node:net";
import { mkdtemp, chmod, lstat, rm, writeFile, symlink } from "node:fs/promises";
import { EventEmitter } from "node:events";
import { Writable } from "node:stream";
import { randomUUID } from "node:crypto";
import { spawn, execFileSync } from "node:child_process";
import { WebSocketServer } from "ws";
import { createNavigation, createTuiPlugin } from "../opencode-tui.mjs";
import { PluginBridge } from "../opencode.mjs";
import { accessToken, runT3 } from "../t3.mjs";

test("Persistent bridge restarts after child loss, replays latest state, and closes without orphaning", async () => {
  const dir = await mkdtemp("/tmp/cbridge-"); const helper = `${dir}/bridge.mjs`;
  await writeFile(helper, `import readline from 'node:readline';
const lines=readline.createInterface({input:process.stdin});
lines.on('line',line=>{const row=JSON.parse(line);process.stdout.write(JSON.stringify({sequence:row.sequence})+'\\n')});
lines.on('close',()=>process.exit(0));`);
  const commands = []; const children = [];
  const bridge = new PluginBridge({ bridgeCommand: [process.execPath, helper],
    spawnProcess: (...args) => { const child = spawn(...args); children.push(child); return child; },
    onCommand: row => commands.push(row) });
  const until = async predicate => {
    const deadline = Date.now() + 2000;
    while (!predicate()) { if (Date.now() >= deadline) throw new Error("Bridge test deadline"); await new Promise(resolve => setTimeout(resolve, 5)); }
  };
  try {
    for (let sequence = 0; sequence < 1000; sequence++) bridge.publish({ engine: "opencode", environment: "env", session_id: "ses_a", sequence });
    await until(() => commands.some(row => row.sequence === 999));
    assert.equal(children.length, 1);
    children[0].kill("SIGKILL");
    await until(() => bridge.child === null);
    bridge.publish({ engine: "opencode", environment: "env", session_id: "ses_a", sequence: 1000 });
    await until(() => children.length === 2 && commands.some(row => row.sequence === 1000));
    await bridge.close();
    assert.equal(bridge.child, null); assert.equal(bridge.timer, null);
    assert.ok(children.every(child => child.exitCode !== null || child.signalCode !== null));
  } finally { await bridge.close(); for (const child of children) child.kill(); await rm(dir, { recursive: true }); }
});

test("Native TUI navigation acknowledges actual selected route and rejects stale incarnation", async () => {
  const runtime = await mkdtemp("/tmp/cnav-");
  let selected = "ses_a";
  const incarnation = randomUUID();
  const nav = await createNavigation({ runtime, incarnation, known: id => ["ses_a", "ses_b"].includes(id),
    select: id => { setTimeout(() => { selected = id; nav.reconcile(); }, 10); }, current: () => selected });
  const request = value => new Promise((resolve, reject) => {
    const peer = connect(nav.binding.socket); let result = "";
    peer.setEncoding("utf8"); peer.on("error", reject);
    peer.on("connect", () => peer.end(JSON.stringify(value) + "\n"));
    peer.on("data", data => { result += data; });
    peer.on("end", () => { try { resolve(JSON.parse(result)); } catch (error) { reject(error); } });
  });
  try {
    assert.equal((await lstat(nav.binding.socket)).mode & 0o777, 0o600);
    const success = await request({ action: "select_session", session_id: "ses_b", request_id: "one", incarnation });
    assert.deepEqual(success, { request_id: "one", ok: true, session_id: "ses_b" });
    assert.equal(selected, "ses_b");
    assert.equal((await request({ action: "select_session", session_id: "ses_a", request_id: "two", incarnation: "stale" })).ok, false);
    assert.equal(selected, "ses_b");
    assert.equal((await request({ action: "select_session", session_id: "ses_unknown", request_id: "three", incarnation })).ok, false);
  } finally { await nav.close(); await rm(runtime, { recursive: true }); }
});

test("TUI navigation refuses an exposed runtime directory", async () => {
  const runtime = await mkdtemp("/tmp/cnav-");
  const directory = `${runtime}/chonk-agents`;
  const { mkdir } = await import("node:fs/promises");
  await mkdir(directory); await chmod(directory, 0o755);
  try { await assert.rejects(createNavigation({ runtime, incarnation: randomUUID(), known: () => true, select: () => {}, current: () => null })); }
  finally { await rm(runtime, { recursive: true }); }
});

test("Explicit T3 token file requires private regular owned storage", async () => {
  const dir = await mkdtemp("/tmp/ctok-"); const path = `${dir}/token`;
  try {
    await writeFile(path, "fixture-secret\n", { mode: 0o600 });
    assert.equal(await accessToken({ CHONK_T3_TOKEN_FILE: path }), "fixture-secret");
    await chmod(path, 0o644); await assert.rejects(accessToken({ CHONK_T3_TOKEN_FILE: path }));
    await chmod(path, 0o600); await symlink(path, `${dir}/link`);
    await assert.rejects(accessToken({ CHONK_T3_TOKEN_FILE: `${dir}/link` }));
    execFileSync("mkfifo", [`${dir}/fifo`]);
    await assert.rejects(accessToken({ CHONK_T3_TOKEN_FILE: `${dir}/fifo` }));
    await writeFile(path, "x".repeat(8193), { mode: 0o600 });
    await assert.rejects(accessToken({ CHONK_T3_TOKEN_FILE: path }));
  } finally { await rm(dir, { recursive: true }); }
});

test("Real local T3 wire authenticates, acknowledges, resumes and redacts sensitive fields", async () => {
  const headers = []; const requests = []; const rows = []; const diagnostics = [];
  const controller = new AbortController();
  const http = createServer((request, response) => {
    response.setHeader("content-type", "application/json");
    if (request.url === "/.well-known/t3/environment") response.end(JSON.stringify({ serverVersion: "0.0.40", environmentId: "fixture-env" }));
    else if (request.url === "/api/auth/websocket-ticket" && request.method === "POST") {
      headers.push(request.headers.authorization); response.end(JSON.stringify({ ticket: "fixture-ticket" }));
    } else { response.statusCode = 404; response.end("{}"); }
  });
  const wss = new WebSocketServer({ server: http, perMessageDeflate: false });
  let connections = 0;
  wss.on("connection", (socket, request) => {
    assert.equal(new URL(request.url, "http://localhost").searchParams.get("wsTicket"), "fixture-ticket");
    const connection = ++connections;
    socket.on("message", data => {
      const frame = JSON.parse(data);
      if (frame._tag === "Request") {
        requests.push(frame);
        const thread = { id: "thread", projectId: "project", title: "Fixture", session: { providerName: "codex", status: "ready" },
          hasPendingApprovals: connection > 1, hasPendingUserInput: false, messages: [{ text: "PRIVATE-PROMPT" }] };
        const values = connection === 1
          ? [{ kind: "snapshot", snapshot: { snapshotSequence: 5, projects: [{ id: "project", workspaceRoot: "/fixture" }], threads: [thread] } }, { kind: "synchronized" }]
          : [{ kind: "thread-upserted", sequence: 6, thread }, { kind: "synchronized" }];
        socket.send(JSON.stringify({ _tag: "Chunk", requestId: "shell", values }));
      } else if (frame._tag === "Ack" && connection === 1) socket.close();
    });
  });
  await new Promise(resolve => http.listen(0, "127.0.0.1", resolve));
  const output = new Writable({ write(chunk, _encoding, done) {
    for (const line of chunk.toString().trim().split("\n")) {
      const row = JSON.parse(line); rows.push(row);
      if (row.phase === "waiting") setImmediate(() => controller.abort());
    }
    done();
  } });
  const timeout = setTimeout(() => controller.abort(), 4000);
  try {
    await runT3({ env: { CHONK_T3_URL: `http://127.0.0.1:${http.address().port}`, CHONK_T3_ACCESS_TOKEN: "fixture-secret" },
      output, diagnostic: message => diagnostics.push(message), signal: controller.signal, retryMin: 5 });
    assert.ok(connections >= 2);
    assert.ok(headers.every(value => value === "Bearer fixture-secret"));
    assert.equal(requests[0].tag, "orchestration.subscribeShell");
    assert.equal(requests[0].payload.requestCompletionMarker, true);
    assert.equal(requests[1].payload.afterSequence, 5);
    assert.ok(rows.some(row => row.phase === "idle"));
    assert.ok(rows.some(row => row.source_health === "disconnected"));
    assert.ok(rows.some(row => row.phase === "waiting"));
    assert.ok(!JSON.stringify([rows, diagnostics]).match(/PRIVATE-PROMPT|fixture-secret|fixture-ticket/));
  } finally {
    clearTimeout(timeout); controller.abort();
    for (const socket of wss.clients) socket.terminate();
    await new Promise(resolve => wss.close(resolve));
    await new Promise(resolve => http.close(resolve));
  }
});


test("Native TUI factory observes resumed route, native errors, capacity retry and disposal", async () => {
  const runtime = await mkdtemp("/tmp/ctui-");
  const rows = []; const handlers = new Map(); let observe; let dispose; let ready = false;
  const sessions = new Map([["resumed", { id: "resumed", projectID: "project", directory: "/project", title: "Resumed" }],
    ["history", { id: "history", projectID: "project", directory: "/project", title: "History" }]]);
  const statuses = new Map(); let selected = "resumed"; let childCount = 0; let rootDisposed = false;
  const api = {
    state: { get ready() { return ready; }, path: { directory: "/project" }, session: {
      get: id => sessions.get(id), status: id => statuses.get(id) ?? { type: "idle" }, permission: () => [], question: () => [],
      messages: id => [{ id: `turn-${id}`, sessionID: id, role: "user", parts: [{ text: "PRIVATE-PART" }] }],
    } },
    route: { get current() { return { name: "session", params: { sessionID: selected } }; }, navigate: (_name, params) => { selected = params.sessionID; observe(); } },
    event: { on: (name, handler) => { handlers.set(name, handler); return () => handlers.delete(name); } },
    lifecycle: { onDispose: callback => { dispose = callback; } },
  };
  const plugin = createTuiPlugin({ runtime,
    reactive: { createRoot: callback => callback(() => { rootDisposed = true; }),
      createEffect: callback => { observe = callback; callback(); }, createSignal: () => [() => 0, () => observe?.()] },
    spawnProcess: () => {
      childCount++;
      const child = new EventEmitter(); const stdin = new EventEmitter(); const stdout = new EventEmitter();
      stdin.write = line => { rows.push(JSON.parse(line)); return true; };
      stdin.end = () => queueMicrotask(() => child.emit("exit", 0));
      stdout.setEncoding = () => {}; child.stdin = stdin; child.stdout = stdout;
      child.kill = () => child.emit("exit", null, "SIGTERM");
      queueMicrotask(() => child.emit("spawn")); return child;
    },
  });
  try {
    await plugin(api); assert.equal(childCount, 0);
    ready = true; observe(); await new Promise(resolve => setImmediate(resolve));
    assert.equal(rows.at(-1).session_id, "resumed");
    assert.equal(rows.at(-1).turn_id, "turn-resumed");
    assert.ok(!rows.some(row => row.session_id === "history"));
    assert.equal(rows.at(-1).capabilities.open_session, true);
    const emit = (type, properties) => handlers.get(type)?.({ type, properties });
    emit("message.updated", { info: { id: "next-turn", sessionID: "resumed", role: "user", parts: [{ text: "PRIVATE-PART" }] } });
    assert.equal(rows.at(-1).turn_id, "next-turn");
    emit("session.created", { info: { id: "unrelated-child", parentID: "history", directory: "/project" } });
    assert.ok(!rows.some(row => row.session_id === "unrelated-child"));
    sessions.set("child", { id: "child", parentID: "resumed", projectID: "project", directory: "/project", title: "Child" });
    statuses.set("child", { type: "busy" }); emit("session.created", { info: sessions.get("child") });
    assert.equal(rows.findLast(row => row.session_id === "child").parent_session_id, "resumed");
    assert.equal(rows.findLast(row => row.session_id === "child").phase, "working");
    sessions.delete("child"); observe();
    assert.equal(rows.findLast(row => row.session_id === "child").phase, "unknown");
    emit("session.deleted", { info: { id: "child", parentID: "resumed", directory: "/project" } });
    assert.equal(rows.at(-1).kind, "session.end");
    emit("session.error", { sessionID: "history", error: { message: "SECRET" } });
    emit("session.error", { sessionID: "resumed", error: { message: "SECRET" } });
    observe(); assert.equal(rows.at(-1).phase, "error");
    statuses.set("resumed", { type: "busy" }); emit("session.status", { sessionID: "resumed", status: { type: "busy" } }); observe();
    assert.equal(rows.at(-1).phase, "working");
    statuses.set("resumed", { type: "idle" }); observe();
    for (let i = 0; i < 33; i++) {
      selected = `s${i}`; sessions.set(selected, { id: selected, projectID: "project", directory: "/project", title: "Next" }); observe();
    }
    assert.equal(rows.at(-1).session_id, "s32");
    assert.ok(rows.some(row => row.kind === "session.disconnect"));
    assert.ok(!JSON.stringify(rows).match(/SECRET|PRIVATE-PART/));
    await dispose(); assert.equal(handlers.size, 0); assert.equal(rootDisposed, true); assert.equal(childCount, 1);
    const count = rows.length; observe(); assert.equal(rows.length, count);
  } finally { await dispose?.(); await rm(runtime, { recursive: true, force: true }); }
});


test("T3 endpoint identity change cannot discard the original source's pending disconnect", async () => {
  let descriptors = 0; let tickets = 0; let connections = 0;
  const rows = []; const controller = new AbortController();
  class Socket extends EventEmitter {
    bufferedAmount = 0;
    constructor() { super(); connections++; queueMicrotask(() => this.emit("open")); }
    send(raw) {
      const frame = JSON.parse(raw);
      if (frame._tag === "Request") queueMicrotask(() => this.emit("message", Buffer.from(JSON.stringify({ _tag: "Chunk", requestId: "shell", values: [
        { kind: "snapshot", snapshot: { snapshotSequence: 1, projects: [{ id: "p", workspaceRoot: "/fixture" }],
          threads: [{ id: "thread", projectId: "p", title: "Fixture", session: { providerName: "codex", status: "running" } }] } },
        { kind: "synchronized" },
      ] })), false));
      else if (frame._tag === "Ack") queueMicrotask(() => this.emit("close"));
    }
    terminate() { this.emit("close"); }
  }
  const fetchRequest = async url => {
    if (url.endsWith("/environment")) {
      descriptors++;
      if (descriptors > 1) setImmediate(() => controller.abort());
      return Response.json({ serverVersion: "0.0.40", environmentId: descriptors === 1 ? "original" : "replacement" });
    }
    tickets++; return Response.json({ ticket: "fixture-ticket" });
  };
  const output = new Writable({ write(chunk, _encoding, done) { rows.push(JSON.parse(chunk.toString())); done(); } });
  await runT3({ env: { CHONK_T3_URL: "http://127.0.0.1:1234", CHONK_T3_ACCESS_TOKEN: "fixture" },
    output, signal: controller.signal, fetchRequest, WebSocketClass: Socket, retryMin: 1 });
  assert.equal(connections, 1); assert.equal(tickets, 1);
  assert.equal(rows.at(-1).source_health, "disconnected");
  assert.ok(rows.every(row => row.environment === "original"));
});
