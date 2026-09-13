// Qualified against OpenCode v1.18.29, commit 16747470f976aca3d362ad730bcd3fe82ecc2c9a.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { MetadataWriter, MAX_SESSIONS, identifier, text, localEnvironment, eventKey } from "./common.mjs";

const OBSERVED_EVENTS = new Set(["session.created", "session.updated", "session.deleted", "session.status",
  "session.error", "session.compacted", "permission.asked", "permission.replied",
  "question.asked", "question.replied", "question.rejected", "message.updated"]);

export class PluginBridge {
  constructor({ bridgeCommand, spawnProcess = spawn, onCommand = () => {}, onReady = () => {} } = {}) {
    this.command = bridgeCommand ?? ["python3", fileURLToPath(new URL("../chonk-agents.py", import.meta.url)), "stream", "opencode"];
    this.spawnProcess = spawnProcess;
    this.onCommand = onCommand;
    this.onReady = onReady;
    this.latest = new Map();
    this.retired = new Map();
    this.closed = false;
    this.delay = 250;
    this.timer = null;
    this.child = null;
    this.writer = null;
  }
  publish(row) {
    if (this.closed) return false;
    const key = eventKey(row);
    const retired = row.kind === "session.disconnect" || row.kind === "session.end";
    if (retired) {
      if (!this.retired.has(key) && this.retired.size >= MAX_SESSIONS) return false;
      this.retired.set(key, row);
      this.latest.delete(key);
    } else {
      if (!this.latest.has(key) && this.latest.size >= MAX_SESSIONS) return false;
      this.latest.set(key, row);
      this.retired.delete(key);
    }
    const connected = Boolean(this.child);
    if (!this.child && !this.timer) this.start();
    if (connected) this.writer?.push(row);
    return true;
  }
  start() {
    if (this.closed || this.child || !this.latest.size && !this.retired.size) return;
    let child;
    try {
      child = this.spawnProcess(this.command[0], this.command.slice(1), {
        stdio: ["pipe", "pipe", "ignore"], env: localEnvironment(), windowsHide: true,
      });
    } catch { this.retry(); return; }
    this.child = child;
    // Disposal may race a peer exit after MetadataWriter removes its listeners.
    child.stdin.on("error", () => {});
    child.stdout.on("error", () => {});
    this.writer = new MetadataWriter(child.stdin, {
      onWritten: (key, row) => {
        if (this.retired.get(key)?.sequence === row.sequence) this.retired.delete(key);
      },
      onReady: () => this.onReady(),
    });
    let incoming = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", data => {
      incoming += data;
      if (Buffer.byteLength(incoming) > 8192) { child.kill(); return; }
      for (;;) {
        const end = incoming.indexOf("\n");
        if (end < 0) break;
        const line = incoming.slice(0, end); incoming = incoming.slice(end + 1);
        try { this.onCommand(JSON.parse(line)); } catch { /* Observation cannot fail OpenCode. */ }
      }
    });
    const failed = () => {
      if (this.child !== child) return;
      this.writer?.close(); this.writer = null; this.child = null;
      this.retry();
    };
    child.once("error", failed);
    child.once("exit", failed);
    child.once("spawn", () => { this.replay(); this.onReady(); });
  }
  replay() {
    for (const row of this.retired.values()) this.writer?.push(row);
    for (const row of this.latest.values()) this.writer?.push(row);
  }
  retry() {
    if (this.closed || this.timer || !this.latest.size && !this.retired.size) return;
    this.timer = setTimeout(() => {
      this.timer = null; this.start();
    }, this.delay);
    this.timer.unref?.();
    this.delay = Math.min(30_000, this.delay * 2);
  }
  async close() {
    this.closed = true;
    clearTimeout(this.timer); this.timer = null;
    const child = this.child;
    this.writer?.close(); this.writer = null;
    this.latest.clear(); this.retired.clear();
    if (!child) return;
    child.stdin.end();
    await new Promise(resolve => {
      const timer = setTimeout(() => { child.kill(); resolve(); }, 100);
      child.once("exit", () => { clearTimeout(timer); resolve(); });
    });
    this.child = null;
  }
}

