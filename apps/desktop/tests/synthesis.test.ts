/**
 * Heatmap x graph synthesis (logic/synthesis.ts) — the PURE aggregation/ranking
 * math behind the Attention overlay (DESIGN-SEMANTIC-GRAPH.md + DESIGN-ATTENTION-
 * HEATMAP.md). The component is a thin renderer over these; here we pin the
 * Engaged per-topic aggregation, the Overlooked coherence-vs-attention ranking,
 * the super-node MEAN intensity (LOD composition), and the edge cases.
 */
import { describe, expect, it } from "vitest";
import type { ImageNode } from "../src/lib/logic/forcegraph";
import {
  engagedTopics,
  nodeIntensity,
  nodeOverlay,
  overlookedTopics,
} from "../src/lib/logic/synthesis";

/** A bare single-image node with a given affinity row (position/velocity are
 * irrelevant to the synthesis math — it reads affinity + intensity only). */
function node(hash: string, affinity: number[], extra: Partial<ImageNode> = {}): ImageNode {
  return { hash, x: 0, y: 0, vx: 0, vy: 0, affinity, ...extra };
}

describe("nodeIntensity (LOD composition)", () => {
  it("a single image reads its direct intensity (missing = 0)", () => {
    const intensity = new Map([["a", 0.7]]);
    expect(nodeIntensity(node("a", [1]), intensity)).toBeCloseTo(0.7);
    // No entry → 0 (the heatmap's honest 'no attention' for an un-dwelt image).
    expect(nodeIntensity(node("b", [1]), intensity)).toBe(0);
  });

  it("a super-node reads its members' MEAN intensity, never their sum", () => {
    const intensity = new Map([
      ["a", 0.2],
      ["b", 0.8],
      ["c", 0.5],
    ]);
    const sup = node("super:0", [1], { members: ["a", "b", "c"], mass: 3 });
    // mean(0.2, 0.8, 0.5) = 0.5, NOT the sum 1.5.
    expect(nodeIntensity(sup, intensity)).toBeCloseTo(0.5);
  });

  it("an empty super-node is 0 (no members to average)", () => {
    const sup = node("super:0", [1], { members: [], mass: 0 });
    expect(nodeIntensity(sup, new Map())).toBe(0);
  });
});

describe("engagedTopics — per-topic aggregate attention", () => {
  it("pours each image's heat into the topic it is pulled toward, weighted by affinity", () => {
    // Two topics. Image a is hot + tied to topic 0; image b is cold + tied to
    // topic 1. Topic 0 should rank first (more attention lives there).
    const nodes = [node("a", [1.0, 0.0]), node("b", [0.0, 1.0])];
    const intensity = new Map([
      ["a", 1.0],
      ["b", 0.0],
    ]);
    const ranked = engagedTopics(nodes, intensity, 2);
    expect(ranked[0].topic).toBe(0);
    expect(ranked[0].score).toBeCloseTo(1); // normalized max
    expect(ranked[1].topic).toBe(1);
    expect(ranked[1].score).toBeCloseTo(0); // cold topic carries no attention
  });

  it("a bridge image splits its heat across both topics by affinity", () => {
    // One hot image related to BOTH topics, more to topic 0. Topic 0 gets more
    // of its attention.
    const nodes = [node("a", [0.9, 0.3])];
    const intensity = new Map([["a", 1.0]]);
    const ranked = engagedTopics(nodes, intensity, 2);
    const t0 = ranked.find((r) => r.topic === 0)!;
    const t1 = ranked.find((r) => r.topic === 1)!;
    expect(t0.score).toBeGreaterThan(t1.score);
    expect(ranked[0].topic).toBe(0);
  });

  it("a super-node contributes its mean intensity scaled by mass", () => {
    // A super-node of 4 images, mean intensity 0.5, all tied to topic 0, should
    // out-weigh a single hot image tied to topic 1 of equal per-image heat.
    const intensity = new Map([
      ["m1", 0.5],
      ["m2", 0.5],
      ["m3", 0.5],
      ["m4", 0.5],
      ["b", 0.5],
    ]);
    const nodes = [
      node("super:0", [1, 0], { members: ["m1", "m2", "m3", "m4"], mass: 4 }),
      node("b", [0, 1]),
    ];
    const ranked = engagedTopics(nodes, intensity, 2);
    expect(ranked[0].topic).toBe(0); // the mass-4 cluster carries more attention
    expect(ranked[0].score).toBeCloseTo(1);
  });

  it("all-cold scope yields every topic at score 0 (well-formed, never NaN)", () => {
    const nodes = [node("a", [1, 0]), node("b", [0, 1])];
    const ranked = engagedTopics(nodes, new Map(), 2);
    expect(ranked).toHaveLength(2);
    for (const r of ranked) {
      expect(r.score).toBe(0);
      expect(Number.isNaN(r.score)).toBe(false);
      expect(Number.isNaN(r.mean)).toBe(false);
    }
  });

  it("empty scope yields one zero row per topic (dense, stable)", () => {
    const ranked = engagedTopics([], new Map(), 3);
    expect(ranked).toHaveLength(3);
    expect(ranked.every((r) => r.score === 0 && r.mean === 0)).toBe(true);
  });
});

