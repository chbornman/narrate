/**
 * View-mode axis flows (DESIGN-VIEW-MODES.md): the unified
 * grid / visualizer / look axis replacing the old `surface` enum + `graphOpen`
 * overlay boolean, plus the shared activeHash that survives a view switch and
 * the R6 seed-from-active reconciliation. Pinned against mocked IPC over the
 * composition root (the graph-store.test.ts pattern). The pure scope precedence
 * is covered in scope.test.ts; here we test the cross-slice view transitions.
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
      case "search":
        return {
          query: { raw: args?.query, filters: args?.filters ?? [], dropped: [], fallback: false },
          images: ["b"].map((h) => ({
            image_hash: h,
            preview: h,
            score: 1,
            provenance: null,
            last_annotated_ts: null,
            debug: null,
          })),
          session_hits: [],
        };
      case "find_similar":
        return ["c", "d"];
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
      case "list_folder":
        // The "second folder" the rail opens in scenario (4): two fresh items.
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
  // a, b, c, d solo JPEGs.
  ui.grid.rawItems = ["a", "b", "c", "d"].map(item);
});

// (1) activeHash continuity across a switch chain keeps the same photo.
describe("activeHash survives view switches (the shared active image)", () => {
  it("grid -> visualizer -> look -> grid keeps photo 'b' active throughout", async () => {
    // Focus grid cell "b" (index 1): grid.activeHash = "b".
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 1));
    expect(ui.actionContext().activeHash).toBe("b");

    // l: grid -> visualizer. R6 seeds viewSelection from the active "b".
    await ui.perform({ kind: "toggle-graph" });
    expect(ui.viewMode).toBe("visualizer");
    expect(ui.viewSelection).toBe("b");
    expect(ui.actionContext().activeHash).toBe("b");

    // Enter on the selected node: visualizer -> look on "b" (openFromGraph).
    await ui.openFromGraph(ui.viewSelection!);
    expect(ui.viewMode).toBe("look");
    expect(ui.look.currentHash).toBe("b");
    expect(ui.actionContext().activeHash).toBe("b");

    // Esc / leave-look: look -> grid, same image active (focus restored to "b").
    await ui.leaveLook();
    expect(ui.viewMode).toBe("grid");
    expect(ui.actionContext().activeHash).toBe("b");
  });
});

// (2) `l` toggles grid <-> visualizer.
describe("l toggles grid <-> visualizer", () => {
  it("first l opens the visualizer, a second l returns to the grid", async () => {
    expect(ui.viewMode).toBe("grid");
    await ui.perform({ kind: "toggle-graph" });
    expect(ui.viewMode).toBe("visualizer");
    await ui.perform({ kind: "toggle-graph" });
    expect(ui.viewMode).toBe("grid");
  });
});

// (3) `l` from Look opens the visualizer seeded from look.currentHash.
describe("l from Look enters the visualizer seeded from the Look image", () => {
  it("look -> visualizer carries the viewed photo across", async () => {
    await ui.openLook("c");
    expect(ui.viewMode).toBe("look");
    expect(ui.look.currentHash).toBe("c");
    await ui.perform({ kind: "toggle-graph" });
    expect(ui.viewMode).toBe("visualizer");
    // openVisualizer left Look first, then seeded from the Look image.
    expect(ui.viewSelection).toBe("c");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["c"]);
  });
});

// (4) scope persistence under a view: openFolder in visualizer STAYS visualizer
// (re-points at the new scope); openFolder in look DROPS to grid.
describe("rail scope-select persists the visualizer, drops Look", () => {
  it("openFolder while in the visualizer keeps the view and takes the new gridScope", async () => {
    await ui.perform({ kind: "toggle-graph" }); // grid -> visualizer
    expect(ui.viewMode).toBe("visualizer");
    await ui.openFolder("r2", "trip");
    // The visualizer PERSISTS (DESIGN-VIEW-MODES.md transition table (i)); the
    // grid scope underneath it is the freshly-opened folder.
    expect(ui.viewMode).toBe("visualizer");
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "r2", folder: "trip" });
  });

  it("openFolder while in Look drops to the grid on the new folder", async () => {
    await ui.openLook("a");
    expect(ui.viewMode).toBe("look");
    await ui.openFolder("r2", "trip");
    expect(ui.viewMode).toBe("grid");
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "r2", folder: "trip" });
  });
});

// (5) Esc ladder in the visualizer (component-local: deselect-first then leave).
describe("the visualizer Esc ladder (deselect first, then leave to grid)", () => {
  it("with a selection, the FIRST Esc deselects and stays in the visualizer", async () => {
    await ui.perform({ kind: "toggle-graph" });
    await ui.selectGraphNode("b");
    expect(ui.viewSelection).toBe("b");
    // The TopicGraph Esc handler: a selection deselects first, view unchanged.
    await ui.selectGraphNode(null);
    expect(ui.viewSelection).toBe(null);
    expect(ui.viewMode).toBe("visualizer");
  });

  it("with nothing selected, Esc leaves the visualizer to the grid", async () => {
    await ui.perform({ kind: "toggle-graph" });
    expect(ui.viewSelection).toBe(null);
    // The second Esc (nothing selected) routes through leaveVisualizer.
    await ui.leaveVisualizer();
    expect(ui.viewMode).toBe("grid");
  });
});

// (6) scope-swap-under-look survives: commit query -> open look -> find-similar
// drops to the grid on the new `similar` scope.
describe("scope-swap under Look survives (find-similar from Look)", () => {
  it("commit a query, open Look, find-similar lands on grid with the similar scope", async () => {
    // Commit a query scope (the mock returns hash "b").
    ui.query = "harbor";
    await ui.runQueryScope("semantic");
    expect(ui.gridScope.kind).toBe("query");

    // Open Look on the one result, then "find similar" from the Look backdrop.
    await ui.openLook("b");
    expect(ui.viewMode).toBe("look");
    await ui.perform({ kind: "find-similar" });

    // find-similar leaves Look first (viewMode grid) and re-scopes the grid to
    // the visual neighbors (the `similar` scope), under-Look swap surviving.
    expect(ui.viewMode).toBe("grid");
    expect(ui.gridScope.kind).toBe("similar");
  });
});
