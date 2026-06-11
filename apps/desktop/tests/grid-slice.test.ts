/**
 * Grid slice (state/grid.svelte.ts) within the frozen contract: stack
 * pairing drives `units`, `selectionTargets` is stack-EXPANDED (CAPTURE
 * §3 via logic/stacks.ts), collapse/expand is live and reversible with
 * the selection following the surviving cells, the R flip re-orders
 * targets display-first, and the T cycle + prefs persistence. The closing
 * block runs the full Ui loop against mocked IPC to pin the coordinator
 * ruling: ONE selected collapsed cell echoes "● 2".
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GridItem, ScopeView } from "../src/lib/types/dto";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
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

import { GridSlice } from "../src/lib/state/grid.svelte";
import { Ui } from "../src/lib/state/app.svelte";
import * as sel from "../src/lib/logic/selection";
import * as prefs from "../src/lib/state/prefs";

const item = (relPath: string): GridItem => {
  const fileName = relPath.split("/").pop() as string;
  return {
    hash: `h:${relPath}`,
    fileName,
    relPath,
    captureTs: null,
    addedTs: "2026-06-01T00:00:00Z",
    hasJournal: false,
    rating: null,
    offline: false,
  };
};

// Default sort (capture-desc, no capture ts) falls back to filename order:
// IMG_1.cr2 < IMG_1.jpg < IMG_2.jpg.
const PAIR_FOLDER = [item("a/IMG_1.cr2"), item("a/IMG_1.jpg"), item("a/IMG_2.jpg")];

let grid: GridSlice;
beforeEach(() => {
  localStorage.clear();
  grid = new GridSlice();
  grid.setItems(PAIR_FOLDER);
});

describe("units = stack pairing over the sorted items (D1)", () => {
  it("collapses the pair into one JPEG-on-top cell by default", () => {
    expect(grid.units.length).toBe(2);
    expect(grid.unitHashes).toEqual(["h:a/IMG_1.jpg", "h:a/IMG_2.jpg"]);
    expect(grid.units[0].alt?.hash).toBe("h:a/IMG_1.cr2");
  });

  it("active pair flags derive from the active cell", () => {
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, 0));
    expect(grid.activeIsPair).toBe(true);
    expect(grid.activePairCollapsed).toBe(true);
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, 1));
    expect(grid.activeIsPair).toBe(false);
  });
});

describe("selectionTargets is stack-expanded (the ● 2 truth upstream)", () => {
  it("one selected collapsed CELL = two ordered targets, JPEG then RAW", () => {
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, 0));
    expect(grid.sel.order.length).toBe(1); // one cell selected…
    expect(grid.selectionTargets).toEqual(["h:a/IMG_1.jpg", "h:a/IMG_1.cr2"]);
  });

  it("select-all over [pair, solo] reports three targets in selection order", () => {
    grid.setSelection(sel.selectAll(sel.EMPTY, grid.unitHashes));
    expect(grid.selectionTargets).toEqual([
      "h:a/IMG_1.jpg",
      "h:a/IMG_1.cr2",
      "h:a/IMG_2.jpg",
    ]);
  });
});

describe("live, reversible collapse (featureset §5)", () => {
  it("toggleActiveStack expands and re-collapses, selection following the cells", () => {
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, 0));
    grid.toggleActiveStack();
    expect(grid.units.length).toBe(3);
    // The active image stays active; targets narrow to it alone.
    expect(grid.activeHash).toBe("h:a/IMG_1.jpg");
    expect(grid.selectionTargets).toEqual(["h:a/IMG_1.jpg"]);
    expect(grid.activeIsPair).toBe(true); // membership survives expansion
    expect(grid.activePairCollapsed).toBe(false);
    grid.toggleActiveStack();
    expect(grid.units.length).toBe(2);
    expect(grid.selectionTargets).toEqual(["h:a/IMG_1.jpg", "h:a/IMG_1.cr2"]);
  });

  it("a selected hidden member folds into the surviving cell on collapse", () => {
    grid.setAllStacks(false);
    const rawIdx = grid.unitHashes.indexOf("h:a/IMG_1.cr2");
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, rawIdx));
    grid.toggleActiveStack(); // collapse the pair from its RAW member
    expect(grid.units.length).toBe(2);
    expect(grid.activeHash).toBe("h:a/IMG_1.jpg"); // the display member
    expect(grid.selectionTargets).toEqual(["h:a/IMG_1.jpg", "h:a/IMG_1.cr2"]);
  });

  it("setAllStacks clears the per-pair overrides and persists the global flag", () => {
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, 0));
    grid.toggleActiveStack();
    expect(grid.pairOverrides.size).toBe(1);
    grid.setAllStacks(false);
    expect(grid.pairOverrides.size).toBe(0);
    expect(grid.units.length).toBe(3);
    expect(prefs.loadStackGlobal()).toBe(false);
    grid.setAllStacks(true);
    expect(grid.units.length).toBe(2);
    expect(prefs.loadStackGlobal()).toBe(true);
  });
});

describe("R flip in Grid (featureset §5: acts on the active image)", () => {
  it("swaps the displayed member and the target order (display first)", () => {
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, 0));
    grid.flipActiveMember();
    expect(grid.activeHash).toBe("h:a/IMG_1.cr2"); // RAW now on top
    expect(grid.selectionTargets).toEqual(["h:a/IMG_1.cr2", "h:a/IMG_1.jpg"]);
    grid.flipActiveMember(); // reversible
    expect(grid.activeHash).toBe("h:a/IMG_1.jpg");
    expect(grid.selectionTargets).toEqual(["h:a/IMG_1.jpg", "h:a/IMG_1.cr2"]);
  });

  it("is a no-op on solo cells and expanded members", () => {
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, 1)); // solo
    grid.flipActiveMember();
    expect(grid.activeHash).toBe("h:a/IMG_2.jpg");
    grid.setAllStacks(false);
    grid.setSelection(sel.click(sel.EMPTY, grid.unitHashes, 0)); // expanded member
    grid.flipActiveMember();
    expect(grid.units.length).toBe(3);
  });
});

describe("T cell-info cycle persists (featureset §1)", () => {
  it("cycles none → minimal → annotated → none and saves each step", () => {
    expect(grid.cellInfo).toBe("none");
    grid.cycleCellInfo();
    expect(grid.cellInfo).toBe("minimal");
    expect(prefs.loadCellInfo()).toBe("minimal");
    grid.cycleCellInfo();
    grid.cycleCellInfo();
    expect(grid.cellInfo).toBe("none");
  });
});

describe("the indicator reads ● 2 for one selected collapsed cell (Ui loop)", () => {
  it("reports the EXPANDED targets; the echoed scope is multi/2 off one cell", async () => {
    const ui = new Ui();
    ui.grid.setItems(PAIR_FOLDER);
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    expect(ui.grid.sel.order.length).toBe(1); // 1 cell…
    expect(ui.shell.scope).toEqual({
      kind: "multi",
      count: 2, // …● 2 — the truth, not the cell count
      previewHashes: ["h:a/IMG_1.jpg", "h:a/IMG_1.cr2"],
    });
  });

  it("expanding the pair narrows the echoed scope back to ● 1", async () => {
    const ui = new Ui();
    ui.grid.setItems(PAIR_FOLDER);
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    await ui.perform({ kind: "stack-toggle-active" });
    expect(ui.shell.scope.kind).toBe("single");
    expect(ui.shell.scope.count).toBe(1);
  });
});

describe("previews-changed ping (thumbs heal as artifacts land)", () => {
  it("each event bumps seq and replaces the hash set", () => {
    const g = new GridSlice();
    expect(g.previewPing.seq).toBe(0);
    g.onPreviewsChanged(["aa", "bb"]);
    expect(g.previewPing.seq).toBe(1);
    expect(g.previewPing.hashes.has("aa")).toBe(true);
    g.onPreviewsChanged(["cc"]);
    expect(g.previewPing.seq).toBe(2);
    expect(g.previewPing.hashes.has("aa")).toBe(false);
    expect(g.previewPing.hashes.has("cc")).toBe(true);
  });

  it("an empty payload still bumps seq (harmless, observable)", () => {
    const g = new GridSlice();
    g.onPreviewsChanged([]);
    expect(g.previewPing.seq).toBe(1);
    expect(g.previewPing.hashes.size).toBe(0);
  });
});

describe("mid-ingest re-sort keeps IDENTITY, not indexes (founder dogfood, June 2026)", () => {
  // The exif pass fills captureTs mid-session; capture-desc then flips
  // from the undated filename fallback to dated order under the user.
  const dated = (relPath: string, ts: string): GridItem => ({
    ...item(relPath),
    captureTs: ts,
  });

  it("focus follows the image across a sort flip, not its index", () => {
    const g = new GridSlice();
    g.setItems([item("a.jpg"), item("b.jpg"), item("c.jpg")]); // filename order
    g.sel = { order: ["h:a.jpg"], focus: 0, anchor: 0 };
    expect(g.activeHash).toBe("h:a.jpg");
    // captureTs lands: a oldest → capture-desc puts it LAST.
    g.setItems([
      dated("a.jpg", "2026-06-01T00:00:00Z"),
      dated("b.jpg", "2026-06-02T00:00:00Z"),
      dated("c.jpg", "2026-06-03T00:00:00Z"),
    ]);
    expect(g.activeHash).toBe("h:a.jpg"); // the image, never index 0
    expect(g.sel.focus).toBe(2);
    expect(g.sel.order).toEqual(["h:a.jpg"]);
  });

  it("togglePairByHash toggles the pair HOSTING the hash at execute time", () => {
    const g = new GridSlice();
    g.setItems([item("a.jpg"), item("a.cr2"), item("b.jpg"), item("b.cr2")]);
    expect(g.units.length).toBe(2); // two collapsed pairs
    // Re-sort: b's capture time is newer → capture-desc puts b first.
    g.setItems([
      dated("a.jpg", "2026-06-01T00:00:00Z"),
      dated("a.cr2", "2026-06-01T00:00:00Z"),
      dated("b.jpg", "2026-06-05T00:00:00Z"),
      dated("b.cr2", "2026-06-05T00:00:00Z"),
    ]);
    expect(g.unitHashes[0]).toBe("h:b.jpg");
    g.togglePairByHash("h:a.jpg"); // a moved to index 1 — still a's pair
    expect(g.unitStack(g.unitHashes.indexOf("h:a.jpg"))).toBe("expanded");
    expect(g.unitStack(g.unitHashes.indexOf("h:b.jpg"))).toBe("collapsed");
  });

  it("a selected display member that pairs up mid-ingest follows its cell", () => {
    const g = new GridSlice();
    g.setItems([item("a.jpg")]);
    g.sel = { order: ["h:a.jpg"], focus: 0, anchor: 0 };
    // The RAW member lands later in the scan: the jpg stays the display
    // member of the now-collapsed pair; selection sticks to the cell.
    g.setItems([item("a.jpg"), item("a.cr2")]);
    expect(g.units.length).toBe(1);
    expect(g.activeHash).toBe("h:a.jpg");
    expect(g.selectionTargets).toEqual(["h:a.jpg", "h:a.cr2"]);
  });
});
