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
  matchClusterLabel,
  nodeIntensity,
  nodeOverlay,
  overlookedTopics,
  unnamedClusters,
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

describe("unnamedClusters — coherent clumps with no named topic (soft topics)", () => {
  /** A node positioned at (x,y) with an affinity row + sparse k-NN neighbors. */
  function pn(
    hash: string,
    x: number,
    y: number,
    affinity: number[],
    neighbors: { i: number; w: number }[],
  ): ImageNode {
    return { hash, x, y, vx: 0, vy: 0, affinity, neighbors };
  }

  it("finds two separate unnamed clusters, excludes named nodes, drops tiny groups", () => {
    // One named topic (index 0). Group A (g0,g1,g2) and group B (g3,g4,g5) are
    // UN-topic'd (affinity 0) and densely linked WITHIN each group, NOT across.
    // A named node (n) holds strongly to topic 0 even though it links to group A
    // — it must be excluded and must not bridge anything. A loose pair (p0,p1)
    // is below minSize (2) and must be dropped.
    const W = 0.8; // above the 0.5 minWeight bar
    const nodes: ImageNode[] = [
      pn("g0", 0, 0, [0], [{ i: 1, w: W }, { i: 2, w: W }]),
      pn("g1", 1, 0, [0], [{ i: 0, w: W }, { i: 2, w: W }]),
      pn("g2", 0, 1, [0], [{ i: 0, w: W }, { i: 1, w: W }]),
      pn("g3", 10, 10, [0], [{ i: 4, w: W }, { i: 5, w: W }]),
      pn("g4", 11, 10, [0], [{ i: 3, w: W }, { i: 5, w: W }]),
      pn("g5", 10, 11, [0], [{ i: 3, w: W }, { i: 4, w: W }]),
      // Named node: high affinity to topic 0, linked to group A — excluded.
      pn("n", 0, 0, [0.9], [{ i: 0, w: W }]),
      // A two-node clump (below minSize), kept apart from both groups.
      pn("p0", -10, -10, [0], [{ i: 9, w: W }]),
      pn("p1", -11, -10, [0], [{ i: 8, w: W }]),
    ];
    const clusters = unnamedClusters(nodes, 1);
    expect(clusters).toHaveLength(2);
    // Both groups are size 3; tie-break is the smallest member hash, so the
    // "g0..g2" group sorts before "g3..g5".
    expect(clusters[0].members).toEqual(["g0", "g1", "g2"]);
    expect(clusters[0].size).toBe(3);
    expect(clusters[1].members).toEqual(["g3", "g4", "g5"]);
    expect(clusters[1].size).toBe(3);
    // The named node never appears in any cluster.
    const all = clusters.flatMap((c) => c.members);
    expect(all).not.toContain("n");
    // The two-node clump is dropped (< minSize).
    expect(all).not.toContain("p0");
    expect(all).not.toContain("p1");
  });

  it("computes the centroid and coherence (mean intra-cluster edge weight)", () => {
    // A single triangle clump at known positions with uniform edge weight 0.6.
    const nodes: ImageNode[] = [
      pn("a", 0, 0, [0], [{ i: 1, w: 0.6 }, { i: 2, w: 0.6 }]),
      pn("b", 3, 0, [0], [{ i: 0, w: 0.6 }, { i: 2, w: 0.6 }]),
      pn("c", 0, 3, [0], [{ i: 0, w: 0.6 }, { i: 1, w: 0.6 }]),
    ];
    const [cl] = unnamedClusters(nodes, 1);
    expect(cl.centroidX).toBeCloseTo(1); // mean(0,3,0)
    expect(cl.centroidY).toBeCloseTo(1); // mean(0,0,3)
    expect(cl.coherence).toBeCloseTo(0.6);
  });

  it("ignores edges below minWeight (a weak link does not join a clump)", () => {
    // Three unnamed nodes, but the edges are below the 0.5 floor: no cluster.
    const nodes: ImageNode[] = [
      pn("a", 0, 0, [0], [{ i: 1, w: 0.3 }, { i: 2, w: 0.3 }]),
      pn("b", 1, 0, [0], [{ i: 0, w: 0.3 }, { i: 2, w: 0.3 }]),
      pn("c", 0, 1, [0], [{ i: 0, w: 0.3 }, { i: 1, w: 0.3 }]),
    ];
    expect(unnamedClusters(nodes, 1)).toEqual([]);
  });

  it("is deterministic: same input yields identical output across runs", () => {
    const W = 0.7;
    const make = (): ImageNode[] => [
      pn("a", 0, 0, [0], [{ i: 1, w: W }, { i: 2, w: W }]),
      pn("b", 1, 0, [0], [{ i: 0, w: W }, { i: 2, w: W }]),
      pn("c", 0, 1, [0], [{ i: 0, w: W }, { i: 1, w: W }]),
    ];
    const first = unnamedClusters(make(), 1);
    const second = unnamedClusters(make(), 1);
    expect(second).toEqual(first);
  });

  it("degenerate inputs yield [] (no nodes / no neighbors / nothing unnamed)", () => {
    expect(unnamedClusters([], 1)).toEqual([]);
    // No neighbors at all → nothing to connect.
    const noEdges = [
      node("a", [0]),
      node("b", [0]),
      node("c", [0]),
    ];
    expect(unnamedClusters(noEdges, 1)).toEqual([]);
    // Everything named (high affinity), even though they are linked.
    const W = 0.9;
    const allNamed: ImageNode[] = [
      { hash: "a", x: 0, y: 0, vx: 0, vy: 0, affinity: [1], neighbors: [{ i: 1, w: W }, { i: 2, w: W }] },
      { hash: "b", x: 0, y: 0, vx: 0, vy: 0, affinity: [1], neighbors: [{ i: 0, w: W }, { i: 2, w: W }] },
      { hash: "c", x: 0, y: 0, vx: 0, vy: 0, affinity: [1], neighbors: [{ i: 0, w: W }, { i: 1, w: W }] },
    ];
    expect(unnamedClusters(allNamed, 1)).toEqual([]);
  });
});

