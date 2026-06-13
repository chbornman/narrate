/**
 * Per-topic INFLUENCE FIELD math (logic/topicfield.ts). The field is the soft
 * colored glow behind the graph nodes showing each topic's "power level" where
 * the images actually landed. These tests pin the non-arbitrary core:
 *   - a node RAISES its topic's field near its sim-space position;
 *   - the splat is WEIGHTED by the node's real affinity (stronger pull = brighter);
 *   - two nodes BLEND (overlapping splats accumulate);
 *   - an empty / all-cold scope yields an all-ZERO field;
 *   - the BLUR spreads the splat to neighbors and smooths it;
 *   - a node only lends power to a topic it is actually pulled toward (floor).
 * The component (TopicGraph.svelte) owns only the canvas hue/alpha draw; all the
 * countable behavior is proven here.
 */
import { describe, expect, it } from "vitest";
import {
  boxBlurSeparable,
  computeTopicFields,
  deriveBounds,
  gaussianKernel,
  sampleField,
  topicHue,
  type FieldNode,
  FIELD_HUE_PALETTE,
} from "../src/lib/logic/topicfield";

/** Map a node's sim position to its center grid cell, mirroring the module's
 * own rounding, so a test can assert "the field is high AT the node". */
function cellOf(
  node: FieldNode,
  bounds: { minX: number; minY: number; maxX: number; maxY: number },
  grid: number,
): [number, number] {
  const spanX = bounds.maxX - bounds.minX || 1;
  const spanY = bounds.maxY - bounds.minY || 1;
  const gx = Math.round((node.x - bounds.minX) * ((grid - 1) / spanX));
  const gy = Math.round((node.y - bounds.minY) * ((grid - 1) / spanY));
  return [gx, gy];
}

describe("computeTopicFields - splat raises the field near a node", () => {
  it("a single affined node makes its topic's field positive at its cell", () => {
    const nodes: FieldNode[] = [{ x: 0, y: 0, affinity: [1, 0] }];
    const f = computeTopicFields(nodes, 2, { grid: 32 });
    const [gx, gy] = cellOf(nodes[0], f.bounds, 32);
    // Topic 0 (the one the node holds to) is lit at the node's cell.
    expect(sampleField(f, 0, gx, gy)).toBeGreaterThan(0);
    // Topic 1 (no affinity) stays dark everywhere.
    expect(f.max[1]).toBe(0);
  });

  it("the field fades with distance from the node (a far cell is weaker)", () => {
    const nodes: FieldNode[] = [{ x: 0, y: 0, affinity: [1] }];
    const f = computeTopicFields(nodes, 1, { grid: 64 });
    const [gx, gy] = cellOf(nodes[0], f.bounds, 64);
    const atNode = sampleField(f, 0, gx, gy);
    // A cell well away from the node is strictly weaker than the peak at it.
    const far = sampleField(f, 0, 0, 0);
    expect(atNode).toBeGreaterThan(far);
    expect(atNode).toBe(f.max[0]); // the peak sits at (near) the node
  });
});

describe("computeTopicFields - affinity weighting", () => {
  it("a stronger affinity produces a brighter field peak", () => {
    // Two separate single-node fields at the same position, different strengths.
    const weak = computeTopicFields([{ x: 0, y: 0, affinity: [0.2] }], 1, {
      grid: 32,
    });
    const strong = computeTopicFields([{ x: 0, y: 0, affinity: [0.9] }], 1, {
      grid: 32,
    });
    expect(strong.max[0]).toBeGreaterThan(weak.max[0]);
    // Linear in the weight: the splat scales by affinity, so ~4.5x brighter.
    expect(strong.max[0] / weak.max[0]).toBeCloseTo(0.9 / 0.2, 4);
  });
});

