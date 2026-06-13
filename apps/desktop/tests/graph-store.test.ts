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

describe("graph lens open/close", () => {
  it("openGraph sets the flag; closeGraph clears it", async () => {
    expect(ui.graphOpen).toBe(false);
    await ui.openGraph();
    expect(ui.graphOpen).toBe(true);
    ui.closeGraph();
    expect(ui.graphOpen).toBe(false);
  });

  it("openGraph leaves Look first (the lens is a grid-level overlay)", async () => {
    ui.surface = "look";
    await ui.openGraph();
    expect(ui.surface).toBe("grid");
    expect(ui.graphOpen).toBe(true);
  });

  it("go-grid (G / goHome) CLOSES the lens (founder bug: G left it hidden behind)", async () => {
    await ui.openGraph();
    expect(ui.graphOpen).toBe(true);
    // The go-grid action routes through goHome, which must drop the lens so the
    // user lands on the grid, not stay behind the still-open Visualizer.
    await ui.perform({ kind: "go-grid" });
    expect(ui.graphOpen).toBe(false);
  });

  it("goHome closes the lens even when there is no derived scope to clear", async () => {
    await ui.openGraph();
    await ui.goHome();
    expect(ui.graphOpen).toBe(false);
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

  it("a query/similar scope unwraps to its underlying source", () => {
    ui.gridScope = {
      kind: "query",
      query: "fog",
      chips: [],
      within: { kind: "collection", id: "c2" },
    };
    expect(ui.graphScope()).toEqual({ kind: "collection", id: "c2" });
  });
});

describe("topic anchor click → scope the grid to the topic", () => {
  it("scopeToTopic commits a SEMANTIC query of the topic phrase and closes the lens", async () => {
    await ui.openGraph();
    await ui.scopeToTopic("harbor at dusk");
    // The lens closed and the grid is now a committed query scope.
    expect(ui.graphOpen).toBe(false);
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
    await ui.openGraph();
    await ui.openFromGraph("b");
    expect(ui.graphOpen).toBe(false);
    expect(ui.surface).toBe("look");
    expect(ui.look.currentHash).toBe("b");
  });
});
