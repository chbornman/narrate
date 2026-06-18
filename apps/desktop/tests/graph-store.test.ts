/**
 * Semantic topic-graph store flows (DESIGN-SEMANTIC-GRAPH.md): the small,
 * clearly-named region added to the composition root — open/close the lens,
 * derive the backend GraphScope from the current grid scope, scope-to-topic
 * (a topic anchor click → committed semantic query), and open-from-graph (an
 * image node click → Look). The pure force sim and the affinity blend are
 * tested separately (forcegraph.test.ts, the Rust topic tests); here we pin the
 * frontend wiring against mocked IPC.
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
      case "set_scope":
        return { kind: "session", count: 0, previewHashes: [] };
      case "search":
        return {
          query: { raw: args?.query, filters: args?.filters, dropped: [], fallback: false },
          images: [{ image_hash: "x", score: 1, provenance: null, debug: null }],
          session_hits: [],
        };
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
      case "folder_tree":
      case "list_roots":
        return [];
      case "graph_tuning":
        return {
          alpha_default: 0.5,
          attraction: 0.02,
          repulsion: 800,
          damping: 0.85,
          centering: 0.01,
          ring_radius: 320,
        };
      case "topic_affinities":
        return {
          images: [{ image_hash: "x", scores: [{ topic: 0, affinity: 0.9 }] }],
          visual_ready: true,
          annotation_ready: true,
        };
      case "suggest_topics":
        return [{ phrase: "harbor", source: "note", count: 3 }];
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
  ui.grid.rawItems = ["a", "b", "x"].map(item);
});

describe("visualizer view open/close", () => {
  it("openVisualizer enters the view; leaveVisualizer returns to grid", async () => {
    expect(ui.viewMode).not.toBe("visualizer");
    await ui.openVisualizer();
    expect(ui.viewMode).toBe("visualizer");
    await ui.leaveVisualizer();
    expect(ui.viewMode).not.toBe("visualizer");
  });

  it("openVisualizer leaves Look first (peer view: it replaces, not overlays)", async () => {
    ui.viewMode = "look";
    await ui.openVisualizer();
    expect(ui.viewMode).toBe("visualizer");
  });

  it("go-grid (G / goHome) CLOSES the lens (founder bug: G left it hidden behind)", async () => {
    await ui.openVisualizer();
    expect(ui.viewMode).toBe("visualizer");
    // The go-grid action routes through goHome, which must drop the lens so the
    // user lands on the grid, not stay behind the still-open Visualizer.
    await ui.perform({ kind: "go-grid" });
    expect(ui.viewMode).not.toBe("visualizer");
  });

  it("goHome closes the lens even when there is no derived scope to clear", async () => {
    await ui.openVisualizer();
    await ui.goHome();
    expect(ui.viewMode).not.toBe("visualizer");
  });
});

describe("graph node selection → write scope (DESIGN-VIEW-MODES.md R6)", () => {
  it("opening the visualizer SEEDS the selection from the active photo (not stale, not neutralized)", async () => {
    // R6 seed-from-active: a grid cell is focused/active underneath; opening the
    // visualizer carries THAT photo across (viewSelection = active), so dictation
    // and rating continue on it. This replaces the old unconditional neutralize.
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0)); // "a" active
    await ui.openVisualizer();
    expect(ui.viewSelection).toBe("a");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["a"]);
  });

  it("opening the visualizer with NOTHING active opens NEUTRAL (empty scope)", async () => {
    // No grid focus/selection: nothing to carry, so the visualizer opens neutral
    // (viewSelection null, scope []) and a dictation becomes a session note.
    ui.grid.sel = sel.EMPTY;
    await ui.openVisualizer();
    expect(ui.viewSelection).toBe(null);
    expect(lastCall("set_scope")?.args?.targets).toEqual([]);
  });

  it("selecting a node reports that hash; deselecting returns to empty", async () => {
    await ui.openVisualizer();
    await ui.selectGraphNode("b");
    expect(ui.viewSelection).toBe("b");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["b"]);
    // Deselect (null) → neutral session scope again.
    await ui.selectGraphNode(null);
    expect(ui.viewSelection).toBe(null);
    expect(lastCall("set_scope")?.args?.targets).toEqual([]);
  });

  it("a graph selection OVERRIDES the grid selection underneath", async () => {
    // Select grid cell 0 (hash "a"), then open the graph and select "x": the
    // reported scope is the graph node, not the grid's "a".
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    await ui.openVisualizer();
    await ui.selectGraphNode("x");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["x"]);
  });

  it("closing the graph clears the selection and re-reports the grid scope", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0)); // "a"
    await ui.openVisualizer();
    await ui.selectGraphNode("x");
    await ui.leaveVisualizer();
    expect(ui.viewSelection).toBe(null);
    // Scope returns to the grid selection underneath.
    expect(lastCall("set_scope")?.args?.targets).toEqual(["a"]);
  });

  it("Enter / double-click route (openFromGraph) opens the selected node in Look", async () => {
    await ui.openVisualizer();
    await ui.selectGraphNode("b");
    // openFromGraph is the shared target of the dblclick gesture and the Enter
    // key; it closes the lens and opens Look on the selected hash.
    await ui.openFromGraph(ui.viewSelection!);
    expect(ui.viewMode).not.toBe("visualizer");
    expect(ui.viewSelection).toBe(null);
    expect(ui.viewMode).toBe("look");
    expect(ui.look.currentHash).toBe("b");
  });
});

describe("graphScope derivation from the current grid scope", () => {
  it("a folder scope maps to a folder GraphScope (snake_case fields)", () => {
    ui.gridScope = { kind: "folder", rootId: "r1", folder: "iceland" };
    expect(ui.graphScope()).toEqual({ kind: "folder", root_id: "r1", folder: "iceland" });
  });

  it("a collection scope maps to a collection GraphScope", () => {
    ui.gridScope = { kind: "collection", id: "c1" };
    expect(ui.graphScope()).toEqual({ kind: "collection", id: "c1" });
  });

  it("a query/similar scope maps to its RESULT HASHES (option a, the lens follows the result)", () => {
    ui.gridScope = {
      kind: "query",
      query: "fog",
      chips: [],
      within: { kind: "collection", id: "c2" },
    };
    // The grid result is what the lens scopes to now (not the source collection
    // underneath, and never the whole library). rawItems is seeded in
    // beforeEach to ["a","b","x"].
    expect(ui.graphScope()).toEqual({ kind: "hashes", hashes: ["a", "b", "x"] });
  });
});

describe("topic anchor click → scope the grid to the topic", () => {
  it("scopeToTopic commits a SEMANTIC query of the topic phrase and closes the lens", async () => {
    await ui.openVisualizer();
    await ui.scopeToTopic("harbor at dusk");
    // The lens closed and the grid is now a committed query scope.
    expect(ui.viewMode).not.toBe("visualizer");
    expect(ui.gridScope.kind).toBe("query");
    // The search ran on the SEMANTIC lane (DESIGN: topic anchor scopes to its
    // strongest matches in fused order).
    const call = lastCall("search");
    expect(call?.args?.query).toBe("harbor at dusk");
    expect(call?.args?.mode).toBe("semantic");
    // The query scope's residue + Escape-to-clear now apply (the query feeder).
    expect(ui.query).toBe("harbor at dusk");
  });
});

describe("image node click → open in Look", () => {
  it("openFromGraph closes the lens and opens the image in Look", async () => {
    await ui.openVisualizer();
    await ui.openFromGraph("b");
    expect(ui.viewMode).not.toBe("visualizer");
    expect(ui.viewMode).toBe("look");
    expect(ui.look.currentHash).toBe("b");
  });
});
