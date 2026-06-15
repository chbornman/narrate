/**
 * Lightweight force simulation for the semantic topic-graph lens
 * (DESIGN-SEMANTIC-GRAPH.md). A pure, dependency-free velocity-Verlet
 * integrator — kept here (not a heavy d3-force dep) so it is small, readable,
 * and UNIT-TESTABLE in isolation (the rAF-driven component just calls `step`).
 *
 * The model (DESIGN "the core mechanic"):
 *   - Topic ANCHOR nodes are FORCE-PLACED (founder dogfood, June 2026): the ring
 *     is only the INITIAL layout. Each anchor is itself pulled toward the
 *     affinity-weighted centroid of the images that hold to it, with mutual
 *     anchor REPULSION so they don't collapse. The consequence is the intended
 *     feature: two topics that share many images drift TOGETHER (surfacing a
 *     relationship), and an image strong for two topics sits in the tight zone
 *     between the now-closer topics. An anchor the user drags is PINNED (a flag
 *     exempts it from the anchor forces) until released.
 *   - Each IMAGE node is pulled toward every topic anchor by a force PROPORTIONAL
 *     to its blended affinity to that topic (so an image relating to two topics
 *     floats between them — the layout IS a semantic map), plus mutual repulsion
 *     between images (so clusters spread, not collapse), plus a gentle pull to
 *     the origin (so an un-topic'd image rests at the center rather than drifting
 *     off-canvas).
 *   - A REHEAT (founder dogfood): when the topic set or alpha/affinity changes,
 *     the caller boosts a "heat" that scales attraction and cools each step, so
 *     the layout SETTLES in roughly a second instead of oozing in slowly.
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

/** A topic anchor — a FORCE-PLACED point the images are pulled toward AND which
 * is itself pulled toward its images' affinity-weighted centroid (with mutual
 * anchor repulsion). The ring is only its initial position; the physics then
 * lets related topics drift together. Draggable + pinnable (founder dogfood). */
export interface TopicAnchor {
  /** Index into the topics array (matches `ImageNode.affinity` ordering). */
  topic: number;
  x: number;
  y: number;
  /** Anchor velocity (force-placed, like an image node). Optional so an older
   * fixed-ring anchor literal still type-checks; `step` treats a missing
   * component as 0 and writes it back. */
  vx?: number;
  vy?: number;
  /** When true the anchor holds its position: the user dragged it there
   * (`fixed`, transient during a drag) or PINNED it (`pinned`, persists until
   * unpinned). Either exempts the anchor from the anchor forces, but images are
   * still pulled toward it — a pinned anchor is a user-placed fixed point. */
  fixed?: boolean;
  pinned?: boolean;
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
  /** Stiffness pulling an anchor toward the affinity-weighted centroid of the
   * images that hold to it. Higher = related topics snap together faster. The
   * ring is only the start; this is the force that moves anchors. */
  anchorAttraction: number;
  /** Mutual anchor-anchor repulsion strength, so force-placed anchors don't
   * collapse onto each other when they share images. Larger than the image
   * repulsion so the ring of topics stays legible. */
  anchorRepulsion: number;
  /** Per-step damping for the anchors (their own cooling). Anchors carry a lot
   * of weight, so a slightly heavier damping keeps them from oscillating. */
  anchorDamping: number;
  /** Weak spring pulling each anchor toward the origin — a SAFETY TETHER, not a
   * layout force. When affinities are healthy the centroid attraction
   * (`anchorAttraction`, ~8x stronger) dominates and this barely moves anything.
   * But when NO image holds to an anchor (a degenerate/zero-affinity set, e.g. a
   * CLIP model mismatch leaving every affinity 0), the centroid attraction is
   * skipped and only mutual repulsion acts — without this tether the anchors
   * accelerate apart FOREVER. The spring balances that repulsion at a finite
   * radius, so a broken-affinity graph rests in a bounded neutral ring instead of
   * flying off-canvas. Absent ⇒ 0 (the pre-tether behavior). */
  anchorCentering?: number;
  /** Current sim HEAT (a reheat multiplier on the attraction terms, image AND
   * anchor). 1 = the steady-state attraction; a fresh topic-set/alpha change
   * boosts this above 1 and the caller cools it toward 1, so the layout settles
   * fast then relaxes. Optional: absent ⇒ 1 (the un-reheated steady state). */
  heat?: number;
  /** STABILITY CLAMP (visualizer audit, June 2026): the maximum distance a body
   * may move in ONE step, in sim-space px. The explicit integrator has no
   * intrinsic bound, so with inverse-square repulsion two affinity-clustered
   * nodes at small separation feel forces of hundreds-to-thousands of px/step;
   * damping only scales velocity, it cannot stop one huge force launching a node
   * across the canvas, which seeds a new near-collision and DIVERGES (energy
   * 1e7+, the "never settles, oozes forever" past ~400 nodes). Clamping each
   * step's displacement keeps the system unconditionally bounded so it always
   * reaches rest. Absent ⇒ [`DEFAULT_MAX_STEP`]. */
  maxStep?: number;
}

