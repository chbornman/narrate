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

/** A mutable image node the simulation integrates. */
export interface ImageNode {
  hash: string;
  x: number;
  y: number;
  vx: number;
  vy: number;
  /** Per-topic blended affinity (same order as the anchors' `topic` indices).
   * A higher value = a stronger pull toward that topic's anchor. */
  affinity: number[];
  /** When true (a node the user is dragging) the integrator leaves position
   * untouched so the drag wins over the physics. */
  fixed?: boolean;
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
  // pair and apply equal-and-opposite.
  for (let i = 0; i < n; i++) {
    for (let j = i + 1; j < n; j++) {
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
      // force magnitude ~ repulsion / d2, projected onto the unit separation.
      const mag = config.repulsion / d2;
      const ux = dx / d;
      const uy = dy / d;
      fx[i] += mag * ux;
      fy[i] += mag * uy;
      fx[j] -= mag * ux;
      fy[j] -= mag * uy;
    }
  }

  // Integrate: v = (v + f)·damping; x += v. (Unit mass, unit dt — the knobs
  // already carry the scale, so extra constants would just be redundant.)
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
    node.vx = (node.vx + fx[i]) * config.damping;
    node.vy = (node.vy + fy[i]) * config.damping;
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
