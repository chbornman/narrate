/**
 * MODULE-LEVEL persisted state for the Visualizer (the semantic topic-graph
 * lens). The lens is mounted/unmounted by an `{#if ui.graphOpen}` gate, so the
 * component instance is DESTROYED on close and re-created on open. Without this
 * module the reopen path re-ran EVERYTHING from scratch (re-fetch affinities,
 * re-seed the golden-spiral layout losing the settled positions, reload every
 * thumbnail, recompute the field) — the founder's "leaving and coming back
 * re-renders everything".
 *
 * The fix: keep the expensive computed state ALIVE at module scope, keyed by the
 * (scope, topic-set) it belongs to. On reopen with the SAME key the component
 * RESTORES the settled node/anchor positions + velocities, the affinity report,
 * the zoom/pan, the computed field, and the loaded-thumbnail cache, so reopening
 * is INSTANT and only a real scope/topic change recomputes.
 *
 * WHY module scope (not "keep the component mounted, display:none"): a snapshot
 * survives even a true remount (HMR, a parent re-key), the caches it holds
 * (thumbnails are content-addressed by hash, affinities are pure data) are
 * legitimately app-global, and the KEYING logic is a pure function this file can
 * unit-test without a DOM or the Svelte runtime. The component stays the thin
 * renderer; this owns "does the reopen pay the cost again".
 *
 * The snapshot stores OPAQUE payloads (`unknown` behind a generic accessor) so
 * this module has no dependency on the force-sim / field / thumbnail types — it
 * is purely a keyed box. The component owns the shapes it puts in and casts on
 * the way out, exactly as it already owns the affinity Map's shape.
 */

import { scopeKey, type ScopeKeyInput } from "./affinitycache";

/**
 * The identity a Visualizer snapshot is keyed on: the SCOPE plus the TOPIC SET.
 * A change to either invalidates the snapshot (a different scope re-scans every
 * affinity; a different topic set re-seeds the layout). The topic set is sorted
 * + joined so it is order-insensitive — adding A then B keys the same as B then
 * A (the same backend result + the same anchors), matching the affinity cache's
 * own keying. The alpha blend is NOT part of the key: an alpha move re-blends in
 * place and reheats, but it is still "the same view" the user is sitting in, so
 * a close/reopen at the current alpha should restore, not recompute.
 */
export function graphStateKey(
  scope: ScopeKeyInput,
  topics: readonly string[],
): string {
  const set = [...topics].sort().join(" ");
  return `${scopeKey(scope)}|${set}`;
}

/**
 * A module-level, single-slot snapshot box for the Visualizer's restorable
 * state. One slot is enough: the lens shows exactly one (scope, topic-set) at a
 * time, and the whole point is the IMMEDIATE close→reopen of THAT view. A scope
 * or topic change overwrites the slot (the old layout is stale anyway), so the
 * box never grows and there is nothing to evict.
 *
 * The payload is opaque (`unknown`): the component stores a struct of its own
 * positions/anchors/affinity/view/field and casts it back on restore. This
 * module only decides WHETHER the stored payload still matches the current key.
 */
class GraphStateStore {
  private storedKey: string | null = null;
  private payload: unknown = null;

  /**
   * The saved payload IFF it was stored under `key`, else null. A key mismatch
   * (the scope or topic set changed since the snapshot) returns null so the
   * caller recomputes — the snapshot is never restored across a generation.
   */
  get(key: string): unknown {
    return this.storedKey === key ? this.payload : null;
  }

  /** Whether a payload is stored for `key` (a reopen of the SAME view can be
   * restored). Cheaper than `get` when the caller only needs the boolean. */
  has(key: string): boolean {
    return this.storedKey === key && this.payload !== null;
  }

  /** Snapshot `payload` under `key`, replacing any prior slot. Called on unmount
   * (close) with the component's current restorable state. */
  set(key: string, payload: unknown): void {
    this.storedKey = key;
    this.payload = payload;
  }

  /** Drop the snapshot (e.g. an explicit refresh, or a teardown that must force
   * a clean recompute next open). */
  clear(): void {
    this.storedKey = null;
    this.payload = null;
  }
}

/**
 * THE module-level Visualizer state store. Lives as long as the JS module (i.e.
 * the whole app session), so it OUTLIVES the component's mount/unmount — that is
 * the entire reason it is here and not a `let` inside the component. Imported by
 * TopicGraph.svelte, which snapshots into it on teardown and restores from it on
 * mount.
 */
export const graphState = new GraphStateStore();
