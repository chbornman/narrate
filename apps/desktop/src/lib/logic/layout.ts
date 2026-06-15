/**
 * One-shot DETERMINISTIC layout for the semantic topic-graph lens (visualizer
 * audit Stage 2, June 2026). The CLIP embeddings do not change while you look at
 * the map, so there is nothing to *simulate*: the perpetual force sim only ever
 * chased an equilibrium we can write in CLOSED FORM. This computes that
 * equilibrium directly:
 *
 *   - each topic ANCHOR sits on the ring (the same ringAnchors seed);
 *   - each IMAGE sits at the AFFINITY-WEIGHTED CENTROID of the anchors it relates
 *     to (an image strong for one topic lands on it; an image between two topics
 *     lands between them — the exact "layout IS a semantic map" property);
 *   - images that would stack at the same centroid are DECLUMPED onto a
 *     deterministic phyllotaxis (golden-angle) spiral so a tight cluster reads as
 *     a legible disc instead of one dot.
 *
 * No worker, no rAF physics, no reheat, no settle detection, no instability:
 * O(N), instant, and identical every run. This is the Stage-2 replacement for
 * `seedNodes` + the live `simulate`/`step` loop. Pure + unit-tested.
 */
import type { ImageNode, TopicAnchor } from "./forcegraph";

/** Golden angle (radians) for the phyllotaxis declump — the most uniform,
 * non-repeating angular spread, so a cluster fans out evenly. */
const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5));

export interface LayoutOptions {
  /** Anchor ring radius (sim-space px); also the scale for the no-topic spiral. */
  ringRadius: number;
  /** Spacing between adjacent declumped points (sim-space px). Controls how
   * tightly a same-centroid cluster packs. */
  spread?: number;
}

/** Quantize a centroid to a bucket key so images with effectively the same
 * weighted centroid share a declump spiral. Cell size = spread, so points more
 * than ~one spacing apart keep their distinct centroids. */
function bucketKey(x: number, y: number, cell: number): string {
  return `${Math.round(x / cell)}:${Math.round(y / cell)}`;
}

/**
 * Place every node deterministically from its affinities + the anchor ring.
 * MUTATES each node's x/y (and zeroes vx/vy) in place, mirroring `seedNodes` so
 * the renderer consumes the same `ImageNode` shape. Returns the nodes.
 *
 * `nodes` order is the stable seed order (the caller keeps it stable across
 * re-layouts); the declump spiral is keyed on that order so a node keeps its
 * slot within its cluster.
 */
export function computeStaticLayout(
  nodes: ImageNode[],
  anchors: TopicAnchor[],
  opts: LayoutOptions,
): ImageNode[] {
  const spread = opts.spread ?? 22;

  // 1. Raw weighted-centroid target per node (no declump yet). A node with no
  //    affinity to anything (all zero — e.g. a degenerate/broken set) gets the
  //    origin, exactly where the sim's centering spring rested it.
  const target = nodes.map((n) => {
    let wx = 0;
    let wy = 0;
    let w = 0;
    for (let t = 0; t < anchors.length; t++) {
      const a = n.affinity[t] ?? 0;
      if (a <= 0) continue;
      wx += a * anchors[t].x;
      wy += a * anchors[t].y;
      w += a;
    }
    if (w > 0) return { x: wx / w, y: wy / w };
    return { x: 0, y: 0 };
  });

  // 2. Bucket by quantized centroid so coincident targets share a spiral.
  const buckets = new Map<string, number[]>();
  for (let i = 0; i < nodes.length; i++) {
    const key =
      anchors.length === 0
        ? "origin"
        : bucketKey(target[i].x, target[i].y, spread);
    const b = buckets.get(key);
    if (b) b.push(i);
    else buckets.set(key, [i]);
  }

  // 3. Lay each bucket's members on a phyllotaxis spiral around the shared
  //    centroid. A single-member bucket sits exactly on its centroid (rank 0,
  //    radius 0). The no-topic case spreads all nodes around the origin at
  //    ringRadius scale so they fill the canvas instead of stacking at 0,0.
  const declumpScale = anchors.length === 0 ? opts.ringRadius / 6 : spread;
  for (const members of buckets.values()) {
    for (let rank = 0; rank < members.length; rank++) {
      const i = members[rank];
      if (members.length === 1) {
        nodes[i].x = target[i].x;
        nodes[i].y = target[i].y;
      } else {
        // r ~ sqrt(rank) gives uniform AREAL density (a filled disc); the golden
        // angle keeps successive points maximally spread.
        const r = declumpScale * Math.sqrt(rank);
        const theta = rank * GOLDEN_ANGLE;
        nodes[i].x = target[i].x + r * Math.cos(theta);
        nodes[i].y = target[i].y + r * Math.sin(theta);
      }
      nodes[i].vx = 0;
      nodes[i].vy = 0;
    }
  }
  return nodes;
}
