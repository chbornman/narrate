/**
 * graphScope() — the grid scope -> backend GraphScope transition (P5 / B4 /
 * STATE-MACHINE 6f). The bug this pins: a query/similar scope whose source does
 * NOT resolve to a folder/collection used to fall back to `{kind:"library"}`,
 * silently WIDENING the lens / dedup / diversify pass from the result the user
 * is looking at to the WHOLE library (a scale spike by accident).
 *
 * The fix makes that transition EXPLICIT: the backend GraphScope enum is
 * folder | collection | library only (no result-set / hash-list variant — see
 * commands/graph.rs), so there is nothing to scope a bare result set TO; rather
 * than widen, graphScope() REFUSES (returns null) and the callers no-op calmly.
 *
 * We assert:
 *   - a plain folder / collection scope resolves UNCHANGED (no behavior change);
 *   - a query / similar / topic scope OVER a folder/collection unwraps to that
 *     SOURCE (the common case stays exactly as before);
 *   - a derived scope whose `within` is NOT a folder/collection resolves to null
 *     (REFUSE), never `{kind:"library"}` — the silent-widening guard.
 *
 * graphScope() is pure over the gridScope shape, so we drive ui.gridScope
 * directly (the same direct-set the scope-subject suite uses) — no IPC needed.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GridScope } from "../src/lib/state/app.svelte";

// The Ui constructor touches the IPC layer (prefs/roots reads) on build; mock it
// to a quiet no-op so the unit under test (a pure method) runs without a backend.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";

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

describe("a derived scope OVER a folder/collection unwraps to that source", () => {
  it("a query within a folder resolves to the FOLDER (not library)", () => {
    const within: GridScope = { kind: "folder", rootId: "r1", folder: "trip" };
    ui.gridScope = { kind: "query", query: "boats", chips: [], within };
    expect(ui.graphScope()).toEqual({
      kind: "folder",
      root_id: "r1",
      folder: "trip",
    });
  });

  it("a 'similar' within a collection resolves to the COLLECTION", () => {
    const within: GridScope = { kind: "collection", id: "coll01" };
    ui.gridScope = { kind: "similar", hash: "h1", filename: "h1.jpg", within };
    expect(ui.graphScope()).toEqual({ kind: "collection", id: "coll01" });
  });

  it("a topic within a folder resolves to the FOLDER", () => {
    const within: GridScope = { kind: "folder", rootId: "r2", folder: "" };
    ui.gridScope = { kind: "topic", phrase: "sunset", within };
    expect(ui.graphScope()).toEqual({
      kind: "folder",
      root_id: "r2",
      folder: "",
    });
  });
});

describe("a derived scope with NO folder/collection source REFUSES", () => {
  it("returns null (the explicit refuse), never {kind:'library'}", () => {
    // The §6f "source folder removed" shape: a derived scope whose `within` is
    // itself a non-source (here a bare query carrying a query within). The old
    // code fell through to {kind:"library"} and silently scanned everything; the
    // fix refuses so the lens/dedup/diversify no-op instead of widening.
    const within = { kind: "query", query: "x", chips: [], within: {} } as unknown as GridScope;
    ui.gridScope = { kind: "query", query: "boats", chips: [], within };
    const scope = ui.graphScope();
    expect(scope).toBeNull();
    // Belt-and-suspenders: it must NOT be the silent library widening.
    expect(scope).not.toEqual({ kind: "library" });
  });
});
