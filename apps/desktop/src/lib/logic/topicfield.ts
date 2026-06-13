/**
 * Per-topic INFLUENCE FIELD for the semantic topic-graph lens
 * (DESIGN-SEMANTIC-GRAPH.md, founder June 2026). A soft colored glow behind the
 * photo nodes showing each topic's "power level" across the field, fading with
 * distance from where that topic's strong images actually LAND.
 *
 * The whole point is that the field is NOT arbitrary: it reflects the REAL
 * strength of the images WHERE THEY END UP. Node positions emerge from the force
 * sim, which pulls every image by ALL topics at once — so an image dragged toward
 * another topic still carries its T-affinity to where it landed. We splat each
 * node's true T-affinity into a low-res scalar buffer at the node's CURRENT
 * sim-space position (a Gaussian/RBF splat), accumulate, then blur. The result
 * automatically paints the cross-pull reality: where two topics' strong images
 * overlap, both fields are high, and the renderer's screen/lighten composite
 * blends their hues into the "bridge" zones the founder cares about.
 *
 * This module is PURE + dependency-free so it is unit-testable in isolation
 * (a node raises the field near its position; two nodes blend; affinity weights
 * the splat; empty scope is all-zero; the blur spreads + smooths). The component
 * (TopicGraph.svelte) owns the canvas: it maps each topic's field to a hue +
 * alpha and draws it scaled up behind the nodes. No DOM, no physics, no IPC here.
 *
 * Performance: the field is a LOW-RES grid (e.g. 96x96) recomputed only on
 * SETTLE / throttled, never per frame. Splat is O(nodes · kernel) with a small
 * fixed kernel radius; the blur is a cheap SEPARABLE box pass (two 1D passes,
 * optionally repeated to approximate a Gaussian). The component caches the
 * rendered texture between recomputes and does ONE scaled drawImage per frame.
 */

/** A minimal positioned node the field reads: a sim-space position plus the
 * per-topic affinity row (same topic ordering as the graph's anchors). This is
 * a structural subset of forcegraph's ImageNode, so the component passes its
 * live nodes straight in without copying. */
export interface FieldNode {
  x: number;
  y: number;
  /** Per-topic affinity (the real pull weight toward each topic). The splat
   * weight for topic T is `affinity[T]` clamped to >= 0 (a negative/zero
   * affinity contributes nothing — an image only lends power to a topic it is
   * actually pulled toward). */
  affinity: number[];
}

/** The sim-space rectangle the grid covers, in sim coordinates. The component
 * derives this from the node bounding box (padded) so the field tracks the
 * layout, and uses the SAME rect to place the drawn texture under the nodes. */
