/**
 * Module-level Visualizer state store (logic/graphstore.ts) — the keyed box that
 * makes a close→reopen of the SAME (scope, topic-set) instant by restoring the
 * settled layout instead of recomputing it (founder: "leaving and coming back
 * re-renders everything from scratch"). Pure keying + slot logic, so the
 * same-key-reuses / changed-key-recomputes contract pins without a DOM or the
 * Svelte runtime. The component owns the snapshot payload's shape; here we use a
 * stand-in payload to test the box.
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

beforeEach(() => {
  graphState.clear();
});

describe("graphStateKey — keyed on scope + topic SET (order-insensitive)", () => {
  it("the same scope + same topic set keys identically regardless of add order", () => {
    expect(graphStateKey(folder, ["harbor", "fog"])).toBe(
      graphStateKey(folder, ["fog", "harbor"]),
    );
  });

  it("a different scope is a different key (affinities are scope-relative)", () => {
    expect(graphStateKey(folder, ["harbor"])).not.toBe(
      graphStateKey(folder2, ["harbor"]),
    );
    expect(graphStateKey(folder, ["harbor"])).not.toBe(
      graphStateKey(collection, ["harbor"]),
    );
  });

  it("a different topic set is a different key (the layout re-seeds)", () => {
    expect(graphStateKey(folder, ["harbor"])).not.toBe(
      graphStateKey(folder, ["harbor", "fog"]),
    );
    expect(graphStateKey(folder, ["harbor"])).not.toBe(
      graphStateKey(folder, []),
    );
  });

  it("no topics still keys stably (the empty-set view)", () => {
    expect(graphStateKey(folder, [])).toBe(graphStateKey(folder, []));
  });
});

describe("graphState — restore the SAME view, recompute a CHANGED one", () => {
  it("a reopen of the same (scope, topic-set) RESTORES the snapshot", () => {
    const key = graphStateKey(folder, ["harbor", "fog"]);
    const payload = { positions: [1, 2, 3] };
    graphState.set(key, payload);

    // Reopen: the SAME view (topics in any order) restores the exact payload.
    const reopenKey = graphStateKey(folder, ["fog", "harbor"]);
    expect(graphState.has(reopenKey)).toBe(true);
    expect(graphState.get(reopenKey)).toBe(payload);
  });

  it("a CHANGED scope is a miss (forces a recompute on open)", () => {
    const key = graphStateKey(folder, ["harbor"]);
    graphState.set(key, { positions: [1] });
    const other = graphStateKey(folder2, ["harbor"]);
    expect(graphState.has(other)).toBe(false);
    expect(graphState.get(other)).toBeNull();
  });

  it("a CHANGED topic set is a miss (forces a recompute on open)", () => {
    const key = graphStateKey(folder, ["harbor"]);
    graphState.set(key, { positions: [1] });
    const added = graphStateKey(folder, ["harbor", "fog"]);
    expect(graphState.has(added)).toBe(false);
    expect(graphState.get(added)).toBeNull();
  });

  it("the box is single-slot: a new view overwrites the prior snapshot", () => {
    const a = graphStateKey(folder, ["harbor"]);
    const b = graphStateKey(folder, ["fog"]);
    graphState.set(a, { v: "a" });
    graphState.set(b, { v: "b" });
    // The prior slot is gone (only the latest view is restorable).
    expect(graphState.get(a)).toBeNull();
    expect(graphState.get(b)).toEqual({ v: "b" });
  });

  it("clear() drops the snapshot (an explicit refresh forces a clean recompute)", () => {
    const key = graphStateKey(folder, ["harbor"]);
    graphState.set(key, { v: 1 });
    expect(graphState.has(key)).toBe(true);
    graphState.clear();
    expect(graphState.has(key)).toBe(false);
    expect(graphState.get(key)).toBeNull();
  });
});

describe("invalidateScopedGraphs — drop a removed root's cached graph", () => {
  it("drops the snapshot for a folder scope under the removed root", () => {
    // The stored key embeds folder:r1/iceland; removing root r1 must drop it so
    // a reopen recomputes instead of restoring a layout over vanished images.
    const key = graphStateKey(folder, ["harbor"]);
    graphState.set(key, { v: 1 });
    invalidateScopedGraphs("r1");
    expect(graphState.has(key)).toBe(false);
    expect(graphState.get(key)).toBeNull();
  });

  it("leaves a DIFFERENT root's snapshot intact (targeted, not a blanket clear)", () => {
    const other: ScopeKeyInput = {
      kind: "folder",
      root_id: "r2",
      folder: "alps",
    };
    const key = graphStateKey(other, ["snow"]);
    graphState.set(key, { v: 2 });
    invalidateScopedGraphs("r1"); // removing r1 must not touch r2's view
    expect(graphState.has(key)).toBe(true);
    expect(graphState.get(key)).toEqual({ v: 2 });
  });

  it("does not drop a root whose id is merely a prefix of the removed id", () => {
    // The trailing slash in folder:${root_id}/ prevents removing "r1" from
    // matching a snapshot under "r10" (a prefix collision without the slash).
    const r10: ScopeKeyInput = {
      kind: "folder",
      root_id: "r10",
      folder: "x",
    };
    const key = graphStateKey(r10, ["t"]);
    graphState.set(key, { v: 3 });
    invalidateScopedGraphs("r1");
    expect(graphState.has(key)).toBe(true);
  });
});
