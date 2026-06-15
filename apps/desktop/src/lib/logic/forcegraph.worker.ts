/**
 * Force-sim Web Worker (PLAN-PERF.md P3). The sim's O(N^2) image-image
 * repulsion is the heavy per-frame compute; running it HERE keeps the UI main
 * thread free for Canvas rendering + pointer input, the "buttery smooth"
 * Visualizer goal. The worker owns NO state model of its own beyond a MIRROR of
 * the live nodes/anchors: the authoritative state stays in the component, and
 * only the COMPUTE moves here.
 *
 * It IMPORTS the pure `step` from forcegraph.ts (the unit-tested integrator is
 * unchanged - the worker is a thin off-thread host for it). Each frame the main
 * thread transfers the mutable x/y/vx/vy buffer + the per-frame flag lane; the
 * worker writes those onto its mirror objects, runs `step` `subSteps` times at
 * the given heat, packs the result back, and transfers the buffer + energy home.
 *
 * The STATIC inputs (affinity rows, mass, anchor topics, the ForceConfig) arrive
 * on a separate "static" message sent only when they change (a topic add, a
 * config change), so the per-frame envelope is just the two typed arrays + two
 * numbers - no re-sending the affinity matrix every frame.
 */

import { step, type ForceConfig, type ImageNode, type TopicAnchor } from "./forcegraph";
import { unpackFlags, unpackMutable, packMutable } from "./forcegraph.soa";
import type { MainToWorker, ReadyMessage, SteppedMessage } from "./forcegraph.protocol";

// The worker's MIRROR of the live state. Rebuilt wholesale on a "static"
// message; its x/y/vx/vy are overwritten from the transferred buffer each
// compute, and its fixed/pinned flags from the transferred flag lane. The
// affinity/mass/topic stay as the static message set them.
let nodes: ImageNode[] = [];
let anchors: TopicAnchor[] = [];
let config: ForceConfig = {
  attraction: 0.02,
  repulsion: 800,
  damping: 0.85,
  centering: 0.01,
  ringRadius: 320,
  anchorAttraction: 0.08,
  anchorRepulsion: 60000,
  anchorDamping: 0.8,
  anchorCentering: 0.01,
};

self.onmessage = (ev: MessageEvent<MainToWorker>) => {
  const msg = ev.data;
  if (msg.kind === "static") {
    // Rebuild the mirror objects from the rarely-changing inputs. Positions are
    // placeholders here (0); the very next compute message overwrites them from
    // the transferred buffer, so the static message never needs to carry them.
    config = msg.config;
    nodes = msg.nodes.map((s) => ({
      hash: "",
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: s.affinity,
      mass: s.mass,
      // Carry the static SEMANTIC-NEIGHBOR edges onto the mirror node so the
      // worker's `step` applies the same similarity spring as the inline path
      // (index-based, so no hash lookup is needed here).
      neighbors: s.neighbors,
    }));
    anchors = msg.anchors.map((s) => ({
      topic: s.topic,
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
    }));
    return;
  }
  // A compute request: write the transferred mutable state + flags onto the
  // mirror, integrate, pack back, transfer home with the energy.
  const { buffer, flags, heat, subSteps } = msg;
  unpackMutable(buffer, nodes, anchors);
  unpackFlags(flags, nodes, anchors);
  // Heat is per-frame (the caller cools it), so fold it into the config copy the
  // sim reads this frame - the static config carries the steady-state attraction.
  const frameConfig: ForceConfig = { ...config, heat };
  let energy = 0;
  for (let s = 0; s < subSteps; s++) {
    energy = step(nodes, anchors, frameConfig);
  }
  packMutable(buffer, nodes, anchors);
  const reply: SteppedMessage = { kind: "stepped", buffer, flags, energy };
  // Transfer the buffer + flags BACK so the main thread reuses the same backing
  // memory next frame (no per-frame allocation on either side).
  (self as unknown as Worker).postMessage(reply, [buffer.buffer, flags.buffer]);
};

// Tell the main thread the worker module is live (the fallback never sees this).
const ready: ReadyMessage = { kind: "ready" };
(self as unknown as Worker).postMessage(ready);