describe("overlookedTopics — coherent-but-cold ranking (the novel inverse)", () => {
  it("ranks a coherent COLD topic above a coherent HOT one", () => {
    // Topic 0: a tight cluster (high affinity) that is COLD → overlooked.
    // Topic 1: an equally tight cluster that is HOT → engaged, not overlooked.
    const nodes = [
      node("a", [1, 0]),
      node("b", [1, 0]),
      node("c", [0, 1]),
      node("d", [0, 1]),
    ];
    const intensity = new Map([
      ["a", 0.0],
      ["b", 0.0],
      ["c", 1.0],
      ["d", 1.0],
    ]);
    const ranked = overlookedTopics(nodes, intensity, 2);
    expect(ranked[0].topic).toBe(0); // the cold-but-coherent body of work wins
    expect(ranked[0].score).toBeGreaterThan(ranked[1].score);
  });

  it("a topic nothing relates to scores 0 (absent, not overlooked)", () => {
    // Topic 1 has no affiliated images: it is not an overlooked BODY OF WORK.
    const nodes = [node("a", [1, 0]), node("b", [1, 0])];
    const intensity = new Map([
      ["a", 0],
      ["b", 0],
    ]);
    const ranked = overlookedTopics(nodes, intensity, 2);
    const t1 = ranked.find((r) => r.topic === 1)!;
    expect(t1.score).toBe(0);
  });

  it("a loose (incoherent) cold topic ranks below a tight cold one", () => {
    // Both cold. Topic 0 is tight (strong affinity); topic 1 is loose (weak
    // affinity). The TIGHT one is the real overlooked body of work.
    const nodes = [
      node("a", [1.0, 0.0]),
      node("b", [1.0, 0.0]),
      node("c", [0.0, 0.15]),
      node("d", [0.0, 0.15]),
    ];
    const intensity = new Map(); // everything cold
    const ranked = overlookedTopics(nodes, intensity, 2);
    expect(ranked[0].topic).toBe(0);
    expect(ranked[0].score).toBeGreaterThan(ranked[1].score);
  });

  it("empty / all-zero scope yields all-zero scores, never NaN", () => {
    const empty = overlookedTopics([], new Map(), 2);
    expect(empty.every((r) => r.score === 0)).toBe(true);
    const cold = overlookedTopics([node("a", [0, 0])], new Map(), 2);
    expect(cold.every((r) => r.score === 0 && !Number.isNaN(r.score))).toBe(true);
  });
});

describe("nodeOverlay — per-node tint/size mapping", () => {
  it("OFF leaves the node untouched (no glow, base size)", () => {
    const o = nodeOverlay(node("a", [1]), new Map([["a", 0.9]]), "off");
    expect(o.glow).toBe(0);
    expect(o.sizeScale).toBe(1);
    // intensity is still reported (the renderer may read it), just not applied.
    expect(o.intensity).toBeCloseTo(0.9);
  });

  it("ENGAGED glows + grows a hot node, leaves a cold node at base size", () => {
    const hot = nodeOverlay(node("a", [1]), new Map([["a", 1]]), "engaged");
    expect(hot.glow).toBeCloseTo(1);
    expect(hot.sizeScale).toBeGreaterThan(1);
    const cold = nodeOverlay(node("b", [1]), new Map(), "engaged");
    expect(cold.glow).toBe(0);
    expect(cold.sizeScale).toBeCloseTo(1);
  });

  it("OVERLOOKED glows a coherent-cold node and dims a hot one", () => {
    // Coherent (dom affinity 1) + cold → glows. The topic-level overlooked score
    // is supplied as 1 so the node inherits its cluster's overlooked-ness.
    const overlooked = nodeOverlay(
      node("a", [1, 0]),
      new Map(),
      "overlooked",
      [1, 0],
    );
    expect(overlooked.glow).toBeGreaterThan(0.5);
    // A HOT coherent node in the same topic does NOT glow (it is engaged).
    const hot = nodeOverlay(
      node("b", [1, 0]),
      new Map([["b", 1]]),
      "overlooked",
      [1, 0],
    );
    expect(hot.glow).toBeCloseTo(0);
    // The non-glowing node recedes toward the dim floor (< base size).
    expect(hot.sizeScale).toBeLessThan(1);
  });

  it("a super-node's overlay reads its members' mean intensity", () => {
    const intensity = new Map([
      ["m1", 1],
      ["m2", 1],
    ]);
    const sup = node("super:0", [1], { members: ["m1", "m2"], mass: 2 });
    const o = nodeOverlay(sup, intensity, "engaged");
    expect(o.intensity).toBeCloseTo(1);
    expect(o.glow).toBeCloseTo(1);
  });
});
