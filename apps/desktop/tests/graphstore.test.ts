/**
 * Module-level Visualizer state store (logic/graphstore.ts) — the keyed box that
 * makes a close→reopen of the SAME view instant by restoring the settled layout
 * instead of recomputing it (founder: "leaving and coming back re-renders
 * everything from scratch"). Pure keying + slot logic, so the
 * same-key-reuses / changed-key-recomputes contract pins without a DOM or the
 * Svelte runtime. The component owns the snapshot payload's shape; here we use a
 * stand-in payload to test the box.
 *
 * The key now folds in the LAYOUT-AFFECTING inputs (alpha, fullLibrary) AND the
 * vectors-version the layout was computed against (audit findings B1/B2): a
 * snapshot is only reused when it is actually valid, so a reopen after new
 * vectors landed (or at a different blend) RECOMPUTES rather than restoring a
 * stale layout, while an unchanged reopen still hits the instant restore.
 */
import { beforeEach, describe, expect, it } from "vitest";
import {
  graphState,
  graphStateKey,
  invalidateScopedGraphs,
} from "../src/lib/logic/graphstore";
import type { ScopeKeyInput } from "../src/lib/logic/affinitycache";

const folder: ScopeKeyInput = { kind: "folder", root_id: "r1", folder: "iceland" };
const folder2: ScopeKeyInput = { kind: "folder", root_id: "r1", folder: "spain" };
const collection: ScopeKeyInput = { kind: "collection", id: "c1" };

// Default layout inputs for the cases that are not exercising alpha/fullLibrary/
// version: a fixed blend, folder scope (not full-library), and a stable version,
// so those tests read as "same scope + same topic-set" exactly as before.
const A = 0.5;
const FL = false;
const V = 7;
// A thin wrapper so the order-insensitivity / scope / topic-set cases below stay
// focused on the dimension under test (the new args are held constant here; the
// dedicated blocks vary them one at a time).
const key = (
  scope: ScopeKeyInput,
  topics: readonly string[],
  alpha = A,
  fullLibrary = FL,
  version = V,
): string => graphStateKey(scope, topics, alpha, fullLibrary, version);

beforeEach(() => {
  graphState.clear();
});

describe("graphStateKey — keyed on scope + topic SET (order-insensitive)", () => {
  it("the same scope + same topic set keys identically regardless of add order", () => {
    expect(key(folder, ["harbor", "fog"])).toBe(key(folder, ["fog", "harbor"]));
  });

  it("a different scope is a different key (affinities are scope-relative)", () => {
    expect(key(folder, ["harbor"])).not.toBe(key(folder2, ["harbor"]));
    expect(key(folder, ["harbor"])).not.toBe(key(collection, ["harbor"]));
  });

  it("a different topic set is a different key (the layout re-seeds)", () => {
    expect(key(folder, ["harbor"])).not.toBe(key(folder, ["harbor", "fog"]));
    expect(key(folder, ["harbor"])).not.toBe(key(folder, []));
  });

  it("no topics still keys stably (the empty-set view)", () => {
    expect(key(folder, [])).toBe(key(folder, []));
  });
});

describe("graphStateKey — alpha is part of the key (B1: alpha changes the layout)", () => {
  it("a different alpha is a different key (re-blend moves every node)", () => {
    expect(key(folder, ["harbor"], 0.5)).not.toBe(key(folder, ["harbor"], 0.7));
  });

  it("the SAME alpha keys identically (the fast-path: same blend reopens instantly)", () => {
    expect(key(folder, ["harbor"], 0.5)).toBe(key(folder, ["harbor"], 0.5));
  });

  it("alpha rounds to 3 decimals so slider float jitter buckets together", () => {
    // 0.5 vs 0.50000001 must collapse to one key (a slider emits both for the
    // same intended blend); a real 0.05-step move (0.5 vs 0.55) must not.
    expect(key(folder, ["harbor"], 0.5)).toBe(key(folder, ["harbor"], 0.50000001));
    expect(key(folder, ["harbor"], 0.5)).not.toBe(key(folder, ["harbor"], 0.55));
  });
});

describe("graphStateKey — fullLibrary is part of the key (B1: it widens the scope)", () => {
  it("a different fullLibrary flag is a different key", () => {
    expect(key(folder, ["harbor"], A, false)).not.toBe(
      key(folder, ["harbor"], A, true),
    );
  });

  it("the SAME fullLibrary flag keys identically (fast-path preserved)", () => {
    expect(key(folder, ["harbor"], A, true)).toBe(key(folder, ["harbor"], A, true));
  });
});

describe("graphStateKey — vectorsVersion is part of the key (B1: new vectors invalidate)", () => {
  it("a different vectorsVersion is a different key (a layout missing the new nodes)", () => {
    // The Seam 1 ingest counter advanced (new images embedded into this scope),
    // so a snapshot built at v7 must NOT be reused at v8 — it would restore a
    // layout missing the new nodes that the missing-half guard cannot refresh.
    expect(key(folder, ["harbor"], A, FL, 7)).not.toBe(
      key(folder, ["harbor"], A, FL, 8),
    );
  });

  it("the SAME vectorsVersion keys identically (no new vectors -> instant restore)", () => {
    expect(key(folder, ["harbor"], A, FL, 7)).toBe(key(folder, ["harbor"], A, FL, 7));
  });
});

