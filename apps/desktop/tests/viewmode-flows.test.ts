/**
 * View-mode axis: multi-step transition CHAINS + scope/activeHash continuity
 * (DESIGN-VIEW-MODES.md transition table). The single-hop transitions and the
 * seed-from-active reconciliation live in viewmode.test.ts / graph-store.test.ts;
 * this file fills the GAPS the task calls out:
 *   - longer chains (grid -> visualizer -> look -> grid -> visualizer ...) that
 *     KEEP the same photo active throughout (activeHash continuity);
 *   - the underlying gridScope (the noun) is PRESERVED across every lens switch
 *     (the lens axis is orthogonal to the scope axis);
 *   - openVisualizer seeds from the active image (scope=[active]) vs neutral;
 *   - openFolder/openCollection while in the visualizer PERSISTS the visualizer,
 *     while in Look DROPS to grid (the rail-scope-select rule), asserted over a
 *     non-folder scope (collection) too;
 *   - the full Esc ladder in the visualizer (deselect, THEN leave to grid).
 *
 * Pinned against mocked IPC over the composition root (the graph-store.test.ts
 * pattern), reusing its set_scope echo shape so scope targets can be read back.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GridItem } from "../src/lib/types/dto";

const ipcLog = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    ipcLog.calls.push({ cmd, args });
    switch (cmd) {
      case "set_scope": {
        const targets = (args?.targets ?? []) as string[];
        const kind =
          targets.length === 0 ? "session" : targets.length === 1 ? "single" : "multi";
        return { kind, count: targets.length, previewHashes: targets.slice(0, 8) };
      }
      case "list_folder":
        // The "second folder" the rail opens: two fresh items p, q.
        return ["p", "q"].map((h) => ({
          hash: h,
          fileName: `${h}.jpg`,
          relPath: `${h}.jpg`,
          captureTs: null,
          addedTs: "2026-02-01T00:00:00Z",
          hasJournal: false,
          rating: null,
          offline: false,
        }));
      case "list_collection_members":
        // The collection the rail opens: two fresh items m, n.
        return ["m", "n"].map((h) => ({
          hash: h,
          fileName: `${h}.jpg`,
          relPath: `${h}.jpg`,
          captureTs: null,
          addedTs: "2026-02-01T00:00:00Z",
          hasJournal: false,
          rating: null,
          offline: false,
        }));
      case "list_images":
        return ((args?.hashes ?? []) as string[]).map((h) => ({
          hash: h,
          fileName: `${h}.jpg`,
          relPath: `${h}.jpg`,
          captureTs: null,
          addedTs: "2026-02-01T00:00:00Z",
          hasJournal: false,
          rating: null,
          offline: false,
        }));
      case "folder_tree":
      case "list_roots":
        return [];
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";
import * as sel from "../src/lib/logic/selection";

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

const lastCall = (cmd: string) =>
  [...ipcLog.calls].reverse().find((c) => c.cmd === cmd);

let ui: Ui;
beforeEach(() => {
  ipcLog.calls.length = 0;
  localStorage.clear();
  ui = new Ui();
  // a, b, c, d solo JPEGs in a folder scope.
  ui.grid.rawItems = ["a", "b", "c", "d"].map(item);
  ui.gridScope = { kind: "folder", rootId: "r1", folder: "trip" };
});

// (1) A LONGER chain than viewmode.test.ts covers: grid -> visualizer -> look
// -> grid -> visualizer -> look on the SAME photo, asserting both the
// activeHash AND the scope echo (set_scope targets) stay on that one image.
describe("a long lens chain keeps one photo active and singly-scoped throughout", () => {
  it("grid->vis->look->grid->vis->look all stay on 'c' (activeHash + scope echo)", async () => {
    // Focus grid cell "c" (index 2).
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 2));
    expect(ui.actionContext().activeHash).toBe("c");

    // l: grid -> visualizer, seeded from active "c".
    await ui.perform({ kind: "toggle-graph" });
    expect(ui.viewMode).toBe("visualizer");
    expect(ui.actionContext().activeHash).toBe("c");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["c"]);

    // Enter: visualizer -> look on "c".
    await ui.openFromGraph(ui.viewSelection!);
    expect(ui.viewMode).toBe("look");
    expect(ui.actionContext().activeHash).toBe("c");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["c"]);

    // Esc: look -> grid, "c" still active (focus restored).
    await ui.leaveLook();
    expect(ui.viewMode).toBe("grid");
    expect(ui.actionContext().activeHash).toBe("c");

    // l again: grid -> visualizer, re-seeded from "c".
    await ui.perform({ kind: "toggle-graph" });
    expect(ui.viewMode).toBe("visualizer");
    expect(ui.actionContext().activeHash).toBe("c");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["c"]);

    // Enter again: visualizer -> look on "c".
    await ui.openFromGraph(ui.viewSelection!);
    expect(ui.viewMode).toBe("look");
    expect(ui.look.currentHash).toBe("c");
    expect(ui.actionContext().activeHash).toBe("c");
  });
});

// (2) The lens axis is ORTHOGONAL to the scope noun: the gridScope the user is
// in must survive EVERY lens switch untouched. viewmode.test.ts asserts
// activeHash continuity but never that the scope noun underneath is preserved.
describe("the gridScope (the noun) survives every lens switch", () => {
  it("a query scope is intact through grid->vis->look->grid", async () => {
    // Stand in a derived (query) scope over the folder.
    ui.gridScope = {
      kind: "query",
      query: "harbor",
      chips: [],
      within: { kind: "folder", rootId: "r1", folder: "trip" },
    };
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0)); // "a"

    await ui.perform({ kind: "toggle-graph" }); // -> visualizer
    expect(ui.gridScope).toEqual({
      kind: "query",
      query: "harbor",
      chips: [],
      within: { kind: "folder", rootId: "r1", folder: "trip" },
    });

    await ui.openFromGraph(ui.viewSelection!); // -> look
    expect(ui.gridScope.kind).toBe("query");

    await ui.leaveLook(); // -> grid
    expect(ui.gridScope).toEqual({
      kind: "query",
      query: "harbor",
      chips: [],
      within: { kind: "folder", rootId: "r1", folder: "trip" },
    });
  });
});

// (3) Seed-from-active reconciliation across the l-toggle: a SECOND `l` after a
// deselect must reopen NEUTRAL (nothing carried), not resurrect the old node.
describe("the l-toggle seeds fresh each open (no stale carry after a deselect)", () => {
  it("open seeded from 'b'; deselect; leave; reopen with nothing focused = neutral", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 1)); // "b"
    await ui.perform({ kind: "toggle-graph" }); // -> visualizer, seed "b"
    expect(ui.viewSelection).toBe("b");

    // Deselect inside the visualizer, then leave back to the grid.
    await ui.selectGraphNode(null);
    expect(ui.viewSelection).toBe(null);
    await ui.leaveVisualizer(); // -> grid; seeds grid focus from null (no move)

    // Clear grid focus so nothing is active, then reopen: must be neutral.
    ui.grid.sel = sel.EMPTY;
    await ui.perform({ kind: "toggle-graph" }); // -> visualizer
    expect(ui.viewSelection).toBe(null);
    expect(lastCall("set_scope")?.args?.targets).toEqual([]);
  });
});

// (4) Rail scope-select rule over a COLLECTION (viewmode.test.ts uses a folder):
// openCollection in the visualizer PERSISTS the lens and re-points the scope;
// openCollection in Look DROPS to grid.
describe("openCollection obeys the rail rule (persist visualizer, drop Look)", () => {
  it("openCollection while in the visualizer keeps the lens, swaps the noun", async () => {
    await ui.perform({ kind: "toggle-graph" }); // grid -> visualizer
    expect(ui.viewMode).toBe("visualizer");
    await ui.openCollection("coll1");
    expect(ui.viewMode).toBe("visualizer");
    expect(ui.gridScope).toEqual({ kind: "collection", id: "coll1" });
  });

  it("openCollection while in Look drops to the grid on the collection", async () => {
    await ui.openLook("a");
    expect(ui.viewMode).toBe("look");
    await ui.openCollection("coll1");
    expect(ui.viewMode).toBe("grid");
    expect(ui.gridScope).toEqual({ kind: "collection", id: "coll1" });
  });
});

// (5) The COMPLETE Esc ladder as one sequence: a selected visualizer takes TWO
// Escs to reach the grid (first deselects in place, second leaves). viewmode.test.ts
// asserts the two halves separately; this pins the ordered ladder end to end.
describe("the full visualizer Esc ladder, in one sequence", () => {
  it("Esc deselects first (stay), then Esc leaves to the grid", async () => {
    await ui.perform({ kind: "toggle-graph" }); // -> visualizer
    await ui.selectGraphNode("b");
    expect(ui.viewSelection).toBe("b");
    expect(ui.viewMode).toBe("visualizer");

    // First Esc: deselect, view UNCHANGED, scope back to neutral.
    await ui.selectGraphNode(null);
    expect(ui.viewSelection).toBe(null);
    expect(ui.viewMode).toBe("visualizer");
    expect(lastCall("set_scope")?.args?.targets).toEqual([]);

    // Second Esc (nothing selected): leave to the grid.
    await ui.leaveVisualizer();
    expect(ui.viewMode).toBe("grid");
  });
});

// (6) goHome (G) from the visualizer with a DERIVED scope underneath lands on the
// plain grid AND clears the derived scope to its source (the fall-through in
// goHome). graph-store.test.ts only checks G closes the lens over a plain scope;
// this pins the lens-close + scope-clear together.
describe("G from the visualizer closes the lens AND clears a derived scope", () => {
  it("query-scope under the visualizer: G lands on the plain folder grid", async () => {
    ui.gridScope = {
      kind: "query",
      query: "fog",
      chips: [],
      within: { kind: "folder", rootId: "r1", folder: "trip" },
    };
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    await ui.perform({ kind: "toggle-graph" }); // -> visualizer
    expect(ui.viewMode).toBe("visualizer");

    await ui.perform({ kind: "go-grid" });
    expect(ui.viewMode).toBe("grid");
    // The derived query scope cleared to its underlying folder source.
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "r1", folder: "trip" });
  });
});
