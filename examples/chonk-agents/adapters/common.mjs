// Metadata only. No provider configuration, transcript, prompt or tool payloads.
export const MAX_SESSIONS = 32;
export const MAX_LINE = 8192;
export const MAX_FRAME = 4 * 1024 * 1024;

export function text(value, bytes = 160) {
  if (typeof value !== "string") return "";
  const clean = value.replace(/[\p{Cc}\p{Cf}]/gu, "");
  return Buffer.from(clean).subarray(0, bytes).toString("utf8").replace(/\uFFFD$/u, "");
}

export function identifier(value, maximum = 160) {
  return typeof value === "string" && value.length > 0 && Buffer.byteLength(value) <= maximum
    && !/[\p{Cc}\p{Cf}]/u.test(value) ? value : null;
}

export function localEnvironment(env = process.env) {
  const names = ["PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME", "XDG_STATE_HOME", "WAYLAND_DISPLAY", "DISPLAY", "TERM", "WINDOWID",
    "TMUX", "TMUX_PANE", "KITTY_WINDOW_ID", "WEZTERM_PANE", "CHONK_CONTROL_SOCKET"];
  return Object.fromEntries(names.filter(key => typeof env[key] === "string").map(key => [key, env[key]]));
}

export function eventKey(row) {
  return JSON.stringify([row.client === "t3" || row.origin === "t3" ? "t3" : row.engine, row.environment, row.session_id]);
}

// A slow/absent consumer cannot grow a token-sized queue. One latest semantic
// state per session is retained; writes stop immediately on stream backpressure.
export class MetadataWriter {
  constructor(stream, { limit = MAX_SESSIONS, onReady = () => {}, onWritten = () => {} } = {}) {
    this.stream = stream;
    this.limit = limit;
    this.pending = new Map();
    this.blocked = false;
    this.closed = false;
    this.flushing = false;
    this.onReady = onReady;
    this.onWritten = onWritten;
    this.onDrain = () => { this.blocked = false; this.flush(); this.onReady(); };
    this.onError = () => { this.closed = true; this.pending.clear(); };
    stream.on("drain", this.onDrain);
    stream.on("error", this.onError);
  }
  push(row) {
    if (this.closed) return false;
    const line = JSON.stringify(row) + "\n";
    if (Buffer.byteLength(line) > MAX_LINE) return false;
    const key = eventKey(row);
    const retired = row.kind === "session.disconnect" || row.kind === "session.end";
    // A retirement must reach the broker before its replacement can occupy the
    // broker's 32-row table. Reserve another 32 *bounded* transition slots; never
    // remove an unsent retirement merely to admit the replacement.
    let count = 0;
    for (const [other, value] of this.pending) if (other !== key && value.retired === retired) count++;
    if (count >= this.limit) return false;
    this.pending.set(key, { line, row, retired });
    this.flush();
    return true;
  }
  flush() {
    if (this.flushing) return;
    this.flushing = true;
    try {
      while (!this.closed && !this.blocked && this.pending.size) {
        // A session may be reselected before its old retirement flushes, so
        // that obsolete transition can become a live snapshot again. Drain all
        // *remaining* retirements before admissions to preserve broker capacity.
        let entry;
        for (const candidate of this.pending) if (candidate[1].retired) { entry = candidate; break; }
        const [key, value] = entry ?? this.pending.entries().next().value;
        this.pending.delete(key);
        try {
          this.blocked = !this.stream.write(value.line);
          this.onWritten(key, value.row);
        } catch { this.onError(); }
      }
    } finally { this.flushing = false; }
  }
  close() {
    this.closed = true;
    this.pending.clear();
    this.stream.off("drain", this.onDrain);
    this.stream.off("error", this.onError);
  }
}

export function cleanOrigin(value) {
  const url = new URL(value);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash)
    throw new Error("A credential-free HTTP(S) origin is required");
  if (url.pathname !== "/") throw new Error("Use the server origin without a path");
  if (url.protocol === "http:" && !["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))
    throw new Error("Remote servers require HTTPS");
  return url.origin;
}

export async function boundedJson(response, maximum = MAX_FRAME) {
  if (!response.ok) throw new Error("Server request failed");
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > maximum) throw new Error("Server response exceeds metadata limit");
      chunks.push(Buffer.from(value));
    }
    return JSON.parse(Buffer.concat(chunks, size).toString("utf8"));
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
