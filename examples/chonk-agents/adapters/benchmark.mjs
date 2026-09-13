// Deterministic adapter-only overhead. No native provider or compositor requests.
import { performance } from "node:perf_hooks";
import { EventEmitter } from "node:events";
import { OpenCodeSessions } from "./opencode.mjs";
import { MetadataWriter } from "./common.mjs";
import { T3Sessions } from "./t3.mjs";

const iterations = 100_000;
const measure = (name, callback) => {
  for (let i = 0; i < 5000; i++) callback(i);
  const samples = [];
  for (let round = 0; round < 5; round++) {
    const cpu = process.cpuUsage(); const start = performance.now();
    for (let i = 0; i < iterations; i++) callback(i);
    const elapsed = performance.now() - start; const used = process.cpuUsage(cpu);
    samples.push({ ns_per_callback: elapsed * 1e6 / iterations, cpu_us: used.user + used.system });
  }
  return { name, iterations, samples };
};
let emitted = 0;
const code = new OpenCodeSessions({ directory: "/fixture", publish: () => { emitted++; } });
const info = { id: "session", projectID: "project", directory: "/fixture", title: "Fixture" };
const ignored = { type: "message.part.updated", properties: { text: "not inspected" } };
const results = [];
results.push(measure("ignored_token_event", () => code.event(ignored)));
const ignoredOutput = emitted;
results.push(measure("unchanged_tui_snapshot", () => code.snapshot(info, { type: "idle" }, [], [], { attached: true })));
const idleOutput = emitted;
results.push(measure("semantic_status_transition", i => code.event({ type: "session.status", properties: {
  sessionID: "session", status: { type: i % 2 ? "busy" : "idle" },
} })));
class Sink extends EventEmitter { writes = 0; write() { this.writes++; return false; } }
const sink = new Sink(); const writer = new MetadataWriter(sink);
results.push(measure("blocked_32_session_latest_queue", i => writer.push({ schema: 2, engine: "opencode", environment: "fixture",
  session_id: `s${i % 32}`, phase: i % 2 ? "idle" : "working", sequence: i })));
let t3Output = 0;
const t3 = new T3Sessions({ environment: "fixture", publish: () => { t3Output++; } });
t3.project({ id: "project", workspaceRoot: "/fixture" }); t3.item({ kind: "synchronized" });
const thread = { id: "thread", projectId: "project", title: "Fixture", session: { providerName: "codex", status: "ready" } };
results.push(measure("unchanged_t3_thread_metadata", () => t3.thread(thread)));
const beforeIdleOutput = emitted + t3Output;
const beforeIdle = process.cpuUsage(); const idleStart = performance.now();
await new Promise(resolve => setTimeout(resolve, 2000));
const idleCpu = process.cpuUsage(beforeIdle);
console.log(JSON.stringify({ node: process.version, results,
  assertions: { ignored_output: ignoredOutput, unchanged_tui_output: idleOutput,
    blocked_sink_writes: sink.writes, queued_latest: writer.pending.size, unchanged_t3_output: t3Output,
    retained_opencode_rows: code.rows.size, retained_t3_rows: t3.rows.size },
  idle: { elapsed_ms: performance.now() - idleStart, cpu_us: idleCpu.user + idleCpu.system,
    semantic_output_delta: emitted + t3Output - beforeIdleOutput }, max_rss_kib: process.resourceUsage().maxRSS,
  scope: "Node metadata callbacks only; no provider, broker or compositor performance claim" }, null, 2));
writer.close();