export interface FieldBounds {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

/** One low-res scalar field per topic, plus the grid geometry needed to draw
 * it. `fields[t][y * width + x]` is topic T's accumulated, blurred influence at
 * grid cell (x, y). Values are >= 0 and un-normalized (the renderer normalizes
 * per topic for a comparable alpha); `max[t]` is the per-topic peak so the
 * renderer can scale alpha without a second pass over the buffer. */
export interface TopicFields {
  width: number;
  height: number;
  topicCount: number;
  bounds: FieldBounds;
  /** Length `topicCount`; each is a `width*height` Float32Array. */
  fields: Float32Array[];
  /** Per-topic peak value (max over the field), for alpha normalization. */
  max: number[];
}

/** Default low-res grid edge. Low enough that splat + blur + one scaled
 * drawImage stay cheap at LOD node counts, high enough that the blurred field
 * reads as a smooth glow rather than blocky cells once it is scaled up. */
export const DEFAULT_GRID = 96;

/** Default Gaussian splat radius in GRID CELLS. The splat is a small truncated
 * Gaussian kernel of this radius; the separable blur then widens the glow
 * further. Kept small so the per-node cost is a fixed handful of cells. */
export const DEFAULT_SPLAT_RADIUS = 3;

/** Default number of separable box-blur passes. Three box passes approximate a
 * Gaussian (central limit) cheaply, smoothing the splats into one soft field. */
export const DEFAULT_BLUR_PASSES = 3;
/** Default box-blur kernel half-width in cells (radius). */
export const DEFAULT_BLUR_RADIUS = 2;

/** A strictly-positive affinity floor mirroring the force sim / synthesis: an
 * image only lends power to a topic it is actually pulled toward, so the field
 * follows the real semantic edges instead of a near-zero haze under everything. */
const AFFINITY_FLOOR = 1e-4;

export interface FieldOptions {
  /** Grid edge (square). Default DEFAULT_GRID. */
  grid?: number;
  /** Splat Gaussian radius in cells. Default DEFAULT_SPLAT_RADIUS. */
  splatRadius?: number;
  /** Separable box-blur passes. Default DEFAULT_BLUR_PASSES (0 disables blur). */
  blurPasses?: number;
  /** Box-blur radius in cells. Default DEFAULT_BLUR_RADIUS. */
  blurRadius?: number;
  /** Padding (sim-space px) added around the node bounding box so a node near
   * the edge still splats its full kernel without clipping. Default derived. */
  pad?: number;
}

/**
 * Compute the per-topic influence fields from positioned nodes.
 *
 * 1. Derive a square-ish sim-space bounds from the node extents (padded), so
 *    the grid tracks wherever the layout settled.
 * 2. SPLAT each node into every topic's buffer: a small truncated-Gaussian
 *    kernel centered on the node's grid cell, scaled by that node's affinity to
 *    the topic. Because the node SITS where the physics put it (shaped by all
 *    topics' pulls), the splat lands the topic's real strength at the real
 *    place — the non-arbitrary core of the design.
 * 3. BLUR each buffer with a cheap separable box pass (repeated to approximate a
 *    Gaussian), turning the discrete splats into one continuous glow that fades
 *    with distance.
 *
 * Empty nodes (or zero topics) yield all-zero fields over a degenerate-but-valid
 * unit bounds, so the renderer can no-op cleanly. Pure + deterministic.
 */
export function computeTopicFields(
  nodes: FieldNode[],
  topicCount: number,
  opts: FieldOptions = {},
): TopicFields {
  const grid = Math.max(1, Math.floor(opts.grid ?? DEFAULT_GRID));
  const splatRadius = Math.max(0, Math.floor(opts.splatRadius ?? DEFAULT_SPLAT_RADIUS));
  const blurPasses = Math.max(0, Math.floor(opts.blurPasses ?? DEFAULT_BLUR_PASSES));
  const blurRadius = Math.max(0, Math.floor(opts.blurRadius ?? DEFAULT_BLUR_RADIUS));

  const width = grid;
  const height = grid;
  const bounds = deriveBounds(nodes, opts.pad);

  const fields: Float32Array[] = [];
  for (let t = 0; t < topicCount; t++) fields.push(new Float32Array(width * height));

  // The Gaussian splat kernel: precompute weights for the (2r+1)^2 footprint so
  // the per-node inner loop is a couple of multiplies, not an exp() per cell.
  const kernel = gaussianKernel(splatRadius);

  // sim-space -> grid-cell scale. Guard a zero-span axis (all nodes on a line)
  // so we don't divide by zero; a degenerate axis maps everything to the center.
  const spanX = bounds.maxX - bounds.minX || 1;
  const spanY = bounds.maxY - bounds.minY || 1;
  const sx = (width - 1) / spanX;
  const sy = (height - 1) / spanY;

  for (const node of nodes) {
    // The node's center grid cell (rounded). The splat kernel spreads around it.
    const gx = Math.round((node.x - bounds.minX) * sx);
    const gy = Math.round((node.y - bounds.minY) * sy);
    for (let t = 0; t < topicCount; t++) {
      const aff = node.affinity[t] ?? 0;
      if (aff <= AFFINITY_FLOOR) continue; // only real pulls lend power
      splat(fields[t], width, height, gx, gy, splatRadius, kernel, aff);
    }
  }

  // Separable box blur per topic: widen + smooth the splats into a soft field.
  if (blurPasses > 0 && blurRadius > 0) {
    const tmp = new Float32Array(width * height);
    for (let t = 0; t < topicCount; t++) {
      for (let p = 0; p < blurPasses; p++) {
        boxBlurSeparable(fields[t], tmp, width, height, blurRadius);
      }
    }
  }

  // Per-topic peak, for the renderer's alpha normalization (one pass).
  const max: number[] = [];
  for (let t = 0; t < topicCount; t++) {
    let m = 0;
    const f = fields[t];
    for (let i = 0; i < f.length; i++) if (f[i] > m) m = f[i];
    max.push(m);
  }

  return { width, height, topicCount, bounds, fields, max };
}

/** Derive the sim-space bounds covering all nodes, padded so an edge node's
 * splat kernel does not clip. Empty input yields a unit square centered at the
 * origin (a valid, all-zero field). */
export function deriveBounds(nodes: FieldNode[], pad?: number): FieldBounds {
  if (nodes.length === 0) {
    return { minX: -1, minY: -1, maxX: 1, maxY: 1 };
  }
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const n of nodes) {
    if (n.x < minX) minX = n.x;
    if (n.y < minY) minY = n.y;
    if (n.x > maxX) maxX = n.x;
    if (n.y > maxY) maxY = n.y;
  }
  // Default padding scales with the span (so the glow has room to fade out past
  // the outermost nodes), with a floor so a tiny/degenerate layout still works.
  const span = Math.max(maxX - minX, maxY - minY);
  const p = pad ?? Math.max(span * 0.15, 1);
  return { minX: minX - p, minY: minY - p, maxX: maxX + p, maxY: maxY + p };
}

