/**
 * SoA pack/unpack for the force-sim Web Worker (PLAN-PERF.md P3). The worker
 * moves the O(N^2) compute off the UI main thread by shipping the MUTABLE
 * per-frame state (x/y/vx/vy per body) as a TRANSFERABLE Float64Array; the
 * buffer layout is its own pure seam so we can pin that positions/velocities
 * round-trip EXACTLY (pack -> unpack is the identity), and that the per-frame
 * fixed/pinned flag lane (the drag path flips it live) round-trips too.
 */
import { describe, expect, it } from "vitest";
import type { ImageNode, TopicAnchor } from "../src/lib/logic/forcegraph";
import {
  bodyCount,
  FLAG_FIXED,
  FLAG_PINNED,
  makeMutableBuffer,
  MUT_STRIDE,
  packFlags,
  packMutable,
  unpackFlags,
  unpackMutable,
} from "../src/lib/logic/forcegraph.soa";

function makeNodes(): ImageNode[] {
  return [
    { hash: "a", x: 1.5, y: -2.25, vx: 0.125, vy: -0.0625, affinity: [1, 0] },
    { hash: "b", x: -100.5, y: 42.75, vx: -3.5, vy: 7.0, affinity: [0, 1], mass: 5 },
  ];
}
function makeAnchors(): TopicAnchor[] {
  return [
    { topic: 0, x: 0, y: -300, vx: 0.5, vy: -0.5 },
    { topic: 1, x: 260, y: 150, vx: -1.25, vy: 2.5 },
  ];
}

describe("SoA mutable buffer round-trip", () => {
  it("sizes the buffer to (nodes + anchors) * stride", () => {
    expect(bodyCount(3, 2)).toBe(5);
    expect(makeMutableBuffer(3, 2).length).toBe(5 * MUT_STRIDE);
  });

  it("pack -> unpack is the identity for every position and velocity", () => {
    const nodes = makeNodes();
    const anchors = makeAnchors();
    const buf = makeMutableBuffer(nodes.length, anchors.length);
    packMutable(buf, nodes, anchors);

    // Unpack onto FRESH objects (same static fields, zeroed mutable lanes) and
    // assert they recover exactly what we packed - Float64 is lossless for these
    // values, so no `toBeCloseTo` slack is needed.
    const nodes2: ImageNode[] = nodes.map((n) => ({
      ...n,
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
    }));
    const anchors2: TopicAnchor[] = anchors.map((a) => ({
      ...a,
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
    }));
    unpackMutable(buf, nodes2, anchors2);

    for (let i = 0; i < nodes.length; i++) {
      expect(nodes2[i].x).toBe(nodes[i].x);
      expect(nodes2[i].y).toBe(nodes[i].y);
      expect(nodes2[i].vx).toBe(nodes[i].vx);
      expect(nodes2[i].vy).toBe(nodes[i].vy);
    }
    for (let a = 0; a < anchors.length; a++) {
      expect(anchors2[a].x).toBe(anchors[a].x);
      expect(anchors2[a].y).toBe(anchors[a].y);
      expect(anchors2[a].vx).toBe(anchors[a].vx);
      expect(anchors2[a].vy).toBe(anchors[a].vy);
    }
  });

  it("lays nodes BEFORE anchors so both sides reconstruct the same order", () => {
    const nodes = makeNodes();
    const anchors = makeAnchors();
    const buf = makeMutableBuffer(nodes.length, anchors.length);
    packMutable(buf, nodes, anchors);
    // First node's x/y are the first two slots; the first anchor's x/y begin
    // right after the last node.
    expect(buf[0]).toBe(nodes[0].x);
    expect(buf[1]).toBe(nodes[0].y);
    const anchorBase = nodes.length * MUT_STRIDE;
    expect(buf[anchorBase]).toBe(anchors[0].x);
    expect(buf[anchorBase + 1]).toBe(anchors[0].y);
  });

  it("treats a missing anchor velocity as 0 (older fixed-ring literal)", () => {
    const nodes: ImageNode[] = [];
    // An anchor without vx/vy (an older literal) must pack as 0, not NaN.
    const anchors: TopicAnchor[] = [{ topic: 0, x: 5, y: 6 }];
    const buf = makeMutableBuffer(0, 1);
    packMutable(buf, nodes, anchors);
    expect(buf[2]).toBe(0); // vx
    expect(buf[3]).toBe(0); // vy
  });
});

describe("SoA per-frame flag lane (drag path)", () => {
  it("packs and round-trips node fixed + anchor fixed/pinned bits", () => {
    const nodes: ImageNode[] = [
      { hash: "a", x: 0, y: 0, vx: 0, vy: 0, affinity: [], fixed: true },
      { hash: "b", x: 0, y: 0, vx: 0, vy: 0, affinity: [] },
    ];
    const anchors: TopicAnchor[] = [
      { topic: 0, x: 0, y: 0, vx: 0, vy: 0, pinned: true },
      { topic: 1, x: 0, y: 0, vx: 0, vy: 0, fixed: true },
    ];
    const flags = new Uint8Array(nodes.length + anchors.length);
    packFlags(flags, nodes, anchors);
    // Node 0 fixed, node 1 clear; anchor 0 pinned, anchor 1 fixed.
    expect(flags[0]).toBe(FLAG_FIXED);
    expect(flags[1]).toBe(0);
    expect(flags[2]).toBe(FLAG_PINNED);
    expect(flags[3]).toBe(FLAG_FIXED);

    // Unpacking onto fresh objects recovers the same flags (the worker applies
    // them to its mirror before stepping, so a grabbed body is held THIS frame).
    const n2: ImageNode[] = nodes.map((n) => ({ ...n, fixed: false }));
    const a2: TopicAnchor[] = anchors.map((a) => ({
      ...a,
      fixed: false,
      pinned: false,
    }));
    unpackFlags(flags, n2, a2);
    expect(n2[0].fixed).toBe(true);
    expect(n2[1].fixed).toBe(false);
    expect(a2[0].pinned).toBe(true);
    expect(a2[0].fixed).toBe(false);
    expect(a2[1].fixed).toBe(true);
    expect(a2[1].pinned).toBe(false);
  });
});
