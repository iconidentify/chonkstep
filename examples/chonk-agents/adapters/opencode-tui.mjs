// A native v1.18.29 TUI observer. Host Solid is reused; no polling or UI nodes.
import { createServer } from "node:net";
import { mkdir, lstat, chmod, unlink } from "node:fs/promises";
import { join, isAbsolute } from "node:path";
import { randomUUID } from "node:crypto";
import { identifier } from "./common.mjs";
import { PluginBridge, OpenCodeSessions } from "./opencode.mjs";

export async function createNavigation({ incarnation, known, select, current, runtime = process.env.XDG_RUNTIME_DIR }) {
  if (!runtime || !isAbsolute(runtime)) throw new Error("Private runtime directory unavailable");
  const directory = join(runtime, "chonk-agents");
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const stat = await lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid() || (stat.mode & 0o077))
    throw new Error("Private runtime directory unavailable");
  const path = join(directory, `opencode-${process.pid}-${incarnation}.sock`);
  if (Buffer.byteLength(path) >= 104) throw new Error("Runtime socket path too long");
  const peers = new Set();
  const pending = new Map();
  let closed = false;
  const server = createServer({ allowHalfOpen: true }, socket => {
    if (peers.size >= 4 || closed) { socket.destroy(); return; }
    peers.add(socket);
    socket.setEncoding("utf8");
    socket.setTimeout(2000, () => socket.destroy());
    socket.on("error", () => socket.destroy());
    socket.on("close", () => { peers.delete(socket); pending.delete(socket); });
    let buffer = "";
    let received = false;
    socket.on("end", () => { if (!received) socket.destroy(); });
    const reply = (request, ok) => {
      pending.delete(socket);
      socket.end(JSON.stringify({ request_id: request.request_id, ok,
        ...(ok ? { session_id: request.session_id } : {}) }) + "\n");
    };
    socket.on("data", data => {
      if (received) { socket.destroy(); return; }
      buffer += data;
      if (Buffer.byteLength(buffer) > 2048) { socket.destroy(); return; }
      if (!buffer.includes("\n")) return;
      received = true;
      let request;
      try { request = JSON.parse(buffer.trim()); } catch { socket.destroy(); return; }
      let recognized = false;
      try { recognized = Boolean(known(request?.session_id)); } catch { /* Native state may be tearing down. */ }
      if (request?.action !== "select_session" || request.incarnation !== incarnation
        || !identifier(request.request_id) || !identifier(request.session_id) || !recognized) {
        if (identifier(request?.request_id)) reply(request, false); else socket.destroy();
        return;
      }
      pending.set(socket, { request, reply });
      try {
        select(request.session_id);
        if (current() === request.session_id && known(request.session_id)) reply(request, true);
      } catch { reply(request, false); }
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(path, () => { server.off("error", reject); resolve(); });
  });
  try { await chmod(path, 0o600); } catch (error) { server.close(); await unlink(path).catch(() => {}); throw error; }
  server.on("error", () => { for (const peer of peers) peer.destroy(); });
  return {
    binding: { kind: "opencode", socket: path, incarnation },
    reconcile() {
      for (const { request, reply } of [...pending.values()]) {
        if (!known(request.session_id)) reply(request, false);
        else if (current() === request.session_id) reply(request, true);
      }
    },
    async close() {
      closed = true;
      for (const peer of peers) peer.destroy();
      await new Promise(resolve => server.close(resolve));
      await unlink(path).catch(() => {});
    },
  };
}

export function createTuiPlugin(options = {}) {
  return async api => {
    // OpenCode's loader installs the host Solid runtime module resolver before
    // importing TUI plugins. A separately bundled Solid copy would be wrong.
    const { createRoot, createEffect, createSignal } = options.reactive ?? await import("solid-js");
    const incarnation = randomUUID();
    const bridge = new PluginBridge(options);
    const sessions = new OpenCodeSessions({ directory: api.state.path.directory, publish: row => bridge.publish(row), incarnation });
    const current = () => api.route.current.name === "session" ? api.route.current.params?.sessionID : null;
    let navigation;
    try {
      navigation = await createNavigation({ incarnation,
        known: id => Boolean(sessions.rows.get(id)?.attached && api.state.session.get(id)),
        select: id => api.route.navigate("session", { sessionID: id }), current,
        ...(options.runtime ? { runtime: options.runtime } : {}),
      });
      sessions.navigation = navigation.binding;
    } catch { /* Observation remains usable when the navigation seam is unavailable. */ }
    let disposeRoot;
    let disposed = false;
    const subscriptions = [];
    const observe = () => {
      if (disposed) return;
      try {
        if (!api.state.ready) return;
        const id = current();
        sessions.selectedId = id;
        const info = id && api.state.session.get(id);
        if (info) {
          if (identifier(info.projectID, 128)) sessions.environment = `opencode:${info.projectID}`;
          const row = sessions.metadata(info);
          if (row) row.attached = true;
        }
        // Only sessions actually selected in this TUI are tracked. The retained
        // reactive reads also observe background activity after switching tabs.
        for (const row of sessions.rows.values()) {
          if (!row.attached) continue;
          const native = api.state.session.get(row.id);
          if (!native) {
            // Cache absence is not an authoritative native session deletion.
            row.phase = "unknown"; row.pending.clear(); row.health = "unknown"; row.nativeUnavailable = true;
            sessions.emit(row); continue;
          }
          row.nativeUnavailable = false;
          if (!row.turn && api.state.session.messages) {
            const messages = api.state.session.messages(row.id);
            for (let i = messages.length - 1; i >= Math.max(0, messages.length - 128); i--) {
              if (messages[i].role === "user") { sessions.userTurn(messages[i], false); break; }
            }
          }
          sessions.snapshot(native, api.state.session.status(row.id),
            api.state.session.permission(row.id), api.state.session.question(row.id), { attached: true });
        }
        navigation?.reconcile();
      } catch { sessions.degrade(); }
    };
    const [wake, notify] = createSignal(0);
    // Errors are native lifecycle metadata but are absent from the TUI's
    // idle/busy/retry status union. Keep the outcome until the next busy boundary.
    if (api.event?.on) {
      for (const type of ["session.error", "session.status", "session.created", "session.deleted", "message.updated"]) {
        subscriptions.push(api.event.on(type, event => {
          if (disposed) return;
          try {
            const properties = event?.properties;
            const id = type.startsWith("session.") && properties?.info
              ? properties.info.id : properties?.info?.sessionID ?? properties?.sessionID;
            if (type === "session.created") {
              if (!sessions.rows.get(properties?.info?.parentID)?.attached) return;
              const child = sessions.metadata(properties.info);
              if (!child) return;
              child.attached = true;
            } else if (!sessions.rows.get(id)?.attached) return;
            sessions.event(event);
            if (type === "session.deleted") sessions.rows.get(id).attached = false;
            if (type === "session.created") notify(value => value + 1);
          } catch { sessions.degrade(); }
        }));
      }
    }
    bridge.onReady = () => notify(value => value + 1);
    createRoot(dispose => {
      disposeRoot = dispose;
      createEffect(() => { wake(); observe(); });
    });
    api.lifecycle.onDispose(async () => {
      disposed = true; bridge.onReady = () => {};
      for (const unsubscribe of subscriptions) unsubscribe();
      disposeRoot?.(); sessions.disconnect(); await navigation?.close(); await bridge.close();
    });
  };
}
