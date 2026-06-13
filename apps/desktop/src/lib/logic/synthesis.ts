/**
 * Heatmap x graph SYNTHESIS — the math behind the Attention overlay on the
 * semantic topic-graph lens (DESIGN-SEMANTIC-GRAPH.md + DESIGN-ATTENTION-
 * HEATMAP.md).
 *
 * The concept: overlay the attention FIELD (per-image engagement intensity from
 * the heatmap) onto the semantic STRUCTURE (the topic-graph). Multiplying the
 * two layers is the whole idea — you SEE where your attention lives in the
 * semantic space (ENGAGED), and, the novel inverse, the coherent bodies of work
 * you have OVERLOOKED (coherent-but-cold).
 *
 * This module is PURE + dependency-free so it is unit-testable in isolation: it
 * takes the force sim's image nodes (which already carry per-topic affinity) plus
 * the per-image intensity map (fetched via the EXISTING heatmap `image_intensity`
 * command — no second intensity definition), and computes:
 *   - per-NODE intensity (a super-node aggregates its members' MEAN intensity, so
 *     the overlay composes with v2 LOD without a second code path);
 *   - per-TOPIC aggregate attention (ENGAGED): the affinity-weighted attention
 *     pulled toward each topic anchor, for the tint/glow + a ranked readout;
 *   - per-TOPIC coherence-vs-attention (OVERLOOKED): topics that are tightly
 *     held (high affinity = a coherent body of work) but COLD (low intensity),
 *     for the "themes you've barely touched" readout + glow.
 *
 * The TopicGraph component stays a thin renderer: it calls these, maps the
 * results to canvas tint/size, and lists the ranked readouts. No physics, no
 * IPC, no DOM here.
 */

import type { ImageNode } from "./forcegraph";

/** The three-state Attention overlay (founder-confirmed wording):
 *   - "off"        — the plain graph (today's behavior).
 *   - "engaged"    — where attention LIVES: tint + size nodes by intensity, each
 *                    topic glows by the attention pulled toward it.
 *   - "overlooked" — coherent (high-affinity) but COLD (low-intensity) themes
 *                    glow; the rest dims. The "bodies of work you've overlooked".
 * NOT "hot/neglected" (founder rejected those). Copy is adjustable. */
export type OverlayMode = "off" | "engaged" | "overlooked";

/** One topic's aggregate attention (ENGAGED) or coherence-vs-attention
 * (OVERLOOKED) score, for the ranked readout the graph lists. */
export interface TopicAttention {
  /** Index into the topics array (matches the anchor `topic` ordering). */
  topic: number;
  /** ENGAGED: the topic's total affinity-weighted attention (summed across the
   * images pulled toward it), normalized to [0, 1] across the topic set so the
   * readout + glow are comparable. OVERLOOKED: the "overlooked-ness" score in
   * [0, 1] — high = coherent but cold (a body of work you've barely touched). */
  score: number;
  /** ENGAGED only: the MEAN attention per affiliated image (the topic's affinity-
   * weighted intensity divided by its affinity mass), so a small intense cluster
   * is not buried under a big lukewarm one. 0 in OVERLOOKED mode. */
  mean: number;
}

/** The per-node visual mapping the renderer applies (composes with the existing
 * node draw — see TopicGraph.draw). */
export interface NodeOverlay {
  /** This node's intensity in [0, 1] (a super-node's members' mean). */
  intensity: number;
  /** Glow weight in [0, 1]: how strongly to tint/halo this node in the current
   * mode (ENGAGED ~ intensity; OVERLOOKED ~ coherent-but-cold). 0 = no glow. */
  glow: number;
  /** Size multiplier (>= some floor) applied to the node's base radius, so an
   * engaged node reads bigger and an out-of-focus node dims/shrinks. */
  sizeScale: number;
}

/** A strictly-positive affinity floor: an image only "belongs to" a topic it is
 * actually pulled toward, so attention bleeds along the real semantic edges, not
 * across every (often near-zero) topic. Mirrors the force sim's reading that a
 * 0/negative affinity is "unrelated". */
const AFFINITY_FLOOR = 1e-4;

/**
 * The intensity of ONE node, composing with v2 LOD (DESIGN): a single image's
 * intensity is its direct lookup; a SUPER-NODE's is the MEAN intensity of its
 * member images (so an aggregate of N images reads as hot as its members are on
 * average, never crowned by their sum). Missing intensity = 0 (an honest "no
 * attention", the heatmap's own posture for an un-dwelt image).
 */
export function nodeIntensity(
  node: ImageNode,
  intensity: Map<string, number>,
): number {
  if (node.members !== undefined) {
    if (node.members.length === 0) return 0;
    let sum = 0;
    for (const h of node.members) sum += intensity.get(h) ?? 0;
    return sum / node.members.length;
  }
  return intensity.get(node.hash) ?? 0;
}