describe("graphState — restore the SAME view, recompute a CHANGED one", () => {
  it("a reopen of the same view (topics in any order) RESTORES the snapshot", () => {
    const k = key(folder, ["harbor", "fog"]);
    const payload = { positions: [1, 2, 3] };
    graphState.set(k, payload);

    // Reopen: the SAME view (topics in any order, same alpha/fullLibrary/version)
    // restores the exact payload.
    const reopenKey = key(folder, ["fog", "harbor"]);
    expect(graphState.has(reopenKey)).toBe(true);
    expect(graphState.get(reopenKey)).toBe(payload);
  });

  it("a CHANGED scope is a miss (forces a recompute on open)", () => {
    graphState.set(key(folder, ["harbor"]), { positions: [1] });
    const other = key(folder2, ["harbor"]);
    expect(graphState.has(other)).toBe(false);
    expect(graphState.get(other)).toBeNull();
  });

  it("a CHANGED topic set is a miss (forces a recompute on open)", () => {
    graphState.set(key(folder, ["harbor"]), { positions: [1] });
    const added = key(folder, ["harbor", "fog"]);
    expect(graphState.has(added)).toBe(false);
    expect(graphState.get(added)).toBeNull();
  });

  it("a CHANGED alpha is a miss (the re-blended layout differs)", () => {
    graphState.set(key(folder, ["harbor"], 0.5), { positions: [1] });
    const reblended = key(folder, ["harbor"], 0.7);
    expect(graphState.has(reblended)).toBe(false);
    expect(graphState.get(reblended)).toBeNull();
  });

  it("an ADVANCED vectorsVersion is a miss (new vectors -> recompute, not restore)", () => {
    graphState.set(key(folder, ["harbor"], A, FL, 7), { positions: [1] });
    const advanced = key(folder, ["harbor"], A, FL, 8);
    expect(graphState.has(advanced)).toBe(false);
    expect(graphState.get(advanced)).toBeNull();
  });

  it("the box is single-slot: a new view overwrites the prior snapshot", () => {
    const a = key(folder, ["harbor"]);
    const b = key(folder, ["fog"]);
    graphState.set(a, { v: "a" });
    graphState.set(b, { v: "b" });
    // The prior slot is gone (only the latest view is restorable).
    expect(graphState.get(a)).toBeNull();
    expect(graphState.get(b)).toEqual({ v: "b" });
  });

  it("clear() drops the snapshot (an explicit refresh forces a clean recompute)", () => {
    const k = key(folder, ["harbor"]);
    graphState.set(k, { v: 1 });
    expect(graphState.has(k)).toBe(true);
    graphState.clear();
    expect(graphState.has(k)).toBe(false);
    expect(graphState.get(k)).toBeNull();
  });
});

describe("graphState.peek — read the slot WITHOUT a key check (restore reads its alpha)", () => {
  it("returns the stored payload regardless of key (the restore path peeks its own alpha)", () => {
    // The restore path peeks the single slot to learn the alpha/fullLibrary it
    // was laid out at (the component's live alpha is the default on a fresh
    // mount), then keys the validated get against THOSE.
    const payload = { alpha: 0.7, fullLibrary: true };
    graphState.set(key(folder, ["harbor"], 0.7, true), payload);
    expect(graphState.peek()).toBe(payload);
  });

  it("returns null when the slot is empty", () => {
    expect(graphState.peek()).toBeNull();
  });
});

describe("invalidateScopedGraphs — drop a removed root's cached graph", () => {
  it("drops the snapshot for a folder scope under the removed root", () => {
    // The stored key embeds folder:r1/iceland; removing root r1 must drop it so
    // a reopen recomputes instead of restoring a layout over vanished images.
    const k = key(folder, ["harbor"]);
    graphState.set(k, { v: 1 });
    invalidateScopedGraphs("r1");
    expect(graphState.has(k)).toBe(false);
    expect(graphState.get(k)).toBeNull();
  });

  it("leaves a DIFFERENT root's snapshot intact (targeted, not a blanket clear)", () => {
    const other: ScopeKeyInput = {
      kind: "folder",
      root_id: "r2",
      folder: "alps",
    };
    const k = key(other, ["snow"]);
    graphState.set(k, { v: 2 });
    invalidateScopedGraphs("r1"); // removing r1 must not touch r2's view
    expect(graphState.has(k)).toBe(true);
    expect(graphState.get(k)).toEqual({ v: 2 });
  });

  it("does not drop a root whose id is merely a prefix of the removed id", () => {
    // The trailing slash in folder:${root_id}/ prevents removing "r1" from
    // matching a snapshot under "r10" (a prefix collision without the slash).
    const r10: ScopeKeyInput = {
      kind: "folder",
      root_id: "r10",
      folder: "x",
    };
    const k = key(r10, ["t"]);
    graphState.set(k, { v: 3 });
    invalidateScopedGraphs("r1");
    expect(graphState.has(k)).toBe(true);
  });
});