/** Build a normalized 1D Gaussian weight set for the splat kernel footprint of
 * the given radius (in cells). Index `r + d` holds the weight at offset d. The
 * 2D splat multiplies the x and y weights (separable Gaussian). Sigma is tied to
 * the radius so the kernel tapers to near-zero at its edge. A radius of 0 yields
 * a single unit weight (a point splat). */
export function gaussianKernel(radius: number): Float32Array {
  const size = radius * 2 + 1;
  const k = new Float32Array(size);
  if (radius === 0) {
    k[0] = 1;
    return k;
  }
  // sigma so the kernel edge sits ~2.5 sigma out (tapered but not vanishing).
  const sigma = radius / 2.5;
  const denom = 2 * sigma * sigma;
  for (let i = 0; i < size; i++) {
    const d = i - radius;
    k[i] = Math.exp(-(d * d) / denom);
  }
  return k;
}

/** Splat one node's weighted Gaussian into a field buffer, clipped to the grid.
 * The 2D weight at offset (dx, dy) is `kernel[r+dx] * kernel[r+dy] * weight`.
 * Cells outside the grid are skipped (an edge node simply contributes its
 * in-bounds portion). */
function splat(
  field: Float32Array,
  width: number,
  height: number,
  gx: number,
  gy: number,
  radius: number,
  kernel: Float32Array,
  weight: number,
): void {
  for (let dy = -radius; dy <= radius; dy++) {
    const py = gy + dy;
    if (py < 0 || py >= height) continue;
    const wy = kernel[radius + dy];
    const row = py * width;
    for (let dx = -radius; dx <= radius; dx++) {
      const px = gx + dx;
      if (px < 0 || px >= width) continue;
      field[row + px] += weight * wy * kernel[radius + dx];
    }
  }
}

/** One separable box-blur pass (horizontal then vertical) over `field` in
 * place, using `tmp` as scratch. A box blur of radius r averages each cell with
 * its 2r+1 neighbors along one axis; doing both axes is a 2D box blur, and
 * repeating the pass approximates a Gaussian (central-limit). Edges clamp the
 * window to the valid range (divide by the actual count) so the border does not
 * darken. O(width*height) per axis, independent of the radius. */
export function boxBlurSeparable(
  field: Float32Array,
  tmp: Float32Array,
  width: number,
  height: number,
  radius: number,
): void {
  // Horizontal pass: field -> tmp.
  for (let y = 0; y < height; y++) {
    const row = y * width;
    for (let x = 0; x < width; x++) {
      let sum = 0;
      let count = 0;
      const lo = Math.max(0, x - radius);
      const hi = Math.min(width - 1, x + radius);
      for (let xx = lo; xx <= hi; xx++) {
        sum += field[row + xx];
        count++;
      }
      tmp[row + x] = sum / count;
    }
  }
  // Vertical pass: tmp -> field.
  for (let x = 0; x < width; x++) {
    for (let y = 0; y < height; y++) {
      let sum = 0;
      let count = 0;
      const lo = Math.max(0, y - radius);
      const hi = Math.min(height - 1, y + radius);
      for (let yy = lo; yy <= hi; yy++) {
        sum += tmp[yy * width + x];
        count++;
      }
      field[y * width + x] = sum / count;
    }
  }
}

/** Sample a field at grid cell (x, y) with bounds-checking, for tests + the
 * renderer's spot checks. Out-of-range returns 0. */
export function sampleField(
  fields: TopicFields,
  topic: number,
  x: number,
  y: number,
): number {
  if (topic < 0 || topic >= fields.topicCount) return 0;
  if (x < 0 || x >= fields.width || y < 0 || y >= fields.height) return 0;
  return fields.fields[topic][y * fields.width + x];
}

// ---------------------------------------------------------------------------
// Per-topic hue assignment + theme-aware compositing helpers (pure; the
// component does the actual canvas drawing). Kept here so the palette + the
// field math live together and the hue choice is unit-testable.
// ---------------------------------------------------------------------------

/** A readable, well-separated hue palette (degrees on the HSL wheel) for the
 * per-topic fields. Chosen to stay distinct as topics composite (screen/lighten)
 * and to read on both the dark and light chrome. Topics beyond the palette
 * length wrap with a golden-angle rotation so even many topics stay separated. */
export const FIELD_HUE_PALETTE = [
  210, // blue
  150, // green
  35, // amber
  330, // magenta
  270, // violet
  90, // lime
  0, // red
  185, // teal
] as const;

/** The hue (degrees, 0..360) for a topic index. The first N use the fixed
 * palette; beyond it, a golden-angle walk keeps later topics maximally apart
 * from each other and the palette, so a long topic list never collides. */
export function topicHue(topic: number): number {
  const n = FIELD_HUE_PALETTE.length;
  if (topic < n) return FIELD_HUE_PALETTE[topic];
  // Golden-angle rotation off the palette span for de-collided extra hues.
  return (FIELD_HUE_PALETTE[topic % n] + ((topic / n) | 0) * 137.508) % 360;
}