/** Place `topicCount` anchors evenly around a ring of `radius`, starting at the
 * top (−90 deg) and going clockwise — the INITIAL layout the force sim then
 * relaxes (anchors are force-placed, not pinned to this ring). A stable,
 * readable seed order that does not jump as topics are added/removed at the end.
 * One topic sits at the top; zero topics yields no anchors (the images then just
 * cluster at the center). Anchors start at rest (zero velocity). */
export function ringAnchors(topicCount: number, radius: number): TopicAnchor[] {
  const anchors: TopicAnchor[] = [];
  for (let i = 0; i < topicCount; i++) {
    // Even spacing; -PI/2 puts the first anchor at the top.
    const angle = -Math.PI / 2 + (i / Math.max(1, topicCount)) * 2 * Math.PI;
    anchors.push({
      topic: i,
      x: radius * Math.cos(angle),
      y: radius * Math.sin(angle),
      vx: 0,
      vy: 0,
    });
  }
  return anchors;
}

/** A small softening length so two coincident nodes don't produce an infinite
 * repulsion (and so the inverse-square stays numerically stable). */
const EPSILON = 1;

/** Default per-step displacement clamp (sim-space px) when `ForceConfig.maxStep`
 * is absent. ~20px keeps legitimate reheat-driven motion fast while bounding the
 * inverse-square divergence the audit found (verified: at this clamp an 800-node
 * cluster settles where it previously exploded). A node simply takes a couple
 * more steps to cross a long gap instead of teleporting and destabilizing. */
export const DEFAULT_MAX_STEP = 20;

/** Clamp a velocity/displacement vector to at most `max` magnitude, scaling both
 * components so direction is preserved. Returns the (possibly scaled) pair. */
function clampStep(vx: number, vy: number, max: number): [number, number] {
  const speed = Math.sqrt(vx * vx + vy * vy);
  if (speed > max) {
    const s = max / speed;
    return [vx * s, vy * s];
  }
  return [vx, vy];
}

/** The view transform (sim-space ↔ screen) for the canvas: centered on the
 * canvas, then panned and zoomed. Kept here (pure) so the round-trip is
 * unit-testable and the pan/zoom-to-cursor math has one source of truth. */
export interface ViewTransform {
  width: number;
  height: number;
  zoom: number;
  panX: number;
  panY: number;
}

/** Sim-space → screen px. screen = canvasCenter + pan + sim·zoom. */
export function simToScreen(
  x: number,
  y: number,
  v: ViewTransform,
): [number, number] {
  return [
    v.width / 2 + v.panX + x * v.zoom,
    v.height / 2 + v.panY + y * v.zoom,
  ];
}

/** Screen px → sim-space (the inverse of `simToScreen`); round-trips exactly. */
export function screenToSim(
  sx: number,
  sy: number,
  v: ViewTransform,
): [number, number] {
  return [
    (sx - v.width / 2 - v.panX) / v.zoom,
    (sy - v.height / 2 - v.panY) / v.zoom,
  ];
}

