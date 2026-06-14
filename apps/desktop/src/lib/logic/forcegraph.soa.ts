/**
 * Structure-of-arrays (SoA) pack/unpack for the force-sim Web Worker
 * (PLAN-PERF.md P3). The force sim's O(N^2) image-image repulsion is the heavy
 * per-frame compute; running it in a Worker keeps the UI main thread free for
 * rendering + input (the "buttery smooth" Visualizer goal). To move the compute
 * off-thread WITHOUT per-frame copy cost we ship the MUTABLE per-frame state as
 * a single TRANSFERABLE Float64Array: the main thread transfers the buffer to
 * the worker, the worker integrates over it and transfers it back. Transfer
 * (not copy) means the bytes change hands with no allocation/serialization, so
 * the only marshaling cost is the (tiny) message envelope.
 *
 * The STATIC-ish inputs (per-node affinity rows + mass, per-anchor topic, the
 * ForceConfig) change RARELY (a topic add, a config change), so they are sent to
 * the worker ON CHANGE only via a separate message - never in this per-frame
 * buffer. This module owns the mutable x/y/vx/vy lane that round-trips every
 * frame, plus the small FLAG lane whose value the DRAG path flips per-frame (a
 * node/anchor the user grabs is `fixed` so the sim leaves it under the pointer).
 *
 * Kept PURE + separate from forcegraph.ts (the unit-tested integrator stays
 * exactly as-is; the worker IMPORTS step from it) so the buffer layout is its
 * own unit-testable seam: pack -> unpack must round-trip every position/velocity
 * exactly.
 */

import type { ImageNode, TopicAnchor } from "./forcegraph";

/** Float64 lanes per body in the mutable buffer: x, y, vx, vy. Float64 (not 32)
 * so the worker's integration is bit-identical to the inline main-thread path
 * the fallback runs - no precision drift between the two code paths. */
export const MUT_STRIDE = 4;

/** Bits in the per-frame FLAG lane (a small Uint8Array, one byte per body) for
 * the mutable flags the drag path flips: a grabbed node/anchor is `fixed`, and
 * an anchor can be `pinned`. These ride the per-frame message (not the static
 * message) because a drag changes them every frame and the sim must honor them
 * on the NEXT step, not on the next static resync. */
export const FLAG_FIXED = 1;
export const FLAG_PINNED = 2;

/** Total body count the buffers cover: every image node, then every anchor.
 * Nodes occupy indices [0, nodes.length); anchors [nodes.length, total). */
export function bodyCount(nodeCount: number, anchorCount: number): number {
  return nodeCount + anchorCount;
}

/** Allocate a fresh mutable float buffer sized for the given counts. The caller
 * reuses one buffer across frames (re-allocating only when the node/anchor count
 * changes), transferring it to the worker and receiving it back each frame. */
export function makeMutableBuffer(
  nodeCount: number,
  anchorCount: number,
): Float64Array {
  return new Float64Array(bodyCount(nodeCount, anchorCount) * MUT_STRIDE);
}

/** Pack the MUTABLE state (x, y, vx, vy) of every node then every anchor into
 * `buf` (length = bodyCount * MUT_STRIDE). Nodes first, anchors after, so the
 * worker reconstructs the same order. A missing anchor velocity reads as 0 (an
 * older fixed-ring anchor literal omits it; `step` already tolerates this). */
export function packMutable(
  buf: Float64Array,
  nodes: ImageNode[],
  anchors: TopicAnchor[],
): void {
  let o = 0;
  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i];
    buf[o] = n.x;
    buf[o + 1] = n.y;
    buf[o + 2] = n.vx;
    buf[o + 3] = n.vy;
    o += MUT_STRIDE;
  }
  for (let a = 0; a < anchors.length; a++) {
    const an = anchors[a];
    buf[o] = an.x;
    buf[o + 1] = an.y;
    buf[o + 2] = an.vx ?? 0;
    buf[o + 3] = an.vy ?? 0;
    o += MUT_STRIDE;
  }
}

/** Unpack the MUTABLE state from `buf` back onto the node/anchor objects (the
 * inverse of packMutable). Used on BOTH sides: the worker writes the buffer onto
 * its mirror objects before stepping, and the main thread writes the returned
 * buffer onto the live state objects after the worker computes. The objects keep
 * their static fields (affinity, mass, flags); only the 4 mutable lanes change. */
export function unpackMutable(
  buf: Float64Array,
  nodes: ImageNode[],
  anchors: TopicAnchor[],
): void {
  let o = 0;
  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i];
    n.x = buf[o];
    n.y = buf[o + 1];
    n.vx = buf[o + 2];
    n.vy = buf[o + 3];
    o += MUT_STRIDE;
  }
  for (let a = 0; a < anchors.length; a++) {
    const an = anchors[a];
    an.x = buf[o];
    an.y = buf[o + 1];
    an.vx = buf[o + 2];
    an.vy = buf[o + 3];
    o += MUT_STRIDE;
  }
}

/** Pack the per-frame mutable FLAGS (a node's `fixed`; an anchor's
 * `fixed`/`pinned`) into a one-byte-per-body lane. The drag path flips these
 * live, so they ride every compute message: the worker must hold a grabbed body
 * still THIS step, not after the next static resync. */
export function packFlags(
  flags: Uint8Array,
  nodes: ImageNode[],
  anchors: TopicAnchor[],
): void {
  for (let i = 0; i < nodes.length; i++) {
    flags[i] = nodes[i].fixed === true ? FLAG_FIXED : 0;
  }
  const base = nodes.length;
  for (let a = 0; a < anchors.length; a++) {
    const an = anchors[a];
    flags[base + a] =
      (an.fixed === true ? FLAG_FIXED : 0) |
      (an.pinned === true ? FLAG_PINNED : 0);
  }
}

/** Apply the per-frame flag lane onto the worker's mirror objects before it
 * steps, so a body the user is dragging is `fixed` (held still) on this step. */
export function unpackFlags(
  flags: Uint8Array,
  nodes: ImageNode[],
  anchors: TopicAnchor[],
): void {
  for (let i = 0; i < nodes.length; i++) {
    nodes[i].fixed = (flags[i] & FLAG_FIXED) !== 0;
  }
  const base = nodes.length;
  for (let a = 0; a < anchors.length; a++) {
    const an = anchors[a];
    an.fixed = (flags[base + a] & FLAG_FIXED) !== 0;
    an.pinned = (flags[base + a] & FLAG_PINNED) !== 0;
  }
}
