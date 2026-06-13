/**
 * Force simulation (logic/forcegraph.ts) — the pure velocity-Verlet integrator
 * behind the semantic topic-graph lens (DESIGN-SEMANTIC-GRAPH.md). Deterministic
 * given the same inputs, so a fixture lays out the SAME way every run: the tests
 * pin convergence + the semantic property that an image is pulled toward the
 * topic it most relates to.
 */
import { describe, expect, it } from "vitest";
import {
  ringAnchors,
  seedNodes,
  simulate,
  step,
  type ForceConfig,
} from "../src/lib/logic/forcegraph";

const config: ForceConfig = {
  attraction: 0.05,
  repulsion: 400,
  damping: 0.85,
  centering: 0.01,
  ringRadius: 300,
};

describe("ringAnchors", () => {
  it("places topics evenly on a ring, first at the top", () => {
    const a = ringAnchors(4, 300);
    expect(a).toHaveLength(4);
    // First anchor at the top (angle -90deg): x ~ 0, y ~ -radius.
    expect(Math.abs(a[0].x)).toBeLessThan(1e-6);
    expect(a[0].y).toBeCloseTo(-300, 5);
    // Topic indices are stable 0..n.
    expect(a.map((x) => x.topic)).toEqual([0, 1, 2, 3]);
    // Every anchor sits at the ring radius.
    for (const anchor of a) {
      expect(Math.hypot(anchor.x, anchor.y)).toBeCloseTo(300, 4);
    }
  });

  it("yields no anchors for zero topics", () => {
    expect(ringAnchors(0, 300)).toEqual([]);
  });
});

describe("step / simulate convergence", () => {
  it("a fixture lays out deterministically and reaches rest", () => {
    const hashes = ["a", "b", "c", "d", "e", "f"];
    const aff = new Map<string, number[]>([
      // a/b love topic 0; c/d love topic 1; e/f love topic 2.
      ["a", [1, 0, 0]],
      ["b", [1, 0, 0]],
      ["c", [0, 1, 0]],
      ["d", [0, 1, 0]],
      ["e", [0, 0, 1]],
      ["f", [0, 0, 1]],
    ]);
    const anchors = ringAnchors(3, config.ringRadius);

    const run = () => {
      const nodes = seedNodes(hashes, aff, 3);
      const steps = simulate(nodes, anchors, config, 2000, 1e-4);
      return { nodes, steps };
    };
    const first = run();
    const second = run();

    // Deterministic: identical inputs -> identical final positions.
    expect(first.steps).toBe(second.steps);
    for (let i = 0; i < hashes.length; i++) {
      expect(first.nodes[i].x).toBeCloseTo(second.nodes[i].x, 8);
      expect(first.nodes[i].y).toBeCloseTo(second.nodes[i].y, 8);
    }
    // It actually settled (did not hit the step cap).
    expect(first.steps).toBeLessThan(2000);

    // Semantic property: each image ends up NEAREST to the anchor of the topic
    // it most relates to.
    const nearestTopic = (nx: number, ny: number) => {
      let best = -1;
      let bestD = Infinity;
      for (const an of anchors) {
        const d = (an.x - nx) ** 2 + (an.y - ny) ** 2;
        if (d < bestD) {
          bestD = d;
          best = an.topic;
        }
      }
      return best;
    };
    const byHash = (h: string) => first.nodes.find((n) => n.hash === h)!;
    expect(nearestTopic(byHash("a").x, byHash("a").y)).toBe(0);
    expect(nearestTopic(byHash("c").x, byHash("c").y)).toBe(1);
    expect(nearestTopic(byHash("e").x, byHash("e").y)).toBe(2);
  });

  it("an image relating to two topics floats BETWEEN them", () => {
    // One bridge image with equal affinity to topics 0 and 1 should settle on
    // the side of the ring between those two anchors, not at a third.
    const anchors = ringAnchors(3, config.ringRadius);
    const nodes = seedNodes(["bridge"], new Map([["bridge", [1, 1, 0]]]), 3);
    simulate(nodes, anchors, config, 2000, 1e-4);
    const b = nodes[0];
    const d0 = Math.hypot(anchors[0].x - b.x, anchors[0].y - b.y);
    const d1 = Math.hypot(anchors[1].x - b.x, anchors[1].y - b.y);
    const d2 = Math.hypot(anchors[2].x - b.x, anchors[2].y - b.y);
    // Roughly equidistant to 0 and 1, and clearly farther from the unrelated 2.
    expect(Math.abs(d0 - d1)).toBeLessThan(d2 * 0.25);
    expect(d2).toBeGreaterThan(d0);
  });

  it("a dragged (fixed) node holds its position", () => {
    const anchors = ringAnchors(1, config.ringRadius);
    const nodes = seedNodes(["x"], new Map([["x", [1]]]), 1);
    nodes[0].x = 50;
    nodes[0].y = -50;
    nodes[0].fixed = true;
    step(nodes, anchors, config);
    expect(nodes[0].x).toBe(50);
    expect(nodes[0].y).toBe(-50);
    expect(nodes[0].vx).toBe(0);
  });

  it("with no topics, images do not blow up (centering holds them)", () => {
    const nodes = seedNodes(["a", "b", "c"], new Map(), 0);
    simulate(nodes, [], config, 500, 1e-4);
    for (const n of nodes) {
      expect(Number.isFinite(n.x)).toBe(true);
      expect(Math.hypot(n.x, n.y)).toBeLessThan(config.ringRadius);
    }
  });
});

describe("seedNodes", () => {
  it("is deterministic and pads/truncates affinity rows to the topic count", () => {
    const a = seedNodes(["h"], new Map([["h", [0.5]]]), 3);
    const b = seedNodes(["h"], new Map([["h", [0.5]]]), 3);
    expect(a[0].x).toBe(b[0].x);
    // [0.5] padded to width 3.
    expect(a[0].affinity).toEqual([0.5, 0, 0]);
    // A missing hash seeds an all-zero row of the right width.
    const c = seedNodes(["missing"], new Map(), 2);
    expect(c[0].affinity).toEqual([0, 0]);
  });
});
