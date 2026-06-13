/**
 * Lightweight force simulation for the semantic topic-graph lens
 * (DESIGN-SEMANTIC-GRAPH.md). A pure, dependency-free velocity-Verlet
 * integrator — kept here (not a heavy d3-force dep) so it is small, readable,
 * and UNIT-TESTABLE in isolation (the rAF-driven component just calls `step`).
 *
 * The model (DESIGN "the core mechanic"):
 *   - Topic ANCHOR nodes sit at fixed positions on a RING (stable, readable —
 *     the DESIGN open-decision lean for v1).
 *   - Each IMAGE node is pulled toward every topic anchor by a force PROPORTIONAL
 *     to its blended affinity to that topic (so an image relating to two topics
 *     floats between them — the layout IS a semantic map), plus mutual repulsion
 *     between images (so clusters spread, not collapse), plus a gentle pull to
 *     the origin (so an un-topic'd image rests at the center rather than drifting
 *     off-canvas).
 *
 * Velocity-Verlet with per-step damping: forces → acceleration → velocity
 * (damped) → position. Deterministic given the same inputs, so a fixture lays
 * out the same way every run (the test pins convergence).
 *
 * WHY NOT d3-force: the model here is bespoke (per-topic affinity-weighted
 * attraction to FIXED anchors, not link/charge/center primitives), the node
 * counts that run live physics are dozens-to-hundreds (DESIGN), and a ~120-line
 * integrator is less code than wiring d3-force's many-body + positioning forces
 * to this exact shape. The full-library scale spike runs the SAME integrator
 * unoptimized to feel the wall (DESIGN §scale) — no second code path.
 */

/** A topic anchor — a fixed point on the ring the images are pulled toward. */
export interface TopicAnchor {
  /** Index into the topics array (matches `ImageNode.affinity` ordering). */
  topic: number;
  x: number;
  y: number;
}

/** A mutable image node the simulation integrates. A node may be a single image
 * OR a LOD super-node aggregating many images (DESIGN-SEMANTIC-GRAPH.md v2): the
 * same sim integrates both, weighting a super-node by its `mass` so it repels
 * and is pulled in proportion to how many images it stands for. */
export interface ImageNode {
  hash: string;
  x: number;
  y: number;
  vx: number;
  vy: number;
  /** Per-topic blended affinity (same order as the anchors' `topic` indices).
   * A higher value = a stronger pull toward that topic's anchor. For a
   * super-node this is the member-count-weighted MEAN affinity of its members,
   * so it sits where its members' centroid would. */
  affinity: number[];
  /** When true (a node the user is dragging) the integrator leaves position
   * untouched so the drag wins over the physics. */
  fixed?: boolean;
  /** Repulsion/attraction mass. A single image has mass 1 (the default when
   * omitted); a LOD super-node has mass = its member count, so it repels and is
   * attracted like the cluster of images it replaces (the sim stays within the
   * scale budget by running over FEWER, heavier nodes). */
  mass?: number;
  /** For a LOD super-node: the hashes of the member images it aggregates, so a
   * click/zoom can EXPAND it back into individual image nodes. Absent on a
   * single-image node. */
  members?: string[];
}

/** The physics knobs — sourced from the backend GraphTuning (file-overridable
 * so the founder tunes the lens by feel). */
export interface ForceConfig {
  /** Pull stiffness toward an anchor, multiplied by the node's affinity. */
  attraction: number;
  /** Mutual image-image repulsion strength. */
  repulsion: number;
  /** Per-step velocity retention (cooling): 1 = frictionless, 0 = frozen. */
  damping: number;
  /** Pull toward the origin (un-topic'd nodes settle at the center). */
  centering: number;
  /** Topic-anchor ring radius in sim-space px. */
  ringRadius: number;
}

/** Place `topicCount` anchors evenly around a ring of `radius`, starting at the
 * top (−90 deg) and going clockwise — a stable, readable order that does not
 * jump as topics are added/removed at the end. One topic sits at the top; zero
 * topics yields no anchors (the images then just cluster at the center). */
