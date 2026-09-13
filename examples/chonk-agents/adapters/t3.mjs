// T3 Code 0.0.40, source cfeaca41ae27bdf2c203158d378c87c7308fea2a.
// Read-only Effect RPC subscription. No conversation, provider mutation or approval API.
import { open } from "node:fs/promises";
import { constants } from "node:fs";
import { randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";
import WebSocket from "ws";
import { MetadataWriter, MAX_FRAME, MAX_SESSIONS, identifier, text, cleanOrigin, boundedJson } from "./common.mjs";

export const QUALIFIED_VERSION = "0.0.40";
const ENGINES = new Set(["codex", "claude", "opencode", "grok"]);
const validSequence = value => Number.isSafeInteger(value) && value >= 0;

export async function accessToken(env = process.env) {
  if (env.CHONK_T3_ACCESS_TOKEN) {
    if (env.CHONK_T3_ACCESS_TOKEN.length > 8192 || /[\r\n]/.test(env.CHONK_T3_ACCESS_TOKEN))
      throw new Error("Invalid configured credential");
    return env.CHONK_T3_ACCESS_TOKEN;
  }
  if (!env.CHONK_T3_TOKEN_FILE) throw new Error("A T3 access token must be configured");
  const file = await open(env.CHONK_T3_TOKEN_FILE, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = await file.stat();
    if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o077) || stat.size > 8192)
      throw new Error("Credential file must be private and owned by this user");
    const bytes = Buffer.alloc(8193);
    const { bytesRead } = await file.read(bytes, 0, bytes.length, 0);
    if (bytesRead > 8192) throw new Error("Invalid configured credential");
    const token = bytes.subarray(0, bytesRead).toString("utf8").trim();
    if (!token || /[\r\n]/.test(token)) throw new Error("Invalid configured credential");
    return token;
  } finally { await file.close(); }
}

export function threadPhase(thread) {
  if (thread.hasPendingApprovals === true || thread.hasPendingUserInput === true) return "waiting";
  if (thread.backgroundLiveness === "working" || ["starting", "running"].includes(thread.session?.status)
    || thread.latestTurn?.state === "running") return "working";
  if (thread.session?.status === "error") return "error";
  if (["idle", "ready", "interrupted", "stopped"].includes(thread.session?.status)) return "idle";
  return "unknown";
}