describe("computeTopicFields - two nodes blend", () => {
  it("two coincident nodes accumulate to roughly double a single node", () => {
    // Two nodes at the SAME spot on the same topic: their splats stack, so the
    // combined peak is about twice one node alone (linear accumulation).
    const one: FieldNode = { x: 0, y: 0, affinity: [1] };
    const single = computeTopicFields([one], 1, { grid: 48 });
    const pair = computeTopicFields([one, { ...one }], 1, { grid: 48 });
    expect(pair.max[0]).toBeCloseTo(single.max[0] * 2, 4);
  });

  it("two overlapping splats blend in the field between the nodes", () => {
    // Two nodes a few sim units apart whose splat+blur tails OVERLAP: the
    // midpoint cell is raised by both, proving the fields blend (the founder's
    // "bridge zone"). Spaced close enough that the kernels reach the middle.
    // A coarse grid (large cells) so the splat+blur kernels overlap at the
    // midpoint; the live graph uses a finer grid + a wider blur to the same end.
    const grid = 16;
    const nodes: FieldNode[] = [
      { x: -1, y: 0, affinity: [1] },
      { x: 1, y: 0, affinity: [1] },
    ];
    const pair = computeTopicFields(nodes, 1, { grid });
    const mid = { x: 0, y: 0, affinity: [1] };
    const [mx, my] = cellOf(mid, pair.bounds, grid);
    // The midpoint is lit (both tails reach it) and the overall peak is no lower
    // than a single node (the splats only add).
    expect(sampleField(pair, 0, mx, my)).toBeGreaterThan(0);
    const single = computeTopicFields([nodes[0]], 1, { grid });
    expect(pair.max[0]).toBeGreaterThanOrEqual(single.max[0]);
  });

  it("distinct topics occupy independent fields (no cross-contamination)", () => {
    const nodes: FieldNode[] = [
      { x: -5, y: 0, affinity: [1, 0] }, // pure topic 0, left
      { x: 5, y: 0, affinity: [0, 1] }, // pure topic 1, right
    ];
    const f = computeTopicFields(nodes, 2, { grid: 48 });
    const [lx, ly] = cellOf(nodes[0], f.bounds, 48);
    const [rx, ry] = cellOf(nodes[1], f.bounds, 48);
    // Topic 0 is bright at the LEFT node, topic 1 is bright at the RIGHT node.
    expect(sampleField(f, 0, lx, ly)).toBeGreaterThan(0);
    expect(sampleField(f, 1, rx, ry)).toBeGreaterThan(0);
    // And each topic is (near) dark at the OTHER node's far cell.
    expect(sampleField(f, 1, lx, ly)).toBeLessThan(sampleField(f, 0, lx, ly));
    expect(sampleField(f, 0, rx, ry)).toBeLessThan(sampleField(f, 1, rx, ry));
  });
});

describe("computeTopicFields - empty / zero", () => {
  it("no nodes yields an all-zero field over a valid unit bounds", () => {
    const f = computeTopicFields([], 3, { grid: 16 });
    expect(f.width).toBe(16);
    expect(f.topicCount).toBe(3);
    for (let t = 0; t < 3; t++) {
      expect(f.max[t]).toBe(0);
      for (let i = 0; i < f.fields[t].length; i++) expect(f.fields[t][i]).toBe(0);
    }
    // Degenerate-but-valid bounds (not NaN/Infinity), so the renderer can no-op.
    expect(Number.isFinite(f.bounds.minX)).toBe(true);
    expect(f.bounds.maxX).toBeGreaterThan(f.bounds.minX);
  });

  it("zero-affinity nodes (all cold) leave the field zero (the floor)", () => {
    const nodes: FieldNode[] = [
      { x: 0, y: 0, affinity: [0, 0] },
      { x: 3, y: 3, affinity: [0, 0] },
    ];
    const f = computeTopicFields(nodes, 2, { grid: 32 });
    expect(f.max[0]).toBe(0);
    expect(f.max[1]).toBe(0);
  });

  it("zero topics yields an empty fields array", () => {
    const f = computeTopicFields([{ x: 0, y: 0, affinity: [] }], 0, { grid: 16 });
    expect(f.fields.length).toBe(0);
    expect(f.max.length).toBe(0);
  });
});

