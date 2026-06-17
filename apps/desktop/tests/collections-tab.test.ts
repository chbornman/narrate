/**
 * Rail Collections tab — first slice over the P7.3 commands (B71; founder,
 * June 2026: Folders and Collections are PEER tabs; collections are the
 * point, folders are mechanical). Composition-root flows with mocked IPC
 * (the app-flows.test.ts pattern): collection-open drives the grid exactly
 * as folder selection does, rail arrows/Enter follow the visible tab,
 * Add-to-collection sends the whole stack-expanded selection, and the
 * collections-changed snapshot keeps a viewed collection live.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CollectionDto, GridItem, ScopeView } from "../src/lib/types/dto";

const backend = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  collections: [] as CollectionDto[],
  members: {} as Record<string, GridItem[]>,
  /** Collection ids per image hash (collections_for_image). */
  memberships: {} as Record<string, string[]>,
  /** When set, list_collection_members parks on this promise — lets a
   * test hold a members response in flight (the stale-response race). */
  membersGate: null as Promise<void> | null,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    backend.calls.push({ cmd, args });
    switch (cmd) {
      case "set_scope": {
        const targets = (args?.targets ?? []) as string[];
        const kind =
          targets.length === 0 ? "session" : targets.length === 1 ? "single" : "multi";
        return {
          kind,
          count: targets.length,
          previewHashes: targets.slice(0, 3),
        } satisfies ScopeView;
      }
      case "list_collections":
        return backend.collections;
      case "create_collection": {
        const c = coll(`01X${backend.collections.length}`, String(args?.name), "active");
        backend.collections.push(c);
        return c;
      }
      case "add_to_collection": {
        const id = String(args?.id);
        const hashes = (args?.hashes ?? []) as string[];
        const c = backend.collections.find((c) => c.id === id);
        if (c !== undefined) c.memberCount += hashes.length;
        return hashes.length;
      }
      case "remove_from_collection": {
        const id = String(args?.id);
        const hashes = (args?.hashes ?? []) as string[];
        const c = backend.collections.find((c) => c.id === id);
        if (c !== undefined) c.memberCount = Math.max(0, c.memberCount - hashes.length);
        return hashes.length;
      }
      case "collections_for_image": {
        const ids = backend.memberships[String(args?.hash)] ?? [];
        return backend.collections.filter((c) => ids.includes(c.id));
      }
      case "list_collection_members":
        if (backend.membersGate !== null) await backend.membersGate;
        return backend.members[String(args?.id)] ?? [];
      case "list_folder":
      case "folder_tree":
      case "list_roots":
        return [];
      case "ingest_status":
        return { running: false, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [], vectorsVersion: 0 };
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";
import * as sel from "../src/lib/logic/selection";
import { collectionKey } from "../src/lib/logic/sources";

function coll(id: string, name: string, status: string, memberCount = 0): CollectionDto {
  return {
    id,
    name,
    description: "",
    status,
    createdTs: "2026-06-01T00:00:00.000Z",
    updatedTs: "2026-06-01T00:00:00.000Z",
    memberCount,
    noteCount: 0,
  };
}

const item = (hash: string, fileName = `${hash}.jpg`): GridItem => ({
  hash,
  fileName,
  relPath: fileName,
  captureTs: null,
  addedTs: "2026-02-01T00:00:00Z",
  hasJournal: false,
  rating: null,
  offline: false,
});

const lastCall = (cmd: string) =>
  [...backend.calls].reverse().find((c) => c.cmd === cmd);

let ui: Ui;
beforeEach(() => {
  backend.calls.length = 0;
  backend.collections = [coll("01A", "Quiet Hours", "active", 2), coll("01B", "Fog", "shelved")];
  backend.members = { "01A": [item("m1"), item("m2")] };
  backend.memberships = {};
  backend.membersGate = null;
  localStorage.clear();
  ui = new Ui();
  ui.collections = [...backend.collections];
  ui.grid.rootId = "r1";
  ui.grid.rawItems = ["a", "b", "c"].map((h) => item(h));
});

describe("collection-open drives the grid (the folder-open sibling)", () => {
  it("lists the collection's current members and leaves folder mode", async () => {
    await ui.perform({ kind: "collection-open", id: "01A" });
    expect(ui.collectionId).toBe("01A");
    expect(ui.grid.rootId).toBeNull(); // folder machinery stands down
    expect(ui.grid.itemHashes.sort()).toEqual(["m1", "m2"]);
    expect(ui.folderName).toBe("Quiet Hours"); // the title follows
  });

  it("opening a folder afterwards returns to folder mode", async () => {
    await ui.perform({ kind: "collection-open", id: "01A" });
    await ui.openFolder("r1", "");
    expect(ui.collectionId).toBeNull();
    expect(lastCall("list_folder")?.args).toMatchObject({ rootId: "r1" });
  });

  it("a roots snapshot never yanks a collection view to a folder", async () => {
    await ui.perform({ kind: "collection-open", id: "01A" });
    await ui.onRootsChanged([
      {
        rootId: "r1",
        displayName: "shoots",
        relPath: "",
        volumeId: "v1",
        online: true,
        absPath: "/shoots",
        archived: false,
      },
    ]);
    expect(ui.collectionId).toBe("01A");
  });
});

describe("rail keyboard routing follows the visible tab", () => {
  it("arrows walk collection rows; Enter opens the focused collection", async () => {
    ui.shell.railTab = "collections";
    ui.shell.railOpen = true;
    ui.shell.railFocused = true;
    await ui.perform({ kind: "rail-nav", dir: "down" });
    expect(ui.shell.railFocusKey).toBe(collectionKey("01A"));
    await ui.perform({ kind: "rail-nav", dir: "down" });
    expect(ui.shell.railFocusKey).toBe(collectionKey("01B"));
    // Left/right have nothing to expand in a flat list.
    await ui.perform({ kind: "rail-nav", dir: "left" });
    expect(ui.shell.railFocusKey).toBe(collectionKey("01B"));
    await ui.perform({ kind: "rail-nav", dir: "up" });
    await ui.perform({ kind: "rail-enter" });
    expect(ui.collectionId).toBe("01A");
    expect(ui.shell.railFocused).toBe(false); // focus returns to the grid
  });
});

describe("Add to collection (thumb seat submenu verb)", () => {
  it("sends the WHOLE selection and refreshes the snapshot", async () => {
    let s = sel.click(sel.EMPTY, ui.grid.unitHashes, 0);
    s = sel.toggle(s, ui.grid.unitHashes, 2);
    await ui.applySelection(s);
    await ui.perform({ kind: "add-to-collection", id: "01A" });
    expect(lastCall("add_to_collection")?.args).toEqual({
      id: "01A",
      hashes: ["a", "c"],
    });
    // The awaited direct refresh landed the new count.
    expect(ui.collections.find((c) => c.id === "01A")?.memberCount).toBe(4);
  });

  it("with no selection, the active image is the target", async () => {
    await ui.applySelection(sel.EMPTY);
    ui.grid.sel = { order: [], focus: 1, anchor: 1 }; // active without selection
    await ui.perform({ kind: "add-to-collection", id: "01B" });
    expect(lastCall("add_to_collection")?.args).toEqual({ id: "01B", hashes: ["b"] });
  });
});

describe("New collection… (mint + add the selection in one step)", () => {
  it("perform arms the rail creator with the WHOLE stack-expanded selection", async () => {
    // Founder, dogfood June 12 2026: works with ZERO collections (the dead
    // end the ask is about). Wipe the seed list so this is the empty state.
    ui.collections = [];
    let s = sel.click(sel.EMPTY, ui.grid.unitHashes, 0);
    s = sel.toggle(s, ui.grid.unitHashes, 2);
    await ui.applySelection(s);
    await ui.perform({ kind: "new-collection-add" });
    // The targets are stashed (captured synchronously), the rail is opened
    // on the Collections tab — no collection created yet (the rail's input
    // names it). The create+add is the rail's commit, tested below.
    expect(ui.shell.pendingNewCollection).toEqual(["a", "c"]);
    expect(ui.shell.railTab).toBe("collections");
    expect(ui.shell.railOpen).toBe(true);
    expect(lastCall("create_collection")).toBeUndefined();
  });

  it("with no selection the active image is the armed target", async () => {
    await ui.applySelection(sel.EMPTY);
    ui.grid.sel = { order: [], focus: 1, anchor: 1 }; // active without selection
    await ui.perform({ kind: "new-collection-add" });
    expect(ui.shell.pendingNewCollection).toEqual(["b"]);
  });

  it("createCollectionAndAdd creates THEN adds the targets to the new id", async () => {
    // The chain the rail's commit fires: one create, then one add to the
    // freshly minted id (ordered — the add must wait for the id). The mock
    // returns the new DTO from create_collection; we read its id back here
    // rather than hard-code the counter so the assertion stays robust.
    await ui.createCollectionAndAdd(["a", "c"], "Iceland");
    expect(lastCall("create_collection")?.args).toEqual({ name: "Iceland" });
    const newId = ui.collections.find((c) => c.name === "Iceland")?.id;
    expect(newId).toBeDefined();
    expect(lastCall("add_to_collection")?.args).toEqual({
      id: newId,
      hashes: ["a", "c"],
    });
    // The new collection is non-empty (the founder's "don't leave it empty"
    // requirement): the awaited refresh landed the member count.
    expect(ui.collections.find((c) => c.id === newId)?.memberCount).toBe(2);
  });

  it("a blank name creates nothing and adds nothing (no empty collection)", async () => {
    await ui.createCollectionAndAdd(["a"], "   ");
    expect(lastCall("create_collection")).toBeUndefined();
    expect(lastCall("add_to_collection")).toBeUndefined();
  });
});

describe("create + live snapshots", () => {
  it("createCollection trims, creates, and refreshes the list", async () => {
    await ui.createCollection("  Iceland  ");
    expect(lastCall("create_collection")?.args).toEqual({ name: "Iceland" });
    expect(ui.collections.some((c) => c.name === "Iceland")).toBe(true);
  });

  it("an all-whitespace name creates nothing", async () => {
    await ui.createCollection("   ");
    expect(lastCall("create_collection")).toBeUndefined();
  });

  it("collections-changed re-lists a viewed collection's members", async () => {
    await ui.perform({ kind: "collection-open", id: "01A" });
    backend.members["01A"] = [item("m1"), item("m2"), item("m3")];
    await ui.onCollectionsChanged([coll("01A", "Quiet Hours", "active", 3)]);
    expect(ui.grid.itemHashes.sort()).toEqual(["m1", "m2", "m3"]);
    expect(ui.collections).toHaveLength(1);
  });
});

describe("Remove from collection (the add verb's mirror — no one-way door)", () => {
  it("sends the WHOLE selection and refreshes the snapshot", async () => {
    let s = sel.click(sel.EMPTY, ui.grid.unitHashes, 0);
    s = sel.toggle(s, ui.grid.unitHashes, 2);
    await ui.applySelection(s);
    await ui.perform({ kind: "remove-from-collection", id: "01A" });
    expect(lastCall("remove_from_collection")?.args).toEqual({
      id: "01A",
      hashes: ["a", "c"],
    });
    // The awaited direct refresh landed the new count.
    expect(ui.collections.find((c) => c.id === "01A")?.memberCount).toBe(0);
  });

  it("with no selection, the active image is the target", async () => {
    ui.grid.sel = { order: [], focus: 1, anchor: 1 };
    await ui.perform({ kind: "remove-from-collection", id: "01A" });
    expect(lastCall("remove_from_collection")?.args).toEqual({
      id: "01A",
      hashes: ["b"],
    });
  });
});

describe("membership marks follow the active image (thumb menu checkmarks)", () => {
  it("selection changes load the active image's current memberships", async () => {
    backend.memberships = { a: ["01A"], b: [] };
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    expect(ui.activeMemberships).toEqual(["01A"]);
    expect(ui.actionContext().activeMemberships).toEqual(["01A"]);
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 1));
    expect(ui.activeMemberships).toEqual([]);
  });

  it("add/remove refresh the marks for the unchanged active image", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    backend.memberships = { a: ["01B"] };
    await ui.perform({ kind: "add-to-collection", id: "01B" });
    expect(ui.activeMemberships).toEqual(["01B"]);
    backend.memberships = { a: [] };
    await ui.perform({ kind: "remove-from-collection", id: "01B" });
    expect(ui.activeMemberships).toEqual([]);
  });
});

describe("stale async grid loads never clobber the current view", () => {
  it("a held-up members response loses to a folder opened afterwards", async () => {
    // Hold the collection's members fetch in flight…
    let release!: () => void;
    backend.membersGate = new Promise<void>((r) => (release = r));
    const opening = ui.perform({ kind: "collection-open", id: "01A" });
    backend.membersGate = null;
    // …click a folder while it is still pending…
    await ui.openFolder("r1", "");
    expect(ui.collectionId).toBeNull();
    const folderItems = ui.grid.itemHashes;
    // …then let the stale members response land: the grid must still
    // show the folder, not collection members under a folder header.
    release();
    await opening;
    expect(ui.collectionId).toBeNull();
    expect(ui.grid.itemHashes).toEqual(folderItems);
  });
});