/**
 * ENGAGED — per-topic aggregate attention. Each image contributes its intensity
 * to every topic it is pulled toward, WEIGHTED by its affinity to that topic
 * (its "affinity-weighted attention"): a hot image strongly tied to "harbor"
 * pours most of its heat into harbor, a little into a topic it relates to weakly.
 * A super-node contributes its MEAN intensity scaled by its mass (member count),
 * so it stands in for the cluster of images it replaces.
 *
 * Returns one `TopicAttention` per topic, ranked by descending score then topic
 * index (deterministic), with `score` normalized to [0, 1] across the set (so
 * the glow + readout are comparable). Empty nodes / all-cold scope → every score
 * 0 (a well-formed, honest "no attention anywhere").
 */
export function engagedTopics(
  nodes: ImageNode[],
  intensity: Map<string, number>,
  topicCount: number,
): TopicAttention[] {
  // Accumulate, per topic: the affinity-weighted attention (numerator) and the
  // affinity mass (denominator), so we can report BOTH a total (for the glow)
  // and a mean-per-affiliated-image (so a small intense cluster surfaces).
  const weighted = new Array(topicCount).fill(0);
  const mass = new Array(topicCount).fill(0);
  for (const node of nodes) {
    const heat = nodeIntensity(node, intensity);
    // A super-node stands for `mass` images; a single image has mass 1.
    const m = node.mass ?? 1;
    for (let t = 0; t < topicCount; t++) {
      const aff = node.affinity[t] ?? 0;
      if (aff <= AFFINITY_FLOOR) continue;
      weighted[t] += aff * heat * m;
      mass[t] += aff * m;
    }
  }

  // Normalize the totals to [0, 1] across the topic set for a comparable glow.
  let maxTotal = 0;
  for (let t = 0; t < topicCount; t++) if (weighted[t] > maxTotal) maxTotal = weighted[t];

  const out: TopicAttention[] = [];
  for (let t = 0; t < topicCount; t++) {
    out.push({
      topic: t,
      score: maxTotal > 0 ? weighted[t] / maxTotal : 0,
      mean: mass[t] > 0 ? weighted[t] / mass[t] : 0,
    });
  }
  // Ranked readout: most-engaged first, topic index as a stable tie-break.
  out.sort((a, b) => b.score - a.score || a.topic - b.topic);
  return out;
}

/**
 * OVERLOOKED — per-topic coherence-vs-attention. The novel inverse of ENGAGED:
 * a topic that is COHERENT (images are tightly pulled to it — high mean affinity,
 * a real body of work) but COLD (low mean attention) is "overlooked". Its score
 * multiplies coherence by the COMPLEMENT of attention:
 *
 *   overlooked = coherence · (1 − attention)
 *
 * where `coherence` is the topic's affinity-weighted mean affinity (how tightly
 * its affiliated images hold to it) and `attention` is its affinity-weighted
 * mean intensity (how hot those images are), each normalized to [0, 1] across
 * the topic set so the two layers compose on the same scale (the "multiply the
 * two layers" idea). A topic nothing relates to (no coherence) scores 0 — it is
 * not an overlooked BODY OF WORK, just absent.
 *
 * Returns one `TopicAttention` per topic ranked by descending overlooked-score
 * then topic index. `mean` carries the topic's mean attention (so the readout /
 * tests can see how cold it is). Empty / no-affinity scope → all zeros.
 */
export function overlookedTopics(
  nodes: ImageNode[],
  intensity: Map<string, number>,
  topicCount: number,
): TopicAttention[] {
  // Per topic: affinity mass, affinity-weighted affinity (for coherence), and
  // affinity-weighted intensity (for attention). Weighting both by affinity (and
  // mass) keeps a loosely-related image from dragging a topic's coherence down or
  // its attention up.
  const mass = new Array(topicCount).fill(0);
  const affWeighted = new Array(topicCount).fill(0);
  const heatWeighted = new Array(topicCount).fill(0);
  for (const node of nodes) {
    const heat = nodeIntensity(node, intensity);
    const m = node.mass ?? 1;
    for (let t = 0; t < topicCount; t++) {
      const aff = node.affinity[t] ?? 0;
      if (aff <= AFFINITY_FLOOR) continue;
      mass[t] += aff * m;
      affWeighted[t] += aff * aff * m; // affinity-weighted affinity = coherence num
      heatWeighted[t] += aff * heat * m; // affinity-weighted intensity = attention num
    }
  }

  const coherence = new Array(topicCount).fill(0);
  const attention = new Array(topicCount).fill(0);
  for (let t = 0; t < topicCount; t++) {
    if (mass[t] <= 0) continue;
    coherence[t] = affWeighted[t] / mass[t];
    attention[t] = heatWeighted[t] / mass[t];
  }
  // Normalize each layer to [0, 1] across the set so they compose on one scale.
  normalizeInPlace(coherence);
  normalizeInPlace(attention);

  const out: TopicAttention[] = [];
  for (let t = 0; t < topicCount; t++) {
    // Coherent AND cold = overlooked. A topic with no mass (nothing relates to
    // it) keeps coherence 0, so it scores 0 — absent, not "overlooked".
    const score = mass[t] > 0 ? coherence[t] * (1 - attention[t]) : 0;
    out.push({ topic: t, score, mean: attention[t] });
  }
  out.sort((a, b) => b.score - a.score || a.topic - b.topic);
  return out;
}

