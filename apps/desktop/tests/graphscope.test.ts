/**
 * graphScope() — the grid scope -> backend GraphScope transition (P5 option (a):
 * scope the lenses to the actual SEARCH RESULT / B4 / STATE-MACHINE 6f). The bug
 * this pins: a query/similar scope used to either silently WIDEN the lens / dedup
 * / diversify pass to the WHOLE library (the original fallback), or REFUSE it
 * entirely (the interim fix) — both wrong. The founder's choice is option (a):
 * the lenses operate on EXACTLY the search result.
 *
 * The fix: the backend GraphScope enum gained a `Hashes { hashes }` variant (see
 * commands/graph.rs), so a bare result set IS expressible. graphScope() now
 * returns `{ kind: "hashes", hashes }` (the current grid result hashes) for a
 * query/similar scope, so the visualizer / dedup / diversify compute over the
 * result the reviewer is looking at — not the underlying folder, and never the
 * whole library by accident.
 *
 * We assert:
 *   - a plain folder / collection scope resolves UNCHANGED (no behavior change);
 *   - a query / similar scope resolves to its RESULT HASHES (option (a));
 *   - a topic scope OVER a folder/collection still unwraps to that SOURCE (the
 *     topic rank reads this to know WHAT to rank — kept exactly as before);
 *   - a truly-EMPTY query/similar result (no hashes at all) still REFUSES (null),
 *     so the calm empty-scope affordances still cover the genuinely-empty case;
 *   - it is never the silent `{kind:"library"}` widening.
 *
 * graphScope() reads the gridScope shape + grid.scopeHashes, so we drive
 * ui.gridScope and ui.grid.rawItems directly (the same direct-set the
 * scope-subject / graph-scoping suites use) — no IPC needed.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GridScope } from "../src/lib/state/app.svelte";
import type { GridItem } from "../src/lib/types/dto";

// The Ui constructor touches the IPC layer (prefs/roots reads) on build; mock it
// to a quiet no-op so the unit under test (a pure method) runs without a backend.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";

// A minimal GridItem from a hash (only `.hash` feeds grid.scopeHashes; the rest
// satisfies the type) — the graph-scoping suite's helper.
const item = (hash: string): GridItem => ({
  hash,
  fileName: `${hash}.jpg`,
  relPath: `${hash}.jpg`,
  captureTs: null,
  addedTs: "2026-02-01T00:00:00Z",
  hasJournal: false,
  rating: null,
  offline: false,
});

let ui: Ui;
beforeEach(() => {
  localStorage.clear();
  ui = new Ui();
});

describe("graphScope() resolves a plain source scope unchanged", () => {
  it("a folder scope maps to the snake_case folder GraphScope", () => {
    ui.gridScope = { kind: "folder", rootId: "r1", folder: "trip" };
    ui.grid.rootId = "r1";
    ui.grid.folder = "trip";
    expect(ui.graphScope()).toEqual({
      kind: "folder",
      root_id: "r1",
      folder: "trip",
    });
  });

  it("a collection scope maps to the collection GraphScope", () => {
    ui.gridScope = { kind: "collection", id: "coll01" };
    expect(ui.graphScope()).toEqual({ kind: "collection", id: "coll01" });
  });
});

describe("a query / similar scope resolves to its RESULT HASHES (option a)", () => {
  it("a query scopes to the current grid result hashes, not the source folder", () => {
    const within: GridScope = { kind: "folder", rootId: "r1", folder: "trip" };
    ui.gridScope = { kind: "query", query: "boats", chips: [], within };
    // The committed query's result is what the grid is showing.
    ui.grid.rawItems = ["a", "b", "c"].map(item);
    expect(ui.graphScope()).toEqual({
      kind: "hashes",
      hashes: ["a", "b", "c"],
    });
  });

  it("a 'similar' view scopes to its result hashes (in result order)", () => {
    const within: GridScope = { kind: "collection", id: "coll01" };
    ui.gridScope = { kind: "similar", hash: "h1", filename: "h1.jpg", within };
    // Order is preserved (the neighbor/relevance order the lens cares about).
    ui.grid.rawItems = ["x", "y"].map(item);
    expect(ui.graphScope()).toEqual({ kind: "hashes", hashes: ["x", "y"] });
  });
});

describe("a topic scope still unwraps to its SOURCE (topic-over-a-source)", () => {
  it("a topic within a folder resolves to the FOLDER (the rank reads this)", () => {
    const within: GridScope = { kind: "folder", rootId: "r2", folder: "" };
    ui.gridScope = { kind: "topic", phrase: "sunset", within };
    // Even with items loaded, a topic resolves to its source so its rank knows
    // WHAT to rank — it must not scope to its own (already-ranked) result.
    ui.grid.rawItems = ["a", "b"].map(item);
    expect(ui.graphScope()).toEqual({
      kind: "folder",
      root_id: "r2",
      folder: "",
    });
  });
});

describe("a truly-EMPTY query/similar result still REFUSES (the calm boundary)", () => {
  it("an empty query result returns null, never {kind:'hashes'} or {kind:'library'}", () => {
    const within: GridScope = { kind: "folder", rootId: "r1", folder: "trip" };
    ui.gridScope = { kind: "query", query: "no matches", chips: [], within };
    // No result hashes at all: nothing to scope to, so the lens no-ops calmly.
    ui.grid.rawItems = [];
    const scope = ui.graphScope();
    expect(scope).toBeNull();
    expect(scope).not.toEqual({ kind: "library" });
  });

  it("a derived scope with no nameable source and no hashes also refuses", () => {
    // The §6f "source folder removed" shape: a derived scope whose `within` is
    // itself a non-source. With no result hashes either, it refuses (null)
    // rather than widen to the whole library.
    const within = { kind: "query", query: "x", chips: [], within: {} } as unknown as GridScope;
    ui.gridScope = { kind: "topic", phrase: "sunset", within };
    ui.grid.rawItems = [];
    const scope = ui.graphScope();
    expect(scope).toBeNull();
    expect(scope).not.toEqual({ kind: "library" });
  });
});