/**
 * Advance the simulation by one velocity-Verlet step (mutates `nodes` AND
 * `anchors` in place). `nodes[i].affinity[j]` is the pull weight toward the
 * anchor whose `topic === j`. Returns the total kinetic energy (images +
 * anchors) after the step — a cheap convergence signal the caller watches to
 * stop ticking; counting the anchor motion keeps the loop alive while anchors
 * are still drifting into place.
 *
 * Anchors are FORCE-PLACED (founder dogfood): each anchor is pulled toward the
 * affinity-weighted centroid of the images that hold to it and repelled by the
 * other anchors, then integrated like a node — UNLESS it is `fixed`/`pinned`
 * (user-held), in which case it stays put but still attracts images.
 *
 * The reheat: `config.heat` (default 1) multiplies BOTH attraction terms (image
 * and anchor), so a freshly reheated sim pulls hard and settles fast; the caller
 * cools `heat` back toward 1 across frames.
 *
 * O(N·T + N² + T²): the N² image-image repulsion still dominates at scale (the
 * DESIGN scale spike measures it); the T² anchor repulsion is tiny (T = topic
 * count, a handful). Affinities are NOT recomputed here — they are an input,
 * computed once per topic-set/alpha change by the backend.
 */
export function step(
  nodes: ImageNode[],
  anchors: TopicAnchor[],
  config: ForceConfig,
): number {
  const n = nodes.length;
  const t = anchors.length;
  const heat = config.heat ?? 1;
  const attraction = config.attraction * heat;
  const anchorAttraction = config.anchorAttraction * heat;
  // Accumulate force per node.
  const fx = new Float64Array(n);
  const fy = new Float64Array(n);
  // Accumulate force per anchor, plus the affinity-weighted image centroid each
  // anchor is pulled toward (sum of w·position over images, normalized by Σw).
  const afx = new Float64Array(t);
  const afy = new Float64Array(t);
  const wsumX = new Float64Array(t);
  const wsumY = new Float64Array(t);
  const wsum = new Float64Array(t);

  for (let i = 0; i < n; i++) {
    const node = nodes[i];
    const massI = node.mass ?? 1;
    // Attraction to each topic anchor, weighted by affinity. A negative
    // affinity (orthogonal-or-worse cosine) yields a gentle PUSH away, which is
    // the honest reading of "unrelated" — but most affinities sit in [0, 1].
    for (let a = 0; a < t; a++) {
      const anchor = anchors[a];
      const w = node.affinity[anchor.topic] ?? 0;
      if (w === 0) continue;
      fx[i] += attraction * w * (anchor.x - node.x);
      fy[i] += attraction * w * (anchor.y - node.y);
      // The same affinity weight defines this anchor's centroid: an image that
      // holds strongly to a topic pulls that topic's anchor toward it. Weight by
      // the node's MASS too, so a LOD super-node pulls like the cluster it
      // stands for (a single image is mass 1, the v1 behavior).
      if (w > 0) {
        const ww = w * massI;
        wsumX[a] += ww * node.x;
        wsumY[a] += ww * node.y;
        wsum[a] += ww;
      }
    }
    // Centering — a spring to the origin so nothing flies off.
    fx[i] -= config.centering * node.x;
    fy[i] -= config.centering * node.y;
  }

  // Anchor centroid attraction: pull each anchor toward the affinity-weighted
  // centroid of the images holding to it. Two topics sharing many images have
  // overlapping centroids, so their anchors drift TOGETHER (the founder feature).
  const anchorCentering = config.anchorCentering ?? 0;
  for (let a = 0; a < t; a++) {
    // Safety tether to the origin (see ForceConfig.anchorCentering): always
    // applied, but so weak that the centroid attraction below dominates whenever
    // any image holds to this anchor. Its only real job is to bound an anchor
    // that has NO images pulling it (wsum == 0), which otherwise sees only
    // repulsion and drifts away without limit.
    afx[a] -= anchorCentering * anchors[a].x;
    afy[a] -= anchorCentering * anchors[a].y;
    if (wsum[a] <= 0) continue;
    const cx = wsumX[a] / wsum[a];
    const cy = wsumY[a] / wsum[a];
    afx[a] += anchorAttraction * (cx - anchors[a].x);
    afy[a] += anchorAttraction * (cy - anchors[a].y);
  }

  // Mutual ANCHOR repulsion (inverse-square, softened) so force-placed anchors
  // don't collapse onto each other even when they share most images. Symmetric.
  for (let a = 0; a < t; a++) {
    for (let b = a + 1; b < t; b++) {
      let dx = anchors[a].x - anchors[b].x;
      let dy = anchors[a].y - anchors[b].y;
      let d2 = dx * dx + dy * dy;
      if (d2 < EPSILON) {
        dx = (a - b) * 0.01;
        dy = 0.01;
        d2 = dx * dx + dy * dy;
      }
      const d = Math.sqrt(d2);
      const mag = config.anchorRepulsion / d2;
      const ux = dx / d;
      const uy = dy / d;
      afx[a] += mag * ux;
      afy[a] += mag * uy;
      afx[b] -= mag * ux;
      afy[b] -= mag * uy;
    }
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
  const maxStep = config.maxStep ?? DEFAULT_MAX_STEP;
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
    // Clamp the per-step displacement so a single large force cannot launch the
    // node across the canvas and diverge (audit fix); damping alone cannot.
    [node.vx, node.vy] = clampStep(
      (node.vx + fx[i] / mass) * config.damping,
      (node.vy + fy[i] / mass) * config.damping,
      maxStep,
    );
    node.x += node.vx;
    node.y += node.vy;
    energy += node.vx * node.vx + node.vy * node.vy;
  }

  // Integrate the anchors with their own damping. A fixed (being dragged) or
  // pinned (user-placed) anchor holds still — the user's placement wins over the
  // physics — but images are still pulled toward it. Otherwise it drifts toward
  // its images' centroid, repelled by the other anchors.
  for (let a = 0; a < t; a++) {
    const anchor = anchors[a];
    if (anchor.fixed === true || anchor.pinned === true) {
      anchor.vx = 0;
      anchor.vy = 0;
      continue;
    }
    [anchor.vx, anchor.vy] = clampStep(
      ((anchor.vx ?? 0) + afx[a]) * config.anchorDamping,
      ((anchor.vy ?? 0) + afy[a]) * config.anchorDamping,
      maxStep,
    );
    anchor.x += anchor.vx;
    anchor.y += anchor.vy;
    energy += anchor.vx * anchor.vx + anchor.vy * anchor.vy;
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
  restEnergy = 1e-4,
): number {
  // PER-BODY rest test (audit fix): `step` returns total kinetic energy summed
  // over all bodies, so a fixed threshold never trips at scale (hundreds of
  // nodes of irreducible micro-jitter sum past it and the loop runs forever).
  // Compare the MEAN per-body energy so "at rest" is scale-invariant.
  const bodyCount = Math.max(1, nodes.length + anchors.length);
  for (let s = 1; s <= maxSteps; s++) {
    const energy = step(nodes, anchors, config);
    if (energy / bodyCount < restEnergy) return s;
  }
  return maxSteps;
}

// ---------------------------------------------------------------------------
// Reheat (founder dogfood, June 2026) — the gentle low-attraction sim drifted
// nodes in over many frames; a topic add/remove or a blend-slider move felt
// like a slow ooze, not a snap. The reheat boosts the sim ENERGY so a fresh
// layout SETTLES in roughly a second, then cools to the stable steady state.
// ---------------------------------------------------------------------------

/** The heat a reheat starts at: the attraction multiplier on the first frame
 * after a topic-set / alpha change. > 1 so nodes (and anchors) pull hard and
 * close the distance fast; `coolHeat` decays it back toward the 1.0 steady
 * state over the following frames. */
export const REHEAT_START = 10;
/** Per-frame geometric cooling toward 1.0. A START of 10 cooling at 0.88 reaches
 * ~1.0 in well under a second (≈ 14 frames to halve the excess at ~60 fps), so
 * the layout SNAPS into place then relaxes — the founder's "should snap the
 * photos over, not ooze". A higher start + faster cool than the original (6 /
 * 0.92), paired with the hot-phase sub-stepping below, makes the settle visible
 * in a fraction of a second. */
export const HEAT_COOL = 0.88;

/** Heat above this counts as the "hot phase" of a reheat: while the layout is
 * pulling hard the caller runs MULTIPLE sim sub-steps per animation frame so the
 * nodes close the distance fast (a frame's worth of wall-clock buys several
 * integration steps), then drops back to one step per frame as it cools. */
export const HOT_HEAT = 2;
/** How many sim sub-steps to run per frame during the hot phase. 3 sub-steps
 * triples the convergence rate of the opening frames without tripling the draw
 * cost (the expensive part — the canvas paint — still runs once per frame). */
export const HOT_SUBSTEPS = 3;

/** The number of sim sub-steps to advance this frame given the current heat:
 * the hot phase (heat > HOT_HEAT) runs HOT_SUBSTEPS, the cooled steady state
 * runs 1. Pure so the caller's per-frame loop is trivial and the policy is
 * unit-testable. */
export function subStepsForHeat(heat: number): number {
  return heat > HOT_HEAT ? HOT_SUBSTEPS : 1;
}

/** Cool a heat value one frame toward the 1.0 steady state (never below 1). The
 * caller multiplies `config.attraction`/`anchorAttraction` by this; a pure
 * helper so the cooling curve is unit-testable (a reheat RAISES then cools the
 * energy). */
export function coolHeat(heat: number): number {
  return Math.max(1, 1 + (heat - 1) * HEAT_COOL);
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

/**
 * Pull each node's START position toward the anchor it most relates to, so a
 * topic add SNAPS its relevant photos over instead of waiting for the physics to
 * drift them across the whole canvas (founder: "snap the relevant photos over,
 * not ooze"). For every node we find its strongest POSITIVE affinity; if it
 * clears `minAffinity` the node is seeded a fraction `pull` of the way from its
 * current (spiral) seed toward that anchor. A node with no strong topic stays at
 * its spiral seed (it has no home to snap to). Nodes shared between topics get
 * pulled toward their dominant one, and the reheated sim then resolves the
 * tie-break between the now-nearby anchors — a much shorter trip than from the
 * center.
 *
 * Mutates `nodes` in place (and returns it, for chaining). Pure + deterministic
 * (no Math.random), so the seeding is unit-testable: a high-affinity node lands
 * near its anchor, a flat node stays put.
 *
 * @param pull fraction of the gap to the anchor to close immediately (0 = no
 *   move, 1 = sit exactly on the anchor). ~0.6 lands nodes in the anchor's
 *   neighborhood while leaving the physics room to spread the cluster.
 * @param minAffinity the floor a node's peak affinity must clear to be snapped
 *   (a weakly-related node has no clear home and is left at its seed).
 */
export function seedNearAnchors(
  nodes: ImageNode[],
  anchors: TopicAnchor[],
  pull = 0.6,
  minAffinity = 0.15,
): ImageNode[] {
  for (const node of nodes) {
    if (node.fixed === true) continue; // a held node keeps its placed position
    // Find the anchor this node holds to most strongly (its semantic home).
    let bestAnchor: TopicAnchor | null = null;
    let bestAff = minAffinity;
    for (const anchor of anchors) {
      const w = node.affinity[anchor.topic] ?? 0;
      if (w > bestAff) {
        bestAff = w;
        bestAnchor = anchor;
      }
    }
    if (bestAnchor === null) continue; // no strong topic: leave at the spiral seed
    // Close `pull` of the gap from the current seed to that anchor, so the node
    // STARTS near its home and the reheated sim only fine-tunes from there.
    node.x += (bestAnchor.x - node.x) * pull;
    node.y += (bestAnchor.y - node.y) * pull;
  }
  return nodes;
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