export function ringAnchors(topicCount: number, radius: number): TopicAnchor[] {
  const anchors: TopicAnchor[] = [];
  for (let i = 0; i < topicCount; i++) {
    // Even spacing; -PI/2 puts the first anchor at the top.
    const angle = -Math.PI / 2 + (i / Math.max(1, topicCount)) * 2 * Math.PI;
    anchors.push({
      topic: i,
      x: radius * Math.cos(angle),
      y: radius * Math.sin(angle),
    });
  }
  return anchors;
}

/** A small softening length so two coincident nodes don't produce an infinite
 * repulsion (and so the inverse-square stays numerically stable). */
const EPSILON = 1;

/**
 * Advance the simulation by one velocity-Verlet step (mutates `nodes` in
 * place). `anchors` are fixed; `nodes[i].affinity[j]` is the pull weight toward
 * `anchors` whose `topic === j`. Returns the total kinetic energy after the
 * step — a cheap convergence signal the caller can watch to stop ticking.
 *
 * O(N·T + N²): the N² image-image repulsion is the term that bites at scale
 * (the DESIGN scale spike measures exactly where). Affinities are NOT recomputed
 * here — they are an input, computed once per topic-set/alpha change by the
 * backend (DESIGN: "NOT per-frame recompute of affinities").
 */
export function step(
  nodes: ImageNode[],
  anchors: TopicAnchor[],
  config: ForceConfig,
): number {
  const n = nodes.length;
  // Accumulate force per node.
  const fx = new Float64Array(n);
  const fy = new Float64Array(n);

  for (let i = 0; i < n; i++) {
    const node = nodes[i];
    // Attraction to each topic anchor, weighted by affinity. A negative
    // affinity (orthogonal-or-worse cosine) yields a gentle PUSH away, which is
    // the honest reading of "unrelated" — but most affinities sit in [0, 1].
    for (const anchor of anchors) {
      const w = node.affinity[anchor.topic] ?? 0;
      if (w === 0) continue;
      fx[i] += config.attraction * w * (anchor.x - node.x);
      fy[i] += config.attraction * w * (anchor.y - node.y);
    }
    // Centering — a spring to the origin so nothing flies off.
    fx[i] -= config.centering * node.x;
    fy[i] -= config.centering * node.y;
  }

  // Mutual repulsion (inverse-square, softened). Symmetric: compute once per
  // pair and apply equal-and-opposite. A LOD super-node carries the repulsion of
  // the cluster it stands for: the pair magnitude scales with the PRODUCT of the
  // two nodes' masses, so a heavy super-node pushes neighbors apart like the
  // many images it replaces would (a single image has mass 1, recovering the v1
  // behavior exactly).
  for (let i = 0; i < n; i++) {
    const mi = nodes[i].mass ?? 1;
    for (let j = i + 1; j < n; j++) {
      const mj = nodes[j].mass ?? 1;
      let dx = nodes[i].x - nodes[j].x;
      let dy = nodes[i].y - nodes[j].y;
      let d2 = dx * dx + dy * dy;
      if (d2 < EPSILON) {
        // Coincident-or-nearly: nudge deterministically along the index axis so
        // the pair separates instead of dividing by ~0.
        dx = (i - j) * 0.01;
        dy = 0.01;
        d2 = dx * dx + dy * dy;
      }
      const d = Math.sqrt(d2);
      // force magnitude ~ repulsion·m_i·m_j / d2, projected onto the unit
      // separation.
      const mag = (config.repulsion * mi * mj) / d2;
      const ux = dx / d;
      const uy = dy / d;
      fx[i] += mag * ux;
      fy[i] += mag * uy;
      fx[j] -= mag * ux;
      fy[j] -= mag * uy;
    }
  }

  // Integrate: a = f / mass; v = (v + a)·damping; x += v. A heavier (super-)node
  // accelerates LESS for the same force, so an aggregate of N images does not
  // lurch — it drifts to the cluster's home like a single weighty body. (A
  // single image has mass 1, so this is exactly the v1 unit-mass integrator.)
  let energy = 0;
  for (let i = 0; i < n; i++) {
    const node = nodes[i];
    if (node.fixed === true) {
      // A dragged node holds still; zero its velocity so it doesn't lurch when
      // released.
      node.vx = 0;
      node.vy = 0;
      continue;
    }
    const mass = node.mass ?? 1;
    node.vx = (node.vx + fx[i] / mass) * config.damping;
    node.vy = (node.vy + fy[i] / mass) * config.damping;
    node.x += node.vx;
    node.y += node.vy;
    energy += node.vx * node.vx + node.vy * node.vy;
  }
  return energy;
}

