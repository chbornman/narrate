/**
 * MODULE-LEVEL persisted state for the Visualizer (the semantic topic-graph
 * lens). The lens is mounted/unmounted by the `viewMode === "visualizer"`
 * render arm (DESIGN-VIEW-MODES.md), so the component instance is DESTROYED on
 * close and re-created on open. Without this
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
 * The identity a Visualizer snapshot is keyed on: the SCOPE, the TOPIC SET, the
 * ALPHA blend, the FULL-LIBRARY flag, and the VECTORS VERSION the layout was
 * computed against. A change to ANY of these invalidates the snapshot, because
 * each one changes the laid-out positions:
 *   - scope: re-scans every affinity (the layout is scope-relative);
 *   - topic set: re-seeds the anchor ring + every node's pull (sorted + joined
 *     so it is order-insensitive — adding A then B keys the same as B then A,
 *     matching the affinity cache's own keying);
 *   - alpha: re-blends looks-vs-said, moving every node (rounded to 3 decimals,
 *     finer than the 0.05-step slider, so float jitter buckets together but a
 *     real blend change keys distinctly — mirrors affinityKey);
 *   - fullLibrary: widens the scope to the whole library, a different node set;
 *   - vectorsVersion: the Seam 1 ingest counter. New images embedding into a
 *     scope bumps it; a layout snapshotted at an OLDER version is missing those
 *     nodes, so it must be a different key (a MISS that recomputes) rather than
 *     restoring a stale layout the missing-half guard cannot refresh.
 *
 * WHY fold these into the KEY (rather than restore-then-validate): a key miss is
 * exactly the existing "scope/topics changed" path — the restore returns null
 * and the caller cold-opens (recomputes). No new validation branch, and the
 * fast-path is preserved: an UNCHANGED reopen (same scope/topics/alpha/flag and
 * no new vectors since) keys identically and still hits the instant restore.
 */
export function graphStateKey(
  scope: ScopeKeyInput,
  topics: readonly string[],
  alpha: number,
  fullLibrary: boolean,
  vectorsVersion: number,
): string {
  const set = [...topics].sort().join(" ");
  // 3 decimals is finer than the 0.05-step blend slider, so distinct blends key
  // distinctly while float jitter (0.5 vs 0.50000001) collapses to one bucket —
  // the same rounding affinityKey uses, so the two caches agree on what "the
  // same blend" means.
  const a = alpha.toFixed(3);
  return `${scopeKey(scope)}|${set}|a=${a}|fl=${fullLibrary}|v=${vectorsVersion}`;
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

  /**
   * The stored payload WITHOUT a key check, or null when the slot is empty. The
   * keyed `get` is the validated accessor; `peek` exists for one narrow need: on
   * a fresh mount the component's own ALPHA + fullLibrary start at their defaults
   * (the snapshot is the source of truth for what they should be), so it cannot
   * build the alpha/fullLibrary-bearing key BEFORE it knows the snapshot's alpha.
   * It peeks the single slot to read those layout inputs, sets them, THEN does
   * the validated `get` — so the scope/topics/vectors-version still gate the
   * restore, but a non-default-alpha view (saved at a=0.7, reopened with the
   * component freshly at a=0.5) still hits the fast path instead of self-missing.
   * Safe because the box is single-slot: there is at most one snapshot to peek.
   */
  peek(): unknown {
    return this.payload;
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

  /** Drop the snapshot IFF its stored key contains `substr`. Used to invalidate
   * the cached graph for a scope whose backing data has gone away (a removed
   * root): the stored key embeds the scopeKey, so a substring match on the
   * folder prefix targets exactly the affected view without a blanket clear. */
  clearIfKeyContains(substr: string): void {
    if (this.storedKey !== null && this.storedKey.includes(substr)) {
      this.storedKey = null;
      this.payload = null;
    }
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

/**
 * Invalidate any cached Visualizer graph belonging to a now-removed root. When a
 * root is removed, every folder scope under it is dead, but the module-level
 * snapshot for that scope persists (the close→reopen "view-swap workaround")
 * and would restore a stale layout pointing at vanished images. We key on the
 * folder scopeKey shape `folder:${root_id}/${folder}` (see affinitycache
 * `scopeKey`): the stored graphStateKey embeds that scopeKey, so a substring
 * match on the `folder:${root_id}/` prefix drops exactly the affected snapshot.
 * WHY the trailing slash: it scopes the match to THIS root's folders and avoids
 * a prefix collision with a different root whose id merely starts the same.
 */
export function invalidateScopedGraphs(root_id: string): void {
  graphState.clearIfKeyContains(`folder:${root_id}/`);
}
