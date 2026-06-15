/**
 * One-shot deterministic layout (logic/layout.ts) — the Stage-2 replacement for
 * the live force sim (visualizer audit, June 2026). Pure, so the semantic-map
 * property (affinity-weighted centroid) + the declump + determinism pin without
 * a DOM or a physics loop.
 */
import { describe, expect, it } from "vitest";
import { computeStaticLayout } from "../src/lib/logic/layout";
import { ringAnchors, type ImageNode } from "../src/lib/logic/forcegraph";

const RING = 320;

function node(hash: string, affinity: number[]): ImageNode {
  return { hash, x: 0, y: 0, vx: 0, vy: 0, affinity };
}

function nearestAnchor(n: ImageNode, anchors: ReturnType<typeof ringAnchors>) {
  let best = -1;
  let bestD = Infinity;
  for (const a of anchors) {
    const d = (a.x - n.x) ** 2 + (a.y - n.y) ** 2;
    if (d < bestD) {
      bestD = d;
      best = a.topic;
    }
  }
  return best;
}

describe("computeStaticLayout — the semantic-map property", () => {
  it("an image strong for one topic lands nearest that anchor", () => {
    const anchors = ringAnchors(3, RING);
    const nodes = [node("a", [1, 0, 0]), node("c", [0, 0, 1])];
    computeStaticLayout(nodes, anchors, { ringRadius: RING });
    expect(nearestAnchor(nodes[0], anchors)).toBe(0);
    expect(nearestAnchor(nodes[1], anchors)).toBe(2);
  });

  it("an image between two topics lands BETWEEN them (weighted centroid)", () => {
    const anchors = ringAnchors(3, RING);
    const nodes = [node("bridge", [1, 1, 0])];
    computeStaticLayout(nodes, anchors, { ringRadius: RING });
    const b = nodes[0];
    const d0 = Math.hypot(anchors[0].x - b.x, anchors[0].y - b.y);
    const d1 = Math.hypot(anchors[1].x - b.x, anchors[1].y - b.y);
    const d2 = Math.hypot(anchors[2].x - b.x, anchors[2].y - b.y);
    expect(Math.abs(d0 - d1)).toBeLessThan(1e-6); // equidistant to 0 and 1
    expect(d2).toBeGreaterThan(d0); // farther from the unrelated topic
    // The centroid of two ring anchors sits INSIDE the ring.
    expect(Math.hypot(b.x, b.y)).toBeLessThan(RING);
  });

  it("a lone image sits EXACTLY on its weighted centroid (no declump offset)", () => {
    const anchors = ringAnchors(2, RING);
    const nodes = [node("solo", [1, 0])];
    computeStaticLayout(nodes, anchors, { ringRadius: RING });
    expect(nodes[0].x).toBeCloseTo(anchors[0].x, 9);
    expect(nodes[0].y).toBeCloseTo(anchors[0].y, 9);
  });
});

describe("computeStaticLayout — declump + stability", () => {
  it("identical-affinity images get DISTINCT positions, all near the shared centroid", () => {
    const anchors = ringAnchors(2, RING);
    const nodes = Array.from({ length: 12 }, (_, i) => node("n" + i, [1, 0]));
    computeStaticLayout(nodes, anchors, { ringRadius: RING, spread: 20 });
    const keys = new Set(
      nodes.map((n) => `${n.x.toFixed(3)},${n.y.toFixed(3)}`),
    );
    expect(keys.size).toBe(12); // every node got a distinct slot
    // ...but the whole cluster stays in the neighborhood of anchor 0.
    for (const n of nodes) {
      expect(Math.hypot(n.x - anchors[0].x, n.y - anchors[0].y)).toBeLessThan(
        120,
      );
    }
  });

  it("is fully deterministic: same input yields identical positions", () => {
    const make = () =>
      Array.from({ length: 50 }, (_, i) =>
        node("n" + i, [
          i % 3 === 0 ? 1 : 0,
          i % 3 === 1 ? 1 : 0,
          i % 3 === 2 ? 1 : 0,
        ]),
      );
    const a = make();
    const b = make();
    const anchors = ringAnchors(3, RING);
    computeStaticLayout(a, anchors, { ringRadius: RING });
    computeStaticLayout(b, anchors, { ringRadius: RING });
    for (let i = 0; i < a.length; i++) {
      expect(a[i].x).toBe(b[i].x);
      expect(a[i].y).toBe(b[i].y);
    }
  });

  it("zeroes velocity (handed straight to a static renderer, no residual drift)", () => {
    const anchors = ringAnchors(2, RING);
    const nodes = [{ hash: "x", x: 9, y: 9, vx: 5, vy: -5, affinity: [1, 0] }];
    computeStaticLayout(nodes, anchors, { ringRadius: RING });
    expect(nodes[0].vx).toBe(0);
    expect(nodes[0].vy).toBe(0);
  });
});

describe("computeStaticLayout — degenerate inputs", () => {
  it("no topics: images spread around the origin, bounded (never stack at 0,0)", () => {
    const nodes = Array.from({ length: 40 }, (_, i) => node("n" + i, []));
    computeStaticLayout(nodes, [], { ringRadius: RING });
    const keys = new Set(
      nodes.map((n) => `${n.x.toFixed(2)},${n.y.toFixed(2)}`),
    );
    expect(keys.size).toBe(40); // distinct, not all at origin
    for (const n of nodes) {
      expect(Number.isFinite(n.x) && Number.isFinite(n.y)).toBe(true);
      expect(Math.hypot(n.x, n.y)).toBeLessThan(RING * 2);
    }
  });

  it("a zero-affinity image rests at the origin neighborhood, never NaN", () => {
    const anchors = ringAnchors(3, RING);
    const nodes = [node("dead", [0, 0, 0])];
    computeStaticLayout(nodes, anchors, { ringRadius: RING });
    expect(Number.isFinite(nodes[0].x) && Number.isFinite(nodes[0].y)).toBe(
      true,
    );
    expect(Math.hypot(nodes[0].x, nodes[0].y)).toBeLessThan(1e-6); // at origin
  });
});