/** Run the simulation to (approximate) rest: step until the kinetic energy
 * drops below `restEnergy` or `maxSteps` is hit. Returns the step count taken —
 * the test uses it to assert deterministic convergence; the live component
 * prefers `step` per rAF so the user sees the layout settle. */
export function simulate(
  nodes: ImageNode[],
  anchors: TopicAnchor[],
  config: ForceConfig,
  maxSteps = 600,
  restEnergy = 1e-3,
): number {
  for (let s = 1; s <= maxSteps; s++) {
    const energy = step(nodes, anchors, config);
    if (energy < restEnergy) return s;
  }
  return maxSteps;
}

/** Seed image-node start positions deterministically from the hash (a small
 * spiral) so a layout is reproducible run-to-run — no Math.random, which would
 * make the sim non-deterministic and the test flaky. The physics pulls them to
 * their semantic home from there. */
export function seedNodes(
  hashes: string[],
  affinities: Map<string, number[]>,
  topicCount: number,
): ImageNode[] {
  return hashes.map((hash, i) => {
    // A golden-angle spiral fills the disc evenly; scale keeps the seed compact
    // so the attraction forces dominate the opening frames.
    const angle = i * 2.399963229728653; // golden angle (radians)
    const r = 4 * Math.sqrt(i + 1);
    const aff = affinities.get(hash) ?? new Array(topicCount).fill(0);
    return {
      hash,
      x: r * Math.cos(angle),
      y: r * Math.sin(angle),
      vx: 0,
      vy: 0,
      affinity: aff.length === topicCount ? aff : padTo(aff, topicCount),
    };
  });
}

/** Pad/truncate an affinity row to exactly `len` (defensive against a topic
 * added/removed between an affinity fetch and a re-seed). */
function padTo(row: number[], len: number): number[] {
  const out = new Array(len).fill(0);
  for (let i = 0; i < Math.min(len, row.length); i++) out[i] = row[i];
  return out;
}

// ---------------------------------------------------------------------------
// Level-of-detail (LOD) — full-library tractability (DESIGN-SEMANTIC-GRAPH.md
// v2). v1 ran the live sim + O(N^2) repulsion over EVERY image and a banner
// named the scale wall. v2 keeps the sim within that budget past a threshold by
// AGGREGATING images into SUPER-NODES: the force sim runs over the (few) super-
// nodes, and a super-node EXPANDS into its member image nodes on demand.
// ---------------------------------------------------------------------------

/** Whether to switch on LOD: the raw image count exceeds the tunable threshold
 * (`graph.lod_threshold`). Below it the v1 full-detail layout runs unchanged. */
export function shouldUseLod(imageCount: number, threshold: number): boolean {
  return imageCount > threshold;
}

/** One aggregation bin: the images whose strongest topic is the same anchor
 * (plus an "unaffiliated" bin for images with no topic signal). A bin becomes
 * one super-node. */
interface Bin {
  /** The topic index this bin aggregates around, or -1 for unaffiliated. */
  topic: number;
  hashes: string[];
}

/** The dominant topic of an affinity row: the index of its largest POSITIVE
 * affinity, or -1 when the row is all-zero/negative (no topic pull). Ties go to
 * the lower index for determinism. */
function dominantTopic(affinity: number[]): number {
  let best = -1;
  let bestV = 0; // strictly-positive bar: a 0/negative row is unaffiliated.
  for (let t = 0; t < affinity.length; t++) {
    if (affinity[t] > bestV) {
      bestV = affinity[t];
      best = t;
    }
  }
  return best;
}