describe("the separable box blur", () => {
  it("spreads a single spike to its neighbors and lowers the peak", () => {
    const w = 9;
    const h = 9;
    const field = new Float32Array(w * h);
    const center = 4 * w + 4;
    field[center] = 9; // a single bright spike in the middle
    const before = field[center];
    const tmp = new Float32Array(w * h);
    boxBlurSeparable(field, tmp, w, h, 1);
    // The spike has spread: the center is lower, the immediate neighbor is now
    // non-zero (energy moved outward).
    expect(field[center]).toBeLessThan(before);
    expect(field[center - 1]).toBeGreaterThan(0);
    expect(field[center - w]).toBeGreaterThan(0);
  });

  it("conserves total energy across the interior (box average)", () => {
    // A uniform interior field stays uniform under a box blur (each cell is the
    // mean of an equal neighborhood), proving the normalization is correct.
    const w = 8;
    const h = 8;
    const field = new Float32Array(w * h).fill(5);
    const tmp = new Float32Array(w * h);
    boxBlurSeparable(field, tmp, w, h, 2);
    // Edge clamping divides by the actual window size, so a flat field is
    // preserved everywhere (no border darkening).
    for (let i = 0; i < field.length; i++) expect(field[i]).toBeCloseTo(5, 5);
  });

  it("blurPasses=0 leaves the raw splats unblurred", () => {
    const nodes: FieldNode[] = [{ x: 0, y: 0, affinity: [1] }];
    const blurred = computeTopicFields(nodes, 1, { grid: 32 });
    const raw = computeTopicFields(nodes, 1, { grid: 32, blurPasses: 0 });
    // The unblurred peak is at least as high (blur spreads energy, lowering it).
    expect(raw.max[0]).toBeGreaterThanOrEqual(blurred.max[0]);
  });
});

describe("gaussianKernel", () => {
  it("radius 0 is a single unit weight (point splat)", () => {
    const k = gaussianKernel(0);
    expect(Array.from(k)).toEqual([1]);
  });

  it("is symmetric and peaks at the center", () => {
    const r = 3;
    const k = gaussianKernel(r);
    expect(k.length).toBe(2 * r + 1);
    expect(k[r]).toBe(Math.max(...k)); // center is the peak
    for (let d = 1; d <= r; d++) {
      expect(k[r - d]).toBeCloseTo(k[r + d], 10); // symmetric
      expect(k[r - d]).toBeLessThan(k[r]); // tapers outward
    }
  });
});

describe("deriveBounds", () => {
  it("pads around the node extents", () => {
    const b = deriveBounds([
      { x: -10, y: -4, affinity: [] },
      { x: 6, y: 8, affinity: [] },
    ]);
    expect(b.minX).toBeLessThan(-10);
    expect(b.maxX).toBeGreaterThan(6);
    expect(b.minY).toBeLessThan(-4);
    expect(b.maxY).toBeGreaterThan(8);
  });

  it("empty input is a finite unit square", () => {
    const b = deriveBounds([]);
    expect(b).toEqual({ minX: -1, minY: -1, maxX: 1, maxY: 1 });
  });
});

describe("topicHue", () => {
  it("uses the fixed palette for the first topics", () => {
    for (let t = 0; t < FIELD_HUE_PALETTE.length; t++) {
      expect(topicHue(t)).toBe(FIELD_HUE_PALETTE[t]);
    }
  });

  it("stays in [0, 360) and de-collides beyond the palette", () => {
    const n = FIELD_HUE_PALETTE.length;
    const h0 = topicHue(0);
    const hWrap = topicHue(n); // first past the palette
    expect(hWrap).toBeGreaterThanOrEqual(0);
    expect(hWrap).toBeLessThan(360);
    // The golden-angle rotation moves it off the exact palette value it wraps to.
    expect(hWrap).not.toBe(h0);
  });
});
