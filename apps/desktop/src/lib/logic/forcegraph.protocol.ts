/**
 * The message protocol between TopicGraph.svelte (main thread) and the
 * force-sim Web Worker (PLAN-PERF.md P3). Two message kinds in each direction;
 * the split mirrors the data-lifetime split that makes the worker cheap:
 *
 *   main -> worker
 *     - "static": the rarely-changing inputs (per-node affinity rows + mass,
 *       per-anchor topic index, the ForceConfig). Sent ONLY when topics/scope/
 *       config change, NOT every frame, so the per-frame envelope stays tiny.
 *     - "compute": the per-frame request - the TRANSFERABLE mutable buffer
 *       (x/y/vx/vy of every body) + the flag lane + the heat + sub-step count.
 *       The worker integrates over it and returns it.
 *
 *   worker -> main
 *     - "ready": the worker module loaded (so the main thread knows the worker
 *       path is live; the fallback never sees this).
 *     - "stepped": the integrated mutable buffer (transferred back) + the total
 *       kinetic energy, so the main thread updates positions, draws, and decides
 *       whether to keep ticking (idle-when-stable).
 *
 * Static (no DOM) so it is shared by both threads and unit-testable. The actual
 * Float64Array/Uint8Array payloads travel as transferables; everything else is
 * a small structured-clone-able envelope.
 */

import type { ForceConfig } from "./forcegraph";

/** The static per-node inputs the worker needs to rebuild its mirror node
 * objects: the affinity row and the (optional) mass. Order matches the mutable
 * buffer's node order. */
export interface StaticNode {
  affinity: number[];
  mass?: number;
}

/** The static per-anchor input: just the topic index (the affinity-row column
 * it pulls from). Order matches the mutable buffer's anchor order. */
export interface StaticAnchor {
  topic: number;
}

/** main -> worker: resync the rarely-changing inputs. Sent on a topic/scope/
 * config change (and once at startup), never per frame. */
export interface StaticMessage {
  kind: "static";
  nodes: StaticNode[];
  anchors: StaticAnchor[];
  config: ForceConfig;
}

/** main -> worker: integrate `subSteps` steps at this `heat` over the
 * transferred `buffer` (mutable x/y/vx/vy) honoring the `flags` lane (per-frame
 * fixed/pinned). The buffer + flags are TRANSFERRED (their backing memory moves
 * to the worker), so the main thread must not touch them until they return. */
export interface ComputeMessage {
  kind: "compute";
  buffer: Float64Array;
  flags: Uint8Array;
  heat: number;
  subSteps: number;
}

export type MainToWorker = StaticMessage | ComputeMessage;

/** worker -> main: the worker module is live. */
export interface ReadyMessage {
  kind: "ready";
}

/** worker -> main: the integrated buffer (transferred back) + the post-step
 * kinetic energy. The flags lane rides back too so the main thread reuses the
 * same backing memory next frame (avoids re-allocating it every frame). */
export interface SteppedMessage {
  kind: "stepped";
  buffer: Float64Array;
  flags: Uint8Array;
  energy: number;
}

export type WorkerToMain = ReadyMessage | SteppedMessage;