/**
 * Aggregate full-detail image nodes into LOD super-nodes (DESIGN v2). Images are
 * BINNED by their dominant topic (the anchor they relate to most); each non-empty
 * bin collapses to ONE super-node whose:
 *   - `mass` / `members.length` is the bin's image count (so the sim weights it
 *     like the cluster it replaces, and the UI can size the dot by member count);
 *   - `affinity` is the member-count MEAN of its members' affinity rows (so the
 *     super-node is pulled to where its members' affinity-weighted centroid sits);
 *   - `hash` is a stable synthetic id (`super:<topic>`) so re-aggregation is
 *     deterministic and pick/expand can find it.
 *
 * With zero topics every image is unaffiliated, so this yields a single super-
 * node — the honest "nothing to separate them by yet" state. Pure + deterministic
 * (no Math.random; bins and members keep input order) so the test pins it.
 */
export function aggregateToSuperNodes(
  nodes: ImageNode[],
  topicCount: number,
): ImageNode[] {
  // Bin by dominant topic, preserving input order within each bin.
  const bins = new Map<number, Bin>();
  for (const node of nodes) {
    const t = dominantTopic(node.affinity);
    let bin = bins.get(t);
    if (bin === undefined) {
      bin = { topic: t, hashes: [] };
      bins.set(t, bin);
    }
    bin.hashes.push(node.hash);
  }

  // Build one super-node per bin. Iterate the affiliated topics in ascending
  // order, then the unaffiliated bin last, so the output order is deterministic.
  const order = [...bins.keys()].sort((a, b) => a - b);
  const byHash = new Map(nodes.map((nd) => [nd.hash, nd]));
  const supers: ImageNode[] = [];
  let seedIndex = 0;
  for (const t of order) {
    const bin = bins.get(t)!;
    // Member-count mean of the affinity rows = the bin's centroid in topic
    // space; the sim then pulls the super-node toward that centroid.
    const mean = new Array(topicCount).fill(0);
    for (const h of bin.hashes) {
      const a = byHash.get(h)!.affinity;
      for (let k = 0; k < topicCount; k++) mean[k] += a[k] ?? 0;
    }
    for (let k = 0; k < topicCount; k++) mean[k] /= bin.hashes.length;

    // Deterministic seed position (the same golden-angle spiral as seedNodes).
    const angle = seedIndex * 2.399963229728653;
    const r = 4 * Math.sqrt(seedIndex + 1);
    seedIndex++;
    supers.push({
      hash: `super:${t}`,
      x: r * Math.cos(angle),
      y: r * Math.sin(angle),
      vx: 0,
      vy: 0,
      affinity: mean,
      mass: bin.hashes.length,
      members: bin.hashes,
    });
  }
  return supers;
}

/**
 * Expand ONE super-node back into its member image nodes (DESIGN v2: "expand a
 * super-node on click or zoom past a threshold"). The member nodes are seeded
 * around the super-node's current position (so they spill out from where the
 * aggregate sat, not from the origin) and carry their original per-image
 * affinity from `affinities`. The OTHER super-nodes are kept as-is, so the sim
 * runs over the mixed set (one expanded cluster + the rest still aggregated),
 * staying within the budget. Collapsing again is just a fresh
 * `aggregateToSuperNodes` over the full detail set.
 */
export function expandSuperNode(
  current: ImageNode[],
  superHash: string,
  affinities: Map<string, number[]>,
  topicCount: number,
): ImageNode[] {
  const out: ImageNode[] = [];
  for (const node of current) {
    if (node.hash === superHash && node.members !== undefined) {
      // Replace the super-node with its members, seeded around its position.
      node.members.forEach((h, i) => {
        const angle = i * 2.399963229728653;
        const r = 4 * Math.sqrt(i + 1);
        const aff = affinities.get(h) ?? new Array(topicCount).fill(0);
        out.push({
          hash: h,
          x: node.x + r * Math.cos(angle),
          y: node.y + r * Math.sin(angle),
          vx: 0,
          vy: 0,
          affinity: aff.length === topicCount ? aff : padTo(aff, topicCount),
        });
      });
    } else {
      out.push(node);
    }
  }
  return out;
}