export class OpenCodeSessions {
  constructor({ directory, projectID, publish, incarnation = randomUUID(), navigation }) {
    this.directory = text(directory, 1024);
    this.environment = `opencode:${identifier(projectID, 128) ?? "local"}`;
    this.publish = publish;
    this.incarnation = incarnation;
    this.navigation = navigation;
    this.sequence = 0;
    this.rows = new Map();
    this.seen = new Set();
    this.selectedId = null;
    this.overflow = false;
  }
  row(id) {
    if (!identifier(id)) return null;
    if (!this.rows.has(id)) {
      if (this.rows.size >= MAX_SESSIONS) {
        const eligible = ([id, row]) => id !== this.selectedId && !row.pending.size && row.phase !== "working";
        const victim = [...this.rows].find(eligible);
        this.degrade();
        if (!victim || (victim[1].fingerprint && !this.emit(victim[1], null, "session.disconnect", "Observation capacity reached"))) return null;
        this.rows.delete(victim[0]);
      }
      this.rows.set(id, { id, phase: "unknown", title: "OpenCode session", cwd: this.directory,
        pending: new Set(), sequence: 0, attached: false, observed: false, health: "connected" });
    }
    return this.rows.get(id);
  }
  degrade() {
    this.overflow = true;
    for (const row of this.rows.values()) {
      row.health = "degraded";
      if (row.phase !== "ended" && (row.attached || row.observed || row.fingerprint)) this.emit(row);
    }
  }
  metadata(info) {
    const row = this.row(info?.id);
    if (!row) return null;
    row.title = text(info.title, 100) || row.title;
    row.cwd = text(info.directory, 1024) || row.cwd;
    row.parent = identifier(info.parentID);
    return row;
  }
  emit(row, eventId, kind = "session.snapshot", detail = "Session activity reported by OpenCode") {
    const navigation = row.nativeUnavailable ? null : this.navigation;
    const fingerprint = JSON.stringify([kind, row.phase, [...row.pending].sort(), row.title,
      row.cwd, row.parent, row.turn, row.outcome, row.health, navigation]);
    if (fingerprint === row.fingerprint) return true;
    row.sequence = ++this.sequence;
    const accepted = this.publish({ schema: 2, engine: "opencode", client: "terminal", origin: "plugin",
      session_id: row.id, environment: this.environment, incarnation: this.incarnation,
      sequence: row.sequence, event_id: identifier(eventId) ?? `${this.incarnation}:${row.sequence}`,
      cwd: row.cwd, title: row.title, kind,
      phase: kind === "session.disconnect" ? "unknown" : row.pending.size ? "waiting" : row.phase,
      detail, source_health: kind === "session.disconnect" ? "disconnected" : row.health,
      capabilities: { observe: true, open_browser: false, ...(navigation ? { open_session: true } : {}) },
      ...(navigation ? { navigation } : {}),
      ...(row.parent ? { parent_session_id: row.parent } : {}),
      turn_outcome: row.outcome ?? "unknown",
      ...(row.turn ? { turn_id: row.turn } : {}),
    }) !== false;
    if (accepted) row.fingerprint = fingerprint;
    return accepted;
  }
  userTurn(info, emit = true) {
    // User-message metadata is the native turn identity. Do not read parts,
    // content, prompt, tool fields, or assistant deltas.
    if (info?.role !== "user" || !identifier(info.id)) return;
    const row = this.rows.get(info.sessionID);
    if (!row || row.turn === info.id) return;
    row.turn = info.id; row.error = false; row.outcome = "unknown";
    if (row.phase === "error") row.phase = "unknown";
    if (emit) this.emit(row);
  }
  event(event) {
    if (!event || typeof event.type !== "string") return;
    if (!OBSERVED_EVENTS.has(event.type)) return; // Ignore all token/tool/prompt payload events before serialization.
    const eid = identifier(event.id);
    if (eid && this.seen.has(eid)) return;
    if (eid) { this.seen.add(eid); if (this.seen.size > 256) this.seen.delete(this.seen.values().next().value); }
    const p = event.properties;
    if (!p || typeof p !== "object") return;
    if (event.type === "message.updated") { this.userTurn(p.info); return; }
    const row = p.info ? this.metadata(p.info) : this.row(p.sessionID);
    if (!row) return;
    row.sequence = ++this.sequence;
    row.observed = true;
    if (event.type === "session.status") {
      const status = p.status?.type;
      if (!["idle", "busy", "retry"].includes(status)) return;
      if (status !== "idle" && !["busy", "retry"].includes(row.status)) { row.error = false; row.outcome = "unknown"; }
      row.status = status;
      row.phase = row.error ? "error" : status === "idle" ? "idle" : "working";
    } else if (event.type === "session.deleted") {
      row.phase = "ended"; row.pending.clear();
    } else if (event.type === "session.created") row.phase = "idle";
    else if (event.type === "session.error") {
      row.outcome = p.error?.name === "MessageAbortedError" ? "interrupted" : "error";
      row.error = row.outcome === "error";
      row.phase = row.error ? "error" : "idle";
    }
    else if (event.type.endsWith(".asked")) {
      const request = identifier(p.id);
      if (request && row.pending.size < 64) row.pending.add(request);
      else row.health = "degraded";
    } else if (event.type.endsWith(".replied") || event.type.endsWith(".rejected")) {
      row.pending.delete(p.requestID);
    }
    this.emit(row, eid, row.phase === "ended" ? "session.end" : "session.snapshot");
  }
  snapshot(info, status, permissions = [], questions = [], { attached = false, expectedSequence, health = "connected" } = {}) {
    const prior = this.rows.get(info?.id);
    if (expectedSequence !== undefined && (prior?.sequence ?? 0) !== expectedSequence) return;
    const row = this.metadata(info);
    if (!row) return;
    row.health = this.overflow ? "degraded" : health;
    row.attached = attached || row.attached;
    row.pending.clear();
    for (const request of [...permissions.slice(0, 64), ...questions.slice(0, 64)]) {
      if (request.sessionID === row.id && identifier(request.id) && row.pending.size < 64) row.pending.add(request.id);
    }
    const nativeStatus = status?.type ?? "idle";
    if (["busy", "retry"].includes(nativeStatus) && !["busy", "retry"].includes(row.status)) { row.error = false; row.outcome = "unknown"; }
    row.status = nativeStatus;
    row.phase = row.error ? "error" : ["busy", "retry"].includes(nativeStatus) ? "working" : attached ? "idle" : "unknown";
    if (attached || status || row.pending.size || row.observed) this.emit(row);
  }
  disconnect() {
    for (const row of this.rows.values()) if (row.attached || row.observed || row.phase !== "unknown") this.emit(row, null, "session.disconnect");
  }
}