export class T3Sessions {
  constructor({ environment, publish, incarnation = randomUUID() }) {
    if (!identifier(environment)) throw new Error("Invalid T3 environment identity");
    this.environment = environment;
    this.publish = publish;
    this.incarnation = incarnation;
    this.rows = new Map();
    this.projects = new Map();
    this.cursor = null;
    this.sequence = 0;
    this.connected = false;
    this.overflow = false;
    this.deferred = null;
    this.pendingSnapshot = null;
    this.healthLost = false;
  }
  project(project) {
    if (!identifier(project?.id)) throw new Error("Invalid project metadata");
    if (this.projects.size >= 128 && !this.projects.has(project.id)) { this.overflow = true; return; }
    this.projects.set(project.id, text(project.workspaceRoot, 1024));
  }
  thread(thread) {
    if (!identifier(thread?.id) || !identifier(thread.projectId)) throw new Error("Invalid thread metadata");
    const old = this.rows.get(thread.id);
    const provider = thread.session?.providerName;
    const turn = identifier(thread.session?.activeTurnId) ?? identifier(thread.latestTurn?.turnId);
    const row = { id: thread.id, project: thread.projectId, worktree: text(thread.worktreePath, 1024),
      title: text(thread.title, 100) || "T3 thread", engine: ENGINES.has(provider) ? provider : "unknown",
      turn,
      outcome: turn && turn === thread.latestTurn?.turnId && ["completed", "interrupted", "error"].includes(thread.latestTurn?.state)
        ? thread.latestTurn.state : "unknown",
      phase: threadPhase(thread), fingerprint: old?.fingerprint };
    if (this.pendingSnapshot?.has(row.id) && !this.rows.has(row.id)) {
      this.pendingSnapshot.set(row.id, row); this.retryPending();
    } else this.admit(row);
  }
  admit(row) {
    if (!this.rows.has(row.id) && this.rows.size >= MAX_SESSIONS) {
      this.overflow = true;
      if (this.connected) for (const known of this.rows.values()) this.emit(known);
      const victim = ["working", "waiting"].includes(row.phase)
        && [...this.rows].find(([, known]) => known.retiring || !["working", "waiting"].includes(known.phase));
      if (!victim) return;
      if (this.connected && !this.emit(victim[1], "session.disconnect")) { this.deferred = row; return; }
      this.rows.delete(victim[0]);
    }
    this.rows.set(row.id, row);
    if (this.connected) this.emit(row);
  }
  retryPending() {
    // A full resnapshot can replace all 32 identities while the consumer is
    // blocked. Keep its whitelisted desired state separately until every old
    // retirement is queued ahead of the corresponding admission.
    for (const [id, row] of this.rows) {
      if (row.retiring && this.emit(row, "session.disconnect")) this.rows.delete(id);
    }
    if (this.pendingSnapshot) {
      for (const [id, row] of this.pendingSnapshot) {
        if (!this.rows.has(id) && this.rows.size >= MAX_SESSIONS) continue;
        row.fingerprint = this.rows.get(id)?.fingerprint;
        this.rows.set(id, row); this.pendingSnapshot.delete(id);
      }
      if (!this.pendingSnapshot.size) this.pendingSnapshot = null;
    }
    if (this.deferred) { const row = this.deferred; this.deferred = null; this.admit(row); }
    for (const row of this.rows.values()) {
      if (row.retiring) continue;
      if (this.connected) this.emit(row);
      else if (this.healthLost) this.emit(row, "session.disconnect");
    }
  }
  emit(row, kind = "session.snapshot") {
    if (row.retiring) kind = "session.disconnect";
    const path = row.worktree || this.projects.get(row.project);
    const health = kind === "session.disconnect" ? "disconnected" : this.overflow || !path ? "degraded" : "connected";
    const phase = kind === "session.disconnect" ? "unknown" : row.phase;
    const cwd = path || "/";
    const fingerprint = JSON.stringify([kind, health, phase, cwd, row.title, row.turn, row.outcome, row.engine]);
    if (fingerprint === row.fingerprint) return true;
    const sequence = ++this.sequence;
    const accepted = this.publish({ schema: 2, engine: row.engine, client: "t3", origin: "t3",
      session_id: row.id, thread_id: row.id, environment: this.environment,
      incarnation: this.incarnation, sequence, event_id: `${this.incarnation}:${sequence}`,
      kind, phase, cwd, title: row.title, detail: "T3 thread metadata; provider-session association unavailable",
      source_health: health, capabilities: { observe: true, open_browser: health !== "disconnected" },
      turn_outcome: row.outcome ?? "unknown",
      ...(row.turn ? { turn_id: row.turn } : {}),
    }) !== false;
    if (accepted) row.fingerprint = fingerprint;
    return accepted;
  }
  item(item) {
    if (!item || typeof item !== "object") throw new Error("Invalid T3 stream item");
    if (item.kind === "synchronized") {
      this.connected = true; this.healthLost = false;
      this.retryPending();
      return;
    }
    if (item.kind === "snapshot") {
      const snapshot = item.snapshot;
      if (!validSequence(snapshot?.snapshotSequence) || !Array.isArray(snapshot.threads) || !Array.isArray(snapshot.projects))
        throw new Error("Invalid T3 snapshot");
      const next = new T3Sessions({ environment: this.environment, publish: () => true });
      for (const project of snapshot.projects) next.project(project);
      // Bound retained metadata before staging; source messages, scripts and
      // provider cursors never enter either table.
      for (const active of [true, false]) {
        for (const thread of snapshot.threads) {
          if (["working", "waiting"].includes(threadPhase(thread)) === active) next.thread(thread);
        }
      }
      this.projects = next.projects; this.overflow = next.overflow; this.deferred = null;
      this.pendingSnapshot = next.rows;
      for (const [id, row] of this.rows) row.retiring = !next.rows.has(id);
      this.retryPending();
      this.cursor = snapshot.snapshotSequence;
      return;
    }
    if (!validSequence(item.sequence)) throw new Error("Missing T3 replay sequence");
    if (this.cursor !== null && item.sequence <= this.cursor) return;
    switch (item.kind) {
      case "project-upserted":
        this.project(item.project);
        if (this.connected) for (const row of this.rows.values()) if (row.project === item.project.id) this.emit(row);
        break;
      case "project-removed":
        this.projects.delete(item.projectId);
        if (this.connected) for (const row of this.rows.values()) if (row.project === item.projectId) this.emit(row);
        break;
      case "thread-upserted": this.thread(item.thread); break;
      case "thread-removed": {
        const row = this.rows.get(item.threadId);
        if (row) {
          row.retiring = true;
          if (this.emit(row, "session.disconnect")) this.rows.delete(item.threadId);
        }
        this.pendingSnapshot?.delete(item.threadId);
        if (this.deferred?.id === item.threadId) this.deferred = null;
        break;
      }
      default: throw new Error("Unsupported T3 stream item");
    }
    this.cursor = item.sequence;
  }
  disconnect() {
    this.connected = false; this.healthLost = true;
    for (const row of this.rows.values()) this.emit(row, "session.disconnect");
  }
}