describe("matchClusterLabel — note-phrase label for a soft topic (size match)", () => {
  it("matches the closest-size candidate when it is clear and within tolerance", () => {
    // A clump of 10; the "sunset" candidate (size 9) is a near-perfect size match
    // and far from the next-closest (size 3), so it wins unambiguously.
    const label = matchClusterLabel(10, [
      { label: "sunset over the bay", size: 9 },
      { label: "studio portraits", size: 3 },
    ]);
    expect(label).toBe("sunset over the bay");
  });

  it("returns null when the closest candidate is too far off in size", () => {
    // A clump of 10 vs candidates of 2 and 3: both are >25% away in relative
    // size, so nothing is trustworthy — unlabeled rather than mislabel.
    expect(
      matchClusterLabel(10, [
        { label: "a", size: 2 },
        { label: "b", size: 3 },
      ]),
    ).toBeNull();
  });

  it("returns null on a near-tie (two candidates equally plausible)", () => {
    // Two candidates of size 99 and 98 against a clump of 100: their relative
    // distances (0.01, 0.02) are within the 0.1 separation, so we cannot tell
    // which label belongs to the clump — default to unlabeled.
    expect(
      matchClusterLabel(100, [
        { label: "a", size: 99 },
        { label: "b", size: 98 },
      ]),
    ).toBeNull();
  });

  it("ignores blank labels and matches the next usable candidate", () => {
    // The exact-size candidate has a blank label (no note phrase derived); the
    // size-9 candidate is the best USABLE one and is unambiguous (next is far).
    const label = matchClusterLabel(10, [
      { label: "   ", size: 10 },
      { label: "harvest fields", size: 9 },
      { label: "macro insects", size: 2 },
    ]);
    expect(label).toBe("harvest fields");
  });

  it("returns null for empty candidates or a non-positive cluster size", () => {
    expect(matchClusterLabel(10, [])).toBeNull();
    expect(matchClusterLabel(0, [{ label: "x", size: 0 }])).toBeNull();
  });

  it("trims the returned label", () => {
    const label = matchClusterLabel(5, [{ label: "  beach day  ", size: 5 }]);
    expect(label).toBe("beach day");
  });
});