// v1.18.29 injects the legacy SDK with a protected but stable low-level client.
// That transport preserves its in-process fetch and auth without inspecting any
// credentials. Never replace it with unauthenticated localhost:4096 requests.
export async function readPending(client, kind, signal) {
  const publicMethod = client[kind]?.list;
  const result = typeof publicMethod === "function"
    ? await publicMethod.call(client[kind], { signal })
    : await client._client?.get({ url: `/${kind}`, signal });
  if (!result || result.error || !Array.isArray(result.data) || result.data.length > 256)
    throw new Error("Pending-request snapshot unavailable");
  return result.data;
}

export function createPlugin(options = {}) {
  return async context => {
    const bridge = new PluginBridge(options);
    const sessions = new OpenCodeSessions({ directory: context.directory, projectID: context.project?.id,
      publish: row => bridge.publish(row) });
    const controller = new AbortController();
    const refresh = async () => {
      const stamps = new Map([...sessions.rows].map(([id, row]) => [id, row.sequence]));
      const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(1500)]);
      const results = await Promise.allSettled([
        context.client.session.list({ query: { directory: context.directory, limit: 32 }, signal }),
        context.client.session.status({ signal }),
        readPending(context.client, "permission", signal), readPending(context.client, "question", signal),
      ]);
      if (controller.signal.aborted) return;
      const value = (i, fallback) => results[i].status === "fulfilled" ? results[i].value : fallback;
      const infos = value(0, {})?.data;
      const statuses = value(1, {})?.data ?? {};
      if (!Array.isArray(infos)) return;
      const healthy = results.every(x => x.status === "fulfilled") && !value(0, {})?.error
        && !value(1, {})?.error && typeof value(1, {})?.data === "object";
      for (const info of infos.slice(0, MAX_SESSIONS)) {
        sessions.snapshot(info, statuses[info.id], value(2, []), value(3, []), {
          expectedSequence: stamps.get(info.id) ?? 0, health: healthy ? "connected" : "degraded",
        });
      }
    };
    const timer = setTimeout(() => { void refresh().catch(() => {}); }, 0);
    timer.unref?.();
    return {
      event: async ({ event }) => { try { sessions.event(event); } catch { /* Never change agent behavior. */ } },
      dispose: async () => {
        clearTimeout(timer); controller.abort(); sessions.disconnect(); await bridge.close();
      },
    };
  };
}
