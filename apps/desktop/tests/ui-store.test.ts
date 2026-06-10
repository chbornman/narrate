/**
 * The Ui store against mocked IPC: selection → reported scope (the UI
 * performs no scope logic — it reports and renders the echo, UI §3.4),
 * rating-key semantics over the echoed scope, the note transient, and the
 * Search overlay's return point (I1).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GridItem, ScopeView } from "../src/lib/types/dto";

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
        return {
          kind,
          count: targets.length,
          previewHashes: targets.slice(0, 3),
        } satisfies ScopeView;
      }
      case "add_note":
      case "set_rating":
        return true;
      case "search":
        return {
          query: { raw: args?.query, filters: args?.filters, dropped: [], fallback: false },
          images: [],
          session_hits: [],
        };
      case "list_folder":
      case "folder_tree":
      case "list_roots":
        return [];
      case "ingest_status":
        return { running: false, done: 0, total: 0, errors: 0 };
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
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
  ui = new Ui();
  ui.rawItems = ["a", "b", "c", "d"].map(item);
});

describe("selection → scope reporting (CAPTURE §3, UI §3.4)", () => {
  it("reports ordered targets and renders the core's echo", async () => {
    let s = sel.click(sel.EMPTY, ui.itemHashes, 2); // c (sorted by filename)
    s = sel.toggle(s, ui.itemHashes, 0); // + a
    await ui.applySelection(s);
    expect(lastCall("set_scope")?.args?.targets).toEqual(["c", "a"]);
    expect(ui.scope).toEqual({ kind: "multi", count: 2, previewHashes: ["c", "a"] });
  });

  it("clearing the selection reports session scope (zero targets)", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.itemHashes, 0));
    await ui.applySelection(sel.clear(ui.sel));
    expect(lastCall("set_scope")?.args?.targets).toEqual([]);
    expect(ui.scope.kind).toBe("session");
  });

  it("entering Look narrows scope to the viewed image; leaving restores", async () => {
    await ui.applySelection(sel.selectAll(sel.EMPTY, ui.itemHashes));
    await ui.openLook("b", false);
    expect(lastCall("set_scope")?.args?.targets).toEqual(["b"]);
    expect(ui.scope.kind).toBe("single");
    await ui.leaveLook();
    expect(lastCall("set_scope")?.args?.targets).toEqual(["a", "b", "c", "d"]);
    expect(ui.scope.count).toBe(4);
  });
});

describe("rating keys over the echoed scope (CAPTURE §10, C6)", () => {
  it("session scope: rating keys do nothing", async () => {
    await ui.rate(3);
    expect(lastCall("set_rating")).toBeUndefined();
  });

  it("with a selection, one keystroke → one command (multi applies to all)", async () => {
    await ui.applySelection(sel.selectAll(sel.EMPTY, ui.itemHashes));
    await ui.rate(3);
    expect(lastCall("set_rating")?.args).toEqual({ value: 3 });
  });

  it("0 is sent as an explicit clear, not suppressed", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.itemHashes, 0));
    await ui.rate(0);
    expect(lastCall("set_rating")?.args).toEqual({ value: 0 });
  });
});

describe("typed-note transient (UI §6)", () => {
  it("summon snapshots the scope; submit sends the text and vanishes", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.itemHashes, 1));
    ui.summonNote();
    expect(ui.note.open).toBe(true);
    expect(ui.note.snapshot?.count).toBe(1);
    await ui.submitNote("the hand is the whole picture");
    expect(ui.note.open).toBe(false);
    expect(lastCall("add_note")?.args).toEqual({
      text: "the hand is the whole picture",
    });
  });

  it("a selection change while the input is open cancels it (scope frozen at summon)", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.itemHashes, 1));
    ui.summonNote();
    await ui.applySelection(sel.selectAll(sel.EMPTY, ui.itemHashes));
    expect(ui.note.open).toBe(false);
    // ...and no note was sent.
    expect(lastCall("add_note")).toBeUndefined();
  });
});

describe("search overlay return point (UI §2.2, I1)", () => {
  it("remembers and restores the invoking surface", async () => {
    await ui.openLook("a", false);
    await ui.openSearch();
    expect(ui.searchOpen).toBe(true);
    expect(ui.searchReturn).toBe("look");
    await ui.escape(); // leave-search
    expect(ui.searchOpen).toBe(false);
    expect(ui.surface).toBe("look"); // back where it was invoked from
  });

  it("escape steps back exactly one layer at a time", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.itemHashes, 0));
    await ui.openSearch();
    ui.summonNote();
    await ui.escape(); // 1: note input closes
    expect(ui.note.open).toBe(false);
    expect(ui.searchOpen).toBe(true);
    await ui.escape(); // 2: search closes
    expect(ui.searchOpen).toBe(false);
    expect(ui.sel.order.length).toBe(1);
    await ui.escape(); // 4: selection clears
    expect(ui.sel.order.length).toBe(0);
    await ui.escape(); // bottom: nothing (never quits)
    expect(ui.surface).toBe("grid");
  });

  it("search query echo flows through the §5.4 contract", async () => {
    await ui.openSearch();
    ui.query = "fog ba";
    ui.chips = [{ type: "rating", op: "gte", value: 3 }];
    await ui.runSearch();
    expect(ui.results?.query.raw).toBe("fog ba");
    expect(ui.results?.images).toEqual([]);
    const call = lastCall("search");
    expect(call?.args?.filters).toEqual([{ type: "rating", op: "gte", value: 3 }]);
  });

  it("removing a chip re-runs the query (UI §5.1)", async () => {
    await ui.openSearch();
    ui.query = "fog";
    ui.chips = [
      { type: "rating", op: "gte", value: 3 },
      { type: "has_strokes", value: true },
    ];
    await ui.runSearch();
    ipcLog.calls.length = 0;
    await ui.removeChip(1);
    expect(ui.chips).toEqual([{ type: "rating", op: "gte", value: 3 }]);
    expect(lastCall("search")).toBeDefined();
  });
});
