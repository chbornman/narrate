/**
 * Force simulation (logic/forcegraph.ts) — the pure velocity-Verlet integrator
 * behind the semantic topic-graph lens (DESIGN-SEMANTIC-GRAPH.md). Deterministic
 * given the same inputs, so a fixture lays out the SAME way every run: the tests
 * pin convergence + the semantic property that an image is pulled toward the
 * topic it most relates to.
 */
import { describe, expect, it } from "vitest";
import {
  aggregateToSuperNodes,
  coolHeat,
  expandSuperNode,
  HOT_HEAT,
  HOT_SUBSTEPS,
  REHEAT_START,
  ringAnchors,
  screenToSim,
  seedNearAnchors,
  seedNodes,
  shouldUseLod,
  simToScreen,
  simulate,
  step,
  subStepsForHeat,
  type ForceConfig,
  type ImageNode,
  type TopicAnchor,
  type ViewTransform,
} from "../src/lib/logic/forcegraph";

// Baseline config with the anchor forces OFF (anchorAttraction 0), so the image
// physics tests below pin the v1 FIXED-anchor behavior exactly — the
// force-placed-anchor knobs are exercised by their own describe block. (A
// zero-attraction anchor still attracts images; it just doesn't move itself.)
const config: ForceConfig = {
  attraction: 0.05,
  repulsion: 400,
  damping: 0.85,
  centering: 0.01,
  ringRadius: 300,
  anchorAttraction: 0,
  anchorRepulsion: 0,
  anchorDamping: 0.8,
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

// LOD (DESIGN v2): the full-library tractability layer. Tested in the PURE
// layer — super-node creation past the threshold, the member-count-weighted
// centroid, expand/collapse, and the force sim handling mass.
describe("LOD aggregation", () => {
  it("shouldUseLod fires only past the threshold", () => {
    expect(shouldUseLod(1500, 1500)).toBe(false); // at the threshold: full detail
    expect(shouldUseLod(1501, 1500)).toBe(true); // past it: LOD
    expect(shouldUseLod(10, 1500)).toBe(false);
  });

  it("bins images into super-nodes by dominant topic with member-count and mean affinity", () => {
    // 3 images favor topic 0, 2 favor topic 1.
    const aff = new Map<string, number[]>([
      ["a", [0.9, 0.1]],
      ["b", [0.8, 0.0]],
      ["c", [0.7, 0.2]],
      ["d", [0.1, 0.9]],
      ["e", [0.0, 0.8]],
    ]);
    const nodes = seedNodes([...aff.keys()], aff, 2);
    const supers = aggregateToSuperNodes(nodes, 2);
    // Two bins → two super-nodes, in ascending topic order.
    expect(supers).toHaveLength(2);
    expect(supers[0].hash).toBe("super:0");
    expect(supers[1].hash).toBe("super:1");
    // Masses + member lists reflect the bin sizes.
    expect(supers[0].mass).toBe(3);
    expect(supers[0].members).toEqual(["a", "b", "c"]);
    expect(supers[1].mass).toBe(2);
    expect(supers[1].members).toEqual(["d", "e"]);
    // The super-node affinity is the member-count MEAN of its members' rows.
    expect(supers[0].affinity[0]).toBeCloseTo((0.9 + 0.8 + 0.7) / 3, 6);
    expect(supers[0].affinity[1]).toBeCloseTo((0.1 + 0.0 + 0.2) / 3, 6);
  });

  it("is deterministic: same input yields identical super-nodes", () => {
    const aff = new Map<string, number[]>([
      ["a", [0.9, 0.1]],
      ["b", [0.1, 0.9]],
    ]);
    const nodes = seedNodes([...aff.keys()], aff, 2);
    const first = aggregateToSuperNodes(nodes, 2);
    const second = aggregateToSuperNodes(nodes, 2);
    expect(first.map((n) => [n.hash, n.x, n.y, n.mass])).toEqual(
      second.map((n) => [n.hash, n.x, n.y, n.mass]),
    );
  });

  it("expands a super-node back into its member image nodes near its position", () => {
    const aff = new Map<string, number[]>([
      ["a", [0.9, 0.1]],
      ["b", [0.8, 0.0]],
      ["c", [0.1, 0.9]],
    ]);
    const nodes = seedNodes([...aff.keys()], aff, 2);
    const supers = aggregateToSuperNodes(nodes, 2);
    // Expand super:0 (members a, b); super:1 (member c) stays aggregated.
    const expanded = expandSuperNode(supers, "super:0", aff, 2);
    const hashes = expanded.map((n) => n.hash).sort();
    expect(hashes).toEqual(["a", "b", "super:1"].sort());
    // The expanded members carry their REAL per-image affinity (not the mean).
    const a = expanded.find((n) => n.hash === "a")!;
    expect(a.affinity).toEqual([0.9, 0.1]);
    expect(a.members).toBeUndefined();
    // And they spilled out NEAR the super-node's position.
    const sup0 = supers.find((n) => n.hash === "super:0")!;
    expect(Math.hypot(a.x - sup0.x, a.y - sup0.y)).toBeLessThan(20);
  });

  it("with zero topics, all images bin into one unaffiliated super-node", () => {
    const aff = new Map<string, number[]>([
      ["a", []],
      ["b", []],
      ["c", []],
    ]);
    const nodes = seedNodes([...aff.keys()], aff, 0);
    const supers = aggregateToSuperNodes(nodes, 0);
    expect(supers).toHaveLength(1);
    expect(supers[0].hash).toBe("super:-1");
    expect(supers[0].mass).toBe(3);
  });
});

describe("force sim handles super-nodes (mass)", () => {
  it("a heavy super-node repels a light one harder than the reverse displacement", () => {
    // Place a heavy (mass 10) and a light (mass 1) node apart on the x-axis with
    // NO topics (pure repulsion + centering). The light node should be pushed
    // FARTHER from the origin than the heavy one moves, because the pair force is
    // equal-and-opposite but the heavy node's acceleration is divided by its
    // larger mass.
    const heavy: ImageNode = {
      hash: "H",
      x: -5,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: [],
      mass: 10,
      members: new Array(10).fill("x"),
    };
    const light: ImageNode = {
      hash: "L",
      x: 5,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: [],
      mass: 1,
    };
    const nodes = [heavy, light];
    for (let i = 0; i < 50; i++) step(nodes, [], config);
    // Both moved finitely (no NaN/blowup), and the light node ended FARTHER from
    // the origin than the heavy one (the heavy mass barely budged).
    expect(Number.isFinite(light.x)).toBe(true);
    expect(Math.abs(light.x)).toBeGreaterThan(Math.abs(heavy.x));
  });

  it("a single-image node (no mass) behaves exactly as mass 1", () => {
    const a: ImageNode = { hash: "a", x: 10, y: 0, vx: 0, vy: 0, affinity: [] };
    const b: ImageNode = { hash: "b", x: 10, y: 0, vx: 0, vy: 0, affinity: [], mass: 1 };
    step([a], [], config);
    step([b], [], config);
    expect(a.x).toBeCloseTo(b.x, 10);
    expect(a.vx).toBeCloseTo(b.vx, 10);
  });
});

// Force-placed anchors (founder dogfood, June 2026): the ring is only the
// INITIAL layout. Each anchor is pulled toward its images' affinity-weighted
// centroid (so related topics drift together) with mutual anchor repulsion (so
// they don't collapse). A pinned/fixed anchor holds its position.
describe("force-placed anchors", () => {
  // Anchor physics ON; images held fixed so the test isolates the ANCHOR motion
  // (no image repulsion confound), the anchors free to move toward their images.
  const anchorCfg: ForceConfig = {
    attraction: 0.05,
    repulsion: 0,
    damping: 0.85,
    centering: 0,
    ringRadius: 300,
    anchorAttraction: 0.1,
    anchorRepulsion: 0, // off here so the pure centroid pull is unambiguous
    anchorDamping: 0.8,
  };

  it("an anchor drifts toward the centroid of the images that hold to it", () => {
    // One topic; its anchor starts on the ring (top), its images sit off to the
    // RIGHT. The force-placed anchor should move toward those images (its x
    // should rise from ~0 toward the images' x), not stay pinned at the top.
    const anchor: TopicAnchor = { topic: 0, x: 0, y: -300, vx: 0, vy: 0 };
    const imgs: ImageNode[] = [
      { hash: "a", x: 200, y: 0, vx: 0, vy: 0, affinity: [1], fixed: true },
      { hash: "b", x: 200, y: 40, vx: 0, vy: 0, affinity: [1], fixed: true },
    ];
    const x0 = anchor.x;
    const y0 = anchor.y;
    for (let i = 0; i < 200; i++) step(imgs, [anchor], anchorCfg);
    // It moved toward the images' centroid (right and down from the ring top).
    expect(anchor.x).toBeGreaterThan(x0 + 50);
    expect(anchor.y).toBeGreaterThan(y0 + 50);
    // And it converged NEAR the centroid (≈ (200, 20)).
    expect(anchor.x).toBeCloseTo(200, 0);
    expect(anchor.y).toBeCloseTo(20, 0);
  });

  it("an anchor with no images holding to it does not chase a centroid", () => {
    // Affinity 0 to this topic => no centroid pull; with repulsion off the
    // anchor should stay put (within float noise).
    const anchor: TopicAnchor = { topic: 0, x: 10, y: 20, vx: 0, vy: 0 };
    const imgs: ImageNode[] = [
      { hash: "a", x: 200, y: 0, vx: 0, vy: 0, affinity: [0], fixed: true },
    ];
    for (let i = 0; i < 50; i++) step(imgs, [anchor], anchorCfg);
    expect(anchor.x).toBeCloseTo(10, 6);
    expect(anchor.y).toBeCloseTo(20, 6);
  });

  it("mutual anchor repulsion pushes two coincident anchors apart", () => {
    // Two anchors started at (nearly) the same point with NO images: pure anchor
    // repulsion must separate them instead of leaving them collapsed.
    const repelCfg: ForceConfig = {
      ...anchorCfg,
      anchorAttraction: 0,
      anchorRepulsion: 50000,
    };
    const a: TopicAnchor = { topic: 0, x: 0, y: 0, vx: 0, vy: 0 };
    const b: TopicAnchor = { topic: 1, x: 0.5, y: 0, vx: 0, vy: 0 };
    const d0 = Math.hypot(a.x - b.x, a.y - b.y);
    for (let i = 0; i < 100; i++) step([], [a, b], repelCfg);
    const d1 = Math.hypot(a.x - b.x, a.y - b.y);
    expect(d1).toBeGreaterThan(d0);
    expect(d1).toBeGreaterThan(10); // clearly separated, not collapsed
    expect(Number.isFinite(a.x)).toBe(true);
  });

  it("the anchorCentering tether bounds anchors when NO image holds to them", () => {
    // Regression (June 2026): a degenerate affinity set (every affinity 0, e.g. a
    // CLIP model mismatch) gives the anchors NO centroid pull, so with only mutual
    // repulsion they accelerated apart FOREVER ("topics float away from each other
    // forever"). The weak anchorCentering tether must bound them at a finite
    // radius instead. Two anchors, no images, strong repulsion, many steps.
    const tetherCfg: ForceConfig = {
      ...anchorCfg,
      anchorAttraction: 0.1, // healthy value — but no images means it never acts
      anchorRepulsion: 60000,
      anchorCentering: 0.01,
    };
    const a: TopicAnchor = { topic: 0, x: 5, y: 0, vx: 0, vy: 0 };
    const b: TopicAnchor = { topic: 1, x: -5, y: 0, vx: 0, vy: 0 };
    for (let i = 0; i < 5000; i++) step([], [a, b], tetherCfg);
    // Bounded (not flung to infinity) AND still separated by the repulsion.
    const sep = Math.hypot(a.x - b.x, a.y - b.y);
    expect(Number.isFinite(a.x) && Number.isFinite(b.x)).toBe(true);
    expect(Math.hypot(a.x, a.y)).toBeLessThan(1000);
    expect(sep).toBeGreaterThan(20);

    // And WITHOUT the tether (the pre-fix behavior) the same setup runs away: the
    // anchors end up far past where the tether would have held them.
    const noTether: ForceConfig = { ...tetherCfg, anchorCentering: 0 };
    const c: TopicAnchor = { topic: 0, x: 5, y: 0, vx: 0, vy: 0 };
    const d: TopicAnchor = { topic: 1, x: -5, y: 0, vx: 0, vy: 0 };
    for (let i = 0; i < 5000; i++) step([], [c, d], noTether);
    expect(Math.hypot(c.x, c.y)).toBeGreaterThan(Math.hypot(a.x, a.y));
  });

  it("two topics sharing images drift CLOSER than two topics that share none", () => {
    // Shared: both anchors' images overlap in space, so the centroid pull draws
    // the anchors together. Disjoint: each anchor's images sit far apart, so the
    // anchors stay apart. The shared pair ends up closer.
    const cfg: ForceConfig = { ...anchorCfg, anchorAttraction: 0.1, anchorRepulsion: 2000 };
    const run = (imgs: ImageNode[]) => {
      const an = [
        { topic: 0, x: 0, y: -200, vx: 0, vy: 0 } as TopicAnchor,
        { topic: 1, x: 0, y: 200, vx: 0, vy: 0 } as TopicAnchor,
      ];
      for (let i = 0; i < 300; i++) step(imgs, an, cfg);
      return Math.hypot(an[0].x - an[1].x, an[0].y - an[1].y);
    };
    // Shared: every image holds to BOTH topics, all near the origin.
    const shared: ImageNode[] = [
      { hash: "s1", x: 0, y: 0, vx: 0, vy: 0, affinity: [1, 1], fixed: true },
      { hash: "s2", x: 10, y: 0, vx: 0, vy: 0, affinity: [1, 1], fixed: true },
    ];
    // Disjoint: topic 0's images far left, topic 1's far right.
    const disjoint: ImageNode[] = [
      { hash: "d1", x: -300, y: 0, vx: 0, vy: 0, affinity: [1, 0], fixed: true },
      { hash: "d2", x: 300, y: 0, vx: 0, vy: 0, affinity: [0, 1], fixed: true },
    ];
    expect(run(shared)).toBeLessThan(run(disjoint));
  });

  it("a PINNED anchor stays exactly where it was placed", () => {
    const anchor: TopicAnchor = {
      topic: 0,
      x: 123,
      y: -45,
      vx: 0,
      vy: 0,
      pinned: true,
    };
    // Images pulling hard toward a different spot must NOT move a pinned anchor.
    const imgs: ImageNode[] = [
      { hash: "a", x: -200, y: 200, vx: 0, vy: 0, affinity: [1], fixed: true },
    ];
    for (let i = 0; i < 100; i++) step(imgs, [anchor], anchorCfg);
    expect(anchor.x).toBe(123);
    expect(anchor.y).toBe(-45);
    expect(anchor.vx).toBe(0);
  });

  it("a FIXED (being-dragged) anchor also holds its position", () => {
    const anchor: TopicAnchor = { topic: 0, x: 5, y: 5, vx: 0, vy: 0, fixed: true };
    const imgs: ImageNode[] = [
      { hash: "a", x: 300, y: 0, vx: 0, vy: 0, affinity: [1] },
    ];
    step(imgs, [anchor], anchorCfg);
    expect(anchor.x).toBe(5);
    expect(anchor.y).toBe(5);
  });
});

// Reheat (founder dogfood): a topic-set / alpha change boosts the sim energy so
// the layout SETTLES in ~1 s, then cools to the stable steady state. The heat is
// an attraction multiplier; `coolHeat` decays it toward 1.0.
describe("reheat", () => {
  it("coolHeat decays a boosted heat toward 1.0 and never below", () => {
    let h = REHEAT_START;
    expect(h).toBeGreaterThan(1);
    let prev = h;
    // It monotonically cools toward 1.0.
    for (let i = 0; i < 200; i++) {
      h = coolHeat(h);
      expect(h).toBeLessThanOrEqual(prev);
      expect(h).toBeGreaterThanOrEqual(1);
      prev = h;
    }
    expect(h).toBeCloseTo(1, 3); // settled at the steady state
    // It never dips below 1 even from a steady-state input.
    expect(coolHeat(1)).toBe(1);
  });

  it("a reheated step RAISES the kinetic energy over an un-reheated one", () => {
    // Same fixture, two configs differing only in heat: the hot one pulls harder,
    // so the first step injects more kinetic energy (faster settle).
    const anchors = ringAnchors(2, 300);
    const aff = new Map<string, number[]>([
      ["a", [1, 0]],
      ["b", [0, 1]],
    ]);
    const cold = seedNodes([...aff.keys()], aff, 2);
    const hot = seedNodes([...aff.keys()], aff, 2);
    const coldE = step(cold, ringAnchors(2, 300), { ...config, heat: 1 });
    const hotE = step(hot, anchors, { ...config, heat: REHEAT_START });
    expect(hotE).toBeGreaterThan(coldE);
  });
});

// The view transform (pan/zoom) round-trips exactly: a screen point mapped to
// sim-space and back lands where it started, for any pan/zoom. This is the math
// behind background-drag-to-pan and zoom-to-cursor.
describe("view transform", () => {
  it("simToScreen ∘ screenToSim is the identity for any pan/zoom", () => {
    const views: ViewTransform[] = [
      { width: 800, height: 600, zoom: 1, panX: 0, panY: 0 },
      { width: 800, height: 600, zoom: 2.5, panX: -120, panY: 75 },
      { width: 1024, height: 768, zoom: 0.4, panX: 300, panY: -200 },
    ];
    for (const v of views) {
      for (const [sx, sy] of [
        [0, 0],
        [400, 300],
        [799, 599],
      ] as const) {
        const [x, y] = screenToSim(sx, sy, v);
        const [bx, by] = simToScreen(x, y, v);
        expect(bx).toBeCloseTo(sx, 9);
        expect(by).toBeCloseTo(sy, 9);
      }
    }
  });

  it("panning shifts a sim point by exactly the pan delta in screen px", () => {
    const base: ViewTransform = { width: 800, height: 600, zoom: 2, panX: 0, panY: 0 };
    const panned: ViewTransform = { ...base, panX: 30, panY: -15 };
    const [bx, by] = simToScreen(10, 10, base);
    const [px, py] = simToScreen(10, 10, panned);
    expect(px - bx).toBeCloseTo(30, 9);
    expect(py - by).toBeCloseTo(-15, 9);
  });
});

describe("reheat settle policy (founder: snap, not ooze)", () => {
  it("a fresh reheat is hot enough to drive multiple sub-steps per frame", () => {
    // The reheat starts well into the hot band, so the opening frames run the
    // accelerated multi-substep convergence.
    expect(REHEAT_START).toBeGreaterThan(HOT_HEAT);
    expect(subStepsForHeat(REHEAT_START)).toBe(HOT_SUBSTEPS);
  });

  it("the cooled steady state runs a single step per frame", () => {
    expect(subStepsForHeat(1)).toBe(1);
    expect(subStepsForHeat(HOT_HEAT)).toBe(1); // boundary: not strictly hot
  });

  it("coolHeat carries the reheat through the hot band into the steady state", () => {
    // Cooling REHEAT_START repeatedly must drop below HOT_HEAT (so sub-stepping
    // ends) and converge toward 1 (so the loop can finally idle).
    let h = REHEAT_START;
    let hotFrames = 0;
    for (let i = 0; i < 200; i++) {
      if (subStepsForHeat(h) > 1) hotFrames++;
      h = coolHeat(h);
    }
    expect(hotFrames).toBeGreaterThan(0); // there WAS a hot phase
    expect(h).toBeCloseTo(1, 3); // and it cooled to the steady state
  });
});

describe("seedNearAnchors — snap relevant nodes to their topic (founder)", () => {
  const anchors: TopicAnchor[] = [
    { topic: 0, x: -100, y: 0, vx: 0, vy: 0 },
    { topic: 1, x: 100, y: 0, vx: 0, vy: 0 },
  ];

  it("moves a high-affinity node toward the anchor it most relates to", () => {
    // Node strongly holds topic 1; it should jump most of the way to anchor 1
    // from a far seed, instead of starting at the origin and oozing across.
    const node: ImageNode = {
      hash: "h",
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: [0.1, 0.9],
    };
    const before = Math.hypot(node.x - 100, node.y - 0);
    seedNearAnchors([node], anchors, 0.6, 0.15);
    const after = Math.hypot(node.x - 100, node.y - 0);
    expect(after).toBeLessThan(before); // closer to its home anchor
    expect(node.x).toBeCloseTo(60, 6); // 60% of the way from 0 to 100
  });

  it("leaves a flat / weakly-related node at its seed (no clear home)", () => {
    const node: ImageNode = {
      hash: "h",
      x: 7,
      y: -3,
      vx: 0,
      vy: 0,
      affinity: [0.05, 0.05], // below the minAffinity floor
    };
    seedNearAnchors([node], anchors, 0.6, 0.15);
    expect(node.x).toBe(7);
    expect(node.y).toBe(-3);
  });

  it("does not move a held (fixed) node", () => {
    const node: ImageNode = {
      hash: "h",
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: [0.9, 0.1],
      fixed: true,
    };
    seedNearAnchors([node], anchors, 0.6, 0.15);
    expect(node.x).toBe(0);
    expect(node.y).toBe(0);
  });

  it("a seeded node STARTS closer to its settled home than an un-seeded one", () => {
    // The convergence win is that each node begins its trip near its home anchor
    // instead of out at the spiral seed, so after the SAME small step budget the
    // seeded layout is measurably closer to rest. We compare the distance from a
    // strongly-topic-1 node to anchor 1 after a fixed handful of steps: seeding
    // must leave it nearer its home than the un-seeded run.
    const hashes = ["a", "b", "c", "d"];
    const affinities = new Map<string, number[]>([
      ["a", [0.9, 0.0]],
      ["b", [0.85, 0.05]],
      ["c", [0.0, 0.9]],
      ["d", [0.05, 0.85]],
    ]);
    const cfg: ForceConfig = {
      attraction: 0.05,
      repulsion: 200,
      damping: 0.85,
      centering: 0.01,
      ringRadius: 100,
      anchorAttraction: 0,
      anchorRepulsion: 0,
      anchorDamping: 0.8,
    };
    const anc = ringAnchors(2, 100);
    const home = anc[1]; // anchor for topic 1 (node "c" holds it strongest)

    const distOfCAfter = (seed: boolean): number => {
      const ns = seedNodes(hashes, affinities, 2);
      if (seed) seedNearAnchors(ns, ringAnchors(2, 100));
      // A short, equal step budget for both runs.
      for (let i = 0; i < 8; i++) step(ns, ringAnchors(2, 100), cfg);
      const c = ns.find((n) => n.hash === "c")!;
      return Math.hypot(c.x - home.x, c.y - home.y);
    };

    // Pre-seeded, node c starts ~60% of the way to its home, so after the same
    // few steps it is strictly nearer its anchor than the un-seeded spiral start.
    expect(distOfCAfter(true)).toBeLessThan(distOfCAfter(false));
  });
});