/** Scale a vector so its max is 1 (in place). An all-zero vector is left as-is
 * (no division by zero); a single non-zero entry becomes 1. */
function normalizeInPlace(v: number[]): void {
  let max = 0;
  for (const x of v) if (x > max) max = x;
  if (max <= 0) return;
  for (let i = 0; i < v.length; i++) v[i] /= max;
}

/** How much a dimmed (out-of-focus) node shrinks/fades, and the engaged-node
 * growth headroom — small, deliberately gentle so the layout stays readable. */
const DIM_FLOOR = 0.5; // a cold/incoherent node never vanishes; it just recedes.
const ENGAGED_GROWTH = 1.0; // a fully-hot node renders up to ~2x its base radius.

/**
 * The per-node visual mapping the renderer composes onto the base node draw.
 *
 *   - OFF: no overlay (glow 0, sizeScale 1) — the plain graph.
 *   - ENGAGED: glow = the node's intensity; size grows with intensity. Where your
 *     attention LIVES lights up and reads bigger; cold nodes stay dim/small.
 *   - OVERLOOKED: a node glows when it is COHERENT (tightly pulled to its
 *     dominant topic — high affinity) but COLD (low intensity). Those bodies of
 *     work you've barely touched light up; everything else dims. We read the
 *     node's OWN dominant affinity as its coherence so the highlight tracks
 *     individual nodes/clusters, not just the topic readout.
 *
 * `topicOverlooked` (optional) is the normalized overlooked-score per topic from
 * `overlookedTopics`, so a node can inherit its dominant topic's overlooked-ness
 * for a cluster-level glow that agrees with the readout.
 */
export function nodeOverlay(
  node: ImageNode,
  intensity: Map<string, number>,
  mode: OverlayMode,
  topicOverlooked?: number[],
): NodeOverlay {
  const heat = nodeIntensity(node, intensity);
  if (mode === "off") {
    return { intensity: heat, glow: 0, sizeScale: 1 };
  }
  if (mode === "engaged") {
    // Glow + grow with intensity: where attention lives lights up and reads
    // bigger. A cold node stays at its base size (sizeScale 1), a fully-hot node
    // grows by ENGAGED_GROWTH.
    const glow = clamp01(heat);
    return { intensity: heat, glow, sizeScale: 1 + ENGAGED_GROWTH * glow };
  }
  // OVERLOOKED: coherent-but-cold glows; the rest dims.
  const dom = dominantAffinity(node.affinity);
  // Prefer the topic-level overlooked-ness when supplied (so the node agrees
  // with the readout / its cluster), else fall back to this node's own coherence
  // (its dominant affinity) times its coldness.
  const topicScore =
    topicOverlooked !== undefined && dom.topic >= 0
      ? (topicOverlooked[dom.topic] ?? 0)
      : clamp01(dom.value);
  const coldness = 1 - clamp01(heat);
  const glow = clamp01(topicScore) * coldness;
  // Overlooked nodes brighten + hold size; everything else recedes to the floor.
  const sizeScale = DIM_FLOOR + (1 - DIM_FLOOR) * glow;
  return { intensity: heat, glow, sizeScale };
}

/** The dominant (strongest-positive) topic of an affinity row and its value, or
 * `{ topic: -1, value: 0 }` when the row is all-zero/negative (unaffiliated).
 * Ties go to the lower index (matches forcegraph's `dominantTopic`). */
function dominantAffinity(affinity: number[]): { topic: number; value: number } {
  let topic = -1;
  let value = 0;
  for (let t = 0; t < affinity.length; t++) {
    if (affinity[t] > value) {
      value = affinity[t];
      topic = t;
    }
  }
  return { topic, value };
}

function clamp01(x: number): number {
  return x < 0 ? 0 : x > 1 ? 1 : x;
}
