/**
 * Topics + the topic -> collection bake, the composition-root wiring
 * (DESIGN-TOPICS-COLLECTIONS.md) against mocked IPC. Pins:
 *  - the `topic` gridScope feeds the grid in RANKED (descending-affinity)
 *    order, with the residue/clear machinery (residue active, Escape/G return
 *    to source);
 *  - manual-topic CRUD (add/remove) round-trips through the store;
 *  - the bake calls create_collection_from_topic with the right threshold +
 *    phrase, and refreshes the collections snapshot.
 * The pure threshold math is tested in topicbake.test.ts.
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
      case "topic_ranked_images":
        // Ranked, descending affinity (the backend's contract). The store maps
        // these to GridItems via list_images and feeds them in this order.
        return [
          { hash: "r1", score: 0.9 },
          { hash: "r2", score: 0.6 },
          { hash: "r3", score: 0.3 },
        ];
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
      case "list_topics":
        return ipcLog.calls.some((c) => c.cmd === "add_topic")
          ? [{ id: "t1", phrase: "harbor at dusk", space: "blend", createdTs: "2026-06-13T00:00:00Z" }]
          : [];
      case "add_topic":
        return { id: "t1", phrase: args?.phrase, space: "blend", createdTs: "2026-06-13T00:00:00Z" };
      case "remove_topic":
        return null;
      case "create_collection_from_topic":
        return { id: "c-baked", name: args?.name, status: "active", memberCount: 2 };
      case "list_collections":
        return [];
      case "list_folder":
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
  ui.grid.rawItems = ["a", "b"].map(item);
  // A folder source so the topic scope's `within` (and the graphScope it bakes
  // against) resolve to a real folder, not the library fallback.
  ui.gridScope = { kind: "folder", rootId: "r1", folder: "harbor" };
});

describe("topic gridScope feeds the grid in ranked order", () => {
  it("runTopicScope sets a topic scope and feeds ranked hashes in order", async () => {
    await ui.runTopicScope("harbor at dusk");
    expect(ui.gridScope.kind).toBe("topic");
    if (ui.gridScope.kind === "topic") {
      expect(ui.gridScope.phrase).toBe("harbor at dusk");
      // The source the residue/clear returns to is the underlying folder.
      expect(ui.gridScope.within).toEqual({ kind: "folder", rootId: "r1", folder: "harbor" });
    }
    // topic_ranked_images was queried against the derived GraphScope.
    const ranked = lastCall("topic_ranked_images");
    expect(ranked?.args?.phrase).toBe("harbor at dusk");
    expect(ranked?.args?.scope).toEqual({ kind: "folder", root_id: "r1", folder: "harbor" });
    // The grid was fed the ranked hashes IN ORDER (descending affinity), via
    // list_images; relevance sort is a pass-through so the order survives.
    expect(ui.grid.rawItems.map((i) => i.hash)).toEqual(["r1", "r2", "r3"]);
    expect(ui.grid.sort).toBe("relevance");
    // The scores are cached for the Topics-tab bake bar (same numbers the grid
    // was fed), descending.
    expect(ui.topicScored).toEqual([
      { hash: "r1", score: 0.9 },
      { hash: "r2", score: 0.6 },
      { hash: "r3", score: 0.3 },
    ]);
  });

  it("a topic scope is a derived scope: residue active, clear returns to source", async () => {
    await ui.runTopicScope("harbor at dusk");
    // The escape/action contexts treat it as a relevance-ordered derived scope.
    expect(ui.escapeContext().queryScopeActive).toBe(true);
    expect(ui.actionContext().queryActive).toBe(true);
    // One-key clear / Escape / G returns to the underlying folder source.
    await ui.clearQueryScope();
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "r1", folder: "harbor" });
  });

  it("openTopic leaves Look first then scopes (the rail drives the grid)", async () => {
    ui.surface = "look";
    await ui.openTopic("harbor at dusk");
    expect(ui.surface).toBe("grid");
    expect(ui.gridScope.kind).toBe("topic");
  });
});

describe("manual-topic CRUD", () => {
  it("addTopic saves the phrase and refreshes the snapshot", async () => {
    expect(ui.topics).toEqual([]);
    await ui.addTopic("harbor at dusk");
    expect(lastCall("add_topic")?.args?.phrase).toBe("harbor at dusk");
    expect(ui.topics.map((t) => t.phrase)).toEqual(["harbor at dusk"]);
  });

  it("addTopic ignores a blank or duplicate phrase (no IPC)", async () => {
    await ui.addTopic("   ");
    expect(ipcLog.calls.some((c) => c.cmd === "add_topic")).toBe(false);
    ui.topics = [{ id: "t1", phrase: "dupe", space: "blend", createdTs: "x" }];
    await ui.addTopic("dupe");
    expect(ipcLog.calls.some((c) => c.cmd === "add_topic")).toBe(false);
  });

  it("removeTopic removes by id and, if it was the scoped topic, clears the scope", async () => {
    // Scope the grid to a topic, then remove that very topic.
    ui.topics = [{ id: "t1", phrase: "harbor at dusk", space: "blend", createdTs: "x" }];
    await ui.runTopicScope("harbor at dusk");
    expect(ui.gridScope.kind).toBe("topic");
    await ui.removeTopic("t1");
    expect(lastCall("remove_topic")?.args?.id).toBe("t1");
    // The grid fell back to the underlying folder (never sits on a deleted topic).
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "r1", folder: "harbor" });
  });
});

describe("the bake (topic -> collection)", () => {
  it("bakeTopicCollection calls create_collection_from_topic with phrase + threshold", async () => {
    const created = await ui.bakeTopicCollection("harbor at dusk", 0.65, "My harbor");
    const call = lastCall("create_collection_from_topic");
    expect(call?.args?.phrase).toBe("harbor at dusk");
    expect(call?.args?.threshold).toBe(0.65);
    expect(call?.args?.name).toBe("My harbor");
    // Baked against the derived GraphScope (the folder source).
    expect(call?.args?.scope).toEqual({ kind: "folder", root_id: "r1", folder: "harbor" });
    // The new collection is returned (so the caller can surface it).
    expect(created?.id).toBe("c-baked");
    // The collections snapshot refreshed so the rail shows it.
    expect(ipcLog.calls.some((c) => c.cmd === "list_collections")).toBe(true);
  });

  it("a blank name bakes nothing (guards the empty-collection failure mode)", async () => {
    const created = await ui.bakeTopicCollection("harbor at dusk", 0.5, "   ");
    expect(created).toBeNull();
    expect(ipcLog.calls.some((c) => c.cmd === "create_collection_from_topic")).toBe(false);
  });
});
