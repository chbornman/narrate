/**
 * Worker-vs-inline parity (PLAN-PERF.md P3). The Web Worker is a pure
 * performance move: it must produce the SAME layout as the inline main-thread
 * `step` loop, or the off-thread path would silently change the Visualizer. We
 * cannot spin a real Worker in jsdom, but the worker's per-frame body is just
 * `unpackMutable -> step x subSteps -> packMutable` over the SAME pure `step`
 * the inline path calls. This test replicates that body through the SoA seam and
 * asserts it tracks the inline loop frame-for-frame (the buffer is the only
 * thing that crosses the thread boundary, so identical buffers => identical UI).
 *
 * It also exercises the per-frame FLAG lane: a body marked `fixed` (the drag
 * path) is held still by the sim on the worker path exactly as inline.
 */
import { describe, expect, it } from "vitest";
import {
  ringAnchors,
  seedNodes,
  step,
  subStepsForHeat,
  type ForceConfig,
  type ImageNode,
  type TopicAnchor,
} from "../src/lib/logic/forcegraph";
import {
  makeMutableBuffer,
  packFlags,
  packMutable,
  unpackFlags,
  unpackMutable,
} from "../src/lib/logic/forcegraph.soa";

const config: ForceConfig = {
  attraction: 0.05,
  repulsion: 400,
  damping: 0.85,
  centering: 0.01,
  ringRadius: 300,
  anchorAttraction: 0.08,
  anchorRepulsion: 60000,
  anchorDamping: 0.8,
};

/** One "worker frame": apply the transferred buffer + flags onto the worker's
 * mirror, step `subSteps` times at `heat`, pack the result back. Mirrors
 * forcegraph.worker.ts exactly (which imports the same pure `step`). */
function workerFrame(
  buf: Float64Array,
  flags: Uint8Array,
  mirrorNodes: ImageNode[],
  mirrorAnchors: TopicAnchor[],
  heat: number,
  subSteps: number,
): number {
  unpackMutable(buf, mirrorNodes, mirrorAnchors);
  unpackFlags(flags, mirrorNodes, mirrorAnchors);
  const frameCfg: ForceConfig = { ...config, heat };
  let energy = 0;
  for (let s = 0; s < subSteps; s++) {
    energy = step(mirrorNodes, mirrorAnchors, frameCfg);
  }
  packMutable(buf, mirrorNodes, mirrorAnchors);
  return energy;
}

describe("worker path matches the inline step loop", () => {
  it("produces bit-identical positions over many frames", () => {
    const hashes = ["a", "b", "c", "d", "e", "f"];
    const aff = new Map<string, number[]>([
      ["a", [1, 0, 0]],
      ["b", [1, 0, 0]],
      ["c", [0, 1, 0]],
      ["d", [0, 1, 0]],
      ["e", [0, 0, 1]],
      ["f", [0, 0, 1]],
    ]);

    // INLINE reference: the v1 main-thread loop.
    const inlineNodes = seedNodes(hashes, aff, 3);
    const inlineAnchors = ringAnchors(3, config.ringRadius);

    // WORKER path: the live state (what the component keeps) round-tripped
    // through the buffer each frame, integrated by the worker mirror.
    const liveNodes = seedNodes(hashes, aff, 3);
    const liveAnchors = ringAnchors(3, config.ringRadius);
    // The worker's mirror objects (rebuilt once from the static message; their
    // x/y/vx/vy come from the buffer each frame). Same affinity/topic.
    const mirrorNodes: ImageNode[] = liveNodes.map((n) => ({
      hash: "",
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: n.affinity,
      mass: n.mass,
    }));
    const mirrorAnchors: TopicAnchor[] = liveAnchors.map((a) => ({
      topic: a.topic,
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
    }));
    const buf = makeMutableBuffer(liveNodes.length, liveAnchors.length);
    const flags = new Uint8Array(liveNodes.length + liveAnchors.length);

    // Use a fixed heat schedule for both paths so the comparison is exact.
    let heat = 4; // a hot-ish frame so subSteps > 1 is exercised too
    for (let frame = 0; frame < 120; frame++) {
      const subSteps = subStepsForHeat(heat);
      // Inline: step subSteps times.
      for (let s = 0; s < subSteps; s++) {
        step(inlineNodes, inlineAnchors, { ...config, heat });
      }
      // Worker: pack live -> compute -> unpack back onto live (what the
      // component does each frame).
      packMutable(buf, liveNodes, liveAnchors);
      packFlags(flags, liveNodes, liveAnchors);
      workerFrame(buf, flags, mirrorNodes, mirrorAnchors, heat, subSteps);
      unpackMutable(buf, liveNodes, liveAnchors);
      heat = Math.max(1, heat * 0.9);
    }

    for (let i = 0; i < liveNodes.length; i++) {
      expect(liveNodes[i].x).toBe(inlineNodes[i].x);
      expect(liveNodes[i].y).toBe(inlineNodes[i].y);
      expect(liveNodes[i].vx).toBe(inlineNodes[i].vx);
      expect(liveNodes[i].vy).toBe(inlineNodes[i].vy);
    }
    for (let a = 0; a < liveAnchors.length; a++) {
      expect(liveAnchors[a].x).toBe(inlineAnchors[a].x);
      expect(liveAnchors[a].y).toBe(inlineAnchors[a].y);
    }
  });

  it("honors a per-frame fixed flag (a dragged body is held still)", () => {
    // One node marked fixed via the FLAG lane (the drag path) must not move on
    // the worker path - the worker applies the flag to its mirror before stepping.
    const live: ImageNode[] = [
      { hash: "held", x: 50, y: -50, vx: 0, vy: 0, affinity: [1] },
      { hash: "free", x: 10, y: 10, vx: 0, vy: 0, affinity: [1] },
    ];
    const anchors = ringAnchors(1, config.ringRadius);
    const mirror: ImageNode[] = live.map((n) => ({
      hash: "",
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: n.affinity,
    }));
    const mirrorAnchors: TopicAnchor[] = anchors.map((a) => ({
      topic: a.topic,
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
    }));
    const buf = makeMutableBuffer(live.length, anchors.length);
    const flags = new Uint8Array(live.length + anchors.length);

    live[0].fixed = true; // the drag path holds node 0
    for (let frame = 0; frame < 20; frame++) {
      packMutable(buf, live, anchors);
      packFlags(flags, live, anchors);
      workerFrame(buf, flags, mirror, mirrorAnchors, 1, 1);
      unpackMutable(buf, live, anchors);
    }
    // The held node never moved; the free one did.
    expect(live[0].x).toBe(50);
    expect(live[0].y).toBe(-50);
    expect(live[1].x).not.toBe(10);
  });
});