export function consumeRpc(raw, { sessions, send, requestId = "shell" }) {
  if (Buffer.byteLength(raw) > MAX_FRAME) throw new Error("T3 frame exceeds metadata limit");
  const value = JSON.parse(raw);
  const frames = Array.isArray(value) ? value : [value];
  if (frames.length > 256) throw new Error("T3 frame batch exceeds limit");
  for (const frame of frames) {
    if (frame?._tag === "Pong") continue;
    if (frame?._tag === "Chunk" && frame.requestId === requestId) {
      if (!Array.isArray(frame.values) || !frame.values.length || frame.values.length > 512)
        throw new Error("Invalid T3 chunk");
      for (const item of frame.values) sessions.item(item);
      send({ _tag: "Ack", requestId });
      continue;
    }
    // Do not serialize upstream errors: error payloads may contain sensitive data.
    throw new Error("T3 subscription closed or changed protocol");
  }
}

export async function runT3({ env = process.env, output = process.stdout, diagnostic = () => {}, signal,
  fetchRequest = fetch, WebSocketClass = WebSocket, retryMin = 250 } = {}) {
  const origin = cleanOrigin(env.CHONK_T3_URL ?? "");
  let sessions;
  const writer = new MetadataWriter(output, { onReady: () => sessions?.retryPending() });
  let stopped = false;
  let socket;
  let reconnectTimer;
  let resumeSleep;
  let delay = retryMin;
  const stop = () => {
    stopped = true; clearTimeout(reconnectTimer); resumeSleep?.(); socket?.terminate();
  };
  output.on("error", stop);
  output.on("close", stop);
  signal?.addEventListener("abort", stop, { once: true });
  if (signal?.aborted) stop();
  try {
    while (!stopped) {
      try {
        const requestSignal = signal ? AbortSignal.any([signal, AbortSignal.timeout(5000)]) : AbortSignal.timeout(5000);
        const descriptor = await boundedJson(await fetchRequest(`${origin}/.well-known/t3/environment`, {
          signal: requestSignal, redirect: "error",
        }), 65536);
        if (descriptor.serverVersion !== QUALIFIED_VERSION || !identifier(descriptor.environmentId))
          throw new Error("Unsupported T3 server contract");
        // One adapter process owns one native environment. Replacing it here
        // could discard blocked retirements from the previous environment.
        // An endpoint identity change requires an explicit adapter restart.
        if (sessions && sessions.environment !== descriptor.environmentId)
          throw new Error("T3 environment identity changed; restart the adapter");
        if (!sessions) sessions = new T3Sessions({ environment: descriptor.environmentId, publish: row => writer.push(row) });
        const token = await accessToken(env);
        const ticket = await boundedJson(await fetchRequest(`${origin}/api/auth/websocket-ticket`, {
          method: "POST", headers: { authorization: `Bearer ${token}` }, signal: requestSignal, redirect: "error",
        }), 65536);
        if (stopped) break;
        if (!identifier(ticket.ticket, 256)) throw new Error("Invalid T3 connection ticket");
        const url = new URL(origin); url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
        url.pathname = "/ws"; url.searchParams.set("wsTicket", ticket.ticket);
        await new Promise((resolve, reject) => {
          const ws = new WebSocketClass(url, { maxPayload: MAX_FRAME, maxFragments: 1024,
            maxBufferedChunks: 4096, perMessageDeflate: false, handshakeTimeout: 5000, followRedirects: false });
          socket = ws;
          let synchronized = false;
          let pingDeadline;
          let pingTimer;
          const timeout = setTimeout(() => { ws.terminate(); reject(new Error("T3 synchronization timeout")); }, 10_000);
          const cleanup = () => { clearTimeout(timeout); clearTimeout(pingDeadline); clearInterval(pingTimer); };
          const send = frame => {
            if (ws.bufferedAmount > 65536) throw new Error("T3 transport is overloaded");
            ws.send(JSON.stringify(frame));
          };
          ws.on("open", () => {
            try {
              send({ _tag: "Request", id: "shell", tag: "orchestration.subscribeShell", headers: [],
                payload: { ...(sessions.cursor !== null ? { afterSequence: sessions.cursor } : {}), requestCompletionMarker: true } });
              // Transport liveness only: no state polling and no compositor
              // update on an unchanged healthy connection.
              pingTimer = setInterval(() => {
                try { send({ _tag: "Ping" }); pingDeadline = setTimeout(() => ws.terminate(), 10_000); }
                catch { ws.terminate(); }
              }, 30_000);
            } catch { ws.terminate(); }
          });
          ws.on("message", (data, binary) => {
            try {
              if (binary) throw new Error("Unexpected binary T3 frame");
              consumeRpc(data.toString("utf8"), { sessions, send });
              clearTimeout(pingDeadline);
              if (sessions.connected && !synchronized) {
                synchronized = true; clearTimeout(timeout); delay = retryMin;
                diagnostic("T3 metadata subscription synchronized");
              }
            } catch { ws.terminate(); }
          });
          ws.on("error", () => { cleanup(); ws.terminate(); reject(new Error("T3 transport unavailable")); });
          ws.on("close", () => { cleanup(); resolve(); });
        });
      } catch { if (!stopped) diagnostic("T3 metadata unavailable; reconnecting with bounded backoff"); }
      sessions?.disconnect();
      if (!stopped) await new Promise(resolve => {
        resumeSleep = resolve;
        reconnectTimer = setTimeout(resolve, delay);
        delay = Math.min(30_000, delay * 2);
      });
    }
  } finally {
    signal?.removeEventListener("abort", stop);
    output.off("error", stop); output.off("close", stop);
    sessions?.disconnect(); writer.close();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const controller = new AbortController();
  process.once("SIGTERM", () => controller.abort());
  process.once("SIGINT", () => controller.abort());
  void runT3({ signal: controller.signal, diagnostic: message => process.stderr.write(message + "\n") })
    .catch(() => { process.stderr.write("T3 adapter configuration is invalid\n"); process.exitCode = 1; });
}
