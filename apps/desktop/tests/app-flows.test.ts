/**
 * INTEGRATION cross-slice flows over the composition root with mocked IPC
 * (ui-store.test.ts pattern): the Look navigation set = entry selection
 * (featureset §2 via looknav.navigationSet), flip-aware Look→Grid focus
 * restore, the collapsed-pair "● 2" truth END TO END through openLook and
 * R (coordinator ruling), the inspector following the active image
 * (featureset §3), the drag-folder → register-root confirmation flow
 * (featureset §6, escape layer 2), the cross-window roots-changed handler,
 * and the rail's add-root picker flow (founder dogfood, rounds 1+2).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { GridItem, RootDto, ScopeView } from "../src/lib/types/dto";

const ipcLog = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  failAddRoot: false,
}));

/** The OS folder picker (rail "Add folder…"): null = user cancelled. */
const dialog = vi.hoisted(() => ({ nextDir: null as string | null }));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => dialog.nextDir),
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
          previewHashes: targets.slice(0, 8),
        } satisfies ScopeView;
      }
      case "add_root":
        if (ipcLog.failAddRoot) throw new Error("not a folder");
        return {
          rootId: `root:${args?.path as string}`,
          displayName: String(args?.path),
          relPath: "",
          volumeId: "v1",
          online: true,
          absPath: String(args?.path),
        };
      case "image_journal":
        return [];
      case "image_metadata":
        return null;
      case "list_folder":
      case "folder_tree":
      case "list_roots":
        return [];
      case "ingest_status":
        return { running: false, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0 };
      case "search":
        // Fused-order result hashes; the test seeds them via failSearch-free
        // default of three results r1..r3 (overridable per-test isn't needed).
        return {
          query: { raw: args?.query, filters: args?.filters ?? [], dropped: [], fallback: false },
          images: ["r1", "r2", "r3"].map((h) => ({
            image_hash: h,
            preview: h,
            score: 1,
            provenance: { type: "filter_only" },
            last_annotated_ts: null,
            debug: null,
          })),
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
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";
import * as sel from "../src/lib/logic/selection";

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
  [...ipcLog.calls].reverse().find((c) => c.cmd === cmd);

let ui: Ui;
beforeEach(() => {
  ipcLog.calls.length = 0;
  ipcLog.failAddRoot = false;
  dialog.nextDir = null;
  localStorage.clear();
  ui = new Ui();
  // a..d solo JPEGs in filename order (capture-desc falls back to name).
  ui.grid.rawItems = ["a", "b", "c", "d"].map((h) => item(h));
});

describe("navigation set = entry selection (featureset §2)", () => {
  it("a ≥2 selection including the entry cycles within it, in GRID order", async () => {
    // Select d then b (selection order d,b) — navigation order stays b,d.
    let s = sel.click(sel.EMPTY, ui.grid.unitHashes, 3);
    s = sel.toggle(s, ui.grid.unitHashes, 1);
    await ui.applySelection(s);
    await ui.openLook("d");
    expect(ui.look.order.map((e) => e.display)).toEqual(["b", "d"]);
    expect(ui.look.currentHash).toBe("d");
    // ←/→ stay inside the entry selection.
    await ui.lookNav(-1);
    expect(ui.look.currentHash).toBe("b");
    expect(ui.look.next(-1)).toBe(false); // edge of the set, not the folder
  });

  it("single-image entry cycles the whole folder", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 2));
    await ui.openLook("c");
    expect(ui.look.order.map((e) => e.display)).toEqual(["a", "b", "c", "d"]);
  });

  it("an entry OUTSIDE the selection cycles the folder (scope narrows anyway)", async () => {
    let s = sel.click(sel.EMPTY, ui.grid.unitHashes, 0);
    s = sel.toggle(s, ui.grid.unitHashes, 1);
    await ui.applySelection(s);
    await ui.openLook("d");
    expect(ui.look.order).toHaveLength(4);
  });

  it("the same rule governs query-result entry (M3: results are grid cells)", async () => {
    // A committed query re-scopes the grid to r1,r2,r3 (fused order). Results
    // are ordinary cells now, so the SAME navigation-set rule applies: a ≥2
    // selection including the entry cycles within it, in grid order.
    ui.query = "fog"; // 2+ chars: above MIN_QUERY_CHARS, a real query
    await ui.runQueryScope("semantic");
    expect(ui.grid.unitHashes).toEqual(["r1", "r2", "r3"]);
    // Select r3 then r1 (selection order r3,r1) — grid order is r1,r3.
    let s = sel.click(sel.EMPTY, ui.grid.unitHashes, 2); // r3
    s = sel.toggle(s, ui.grid.unitHashes, 0); // + r1
    await ui.applySelection(s);
    await ui.openLook("r1");
    expect(ui.look.order.map((e) => e.display)).toEqual(["r1", "r3"]); // grid order
    expect(ui.surface).toBe("look");
  });
});

describe("collapsed RAW+JPEG pair — the ● 2 truth end to end (D1)", () => {
  beforeEach(() => {
    // One collapsed pair (IMG_1.jpg + IMG_1.cr3) between solos a and z.
    ui.grid.rawItems = [
      item("a", "a.jpg"),
      item("jpegHash", "IMG_1.jpg"),
      item("rawHash", "IMG_1.cr3"),
      item("z", "z.jpg"),
    ];
    ui.grid.stackGlobalCollapsed = true;
  });

  it("selecting the one cell reports two ordered targets (JPEG first)", async () => {
    const idx = ui.grid.unitHashes.indexOf("jpegHash");
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, idx));
    expect(lastCall("set_scope")?.args?.targets).toEqual(["jpegHash", "rawHash"]);
    expect(ui.shell.scope).toMatchObject({ kind: "multi", count: 2 });
  });

  it("viewing the pair in Look keeps both targets; R re-orders display-first", async () => {
    await ui.openLook("jpegHash");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["jpegHash", "rawHash"]);
    await ui.perform({ kind: "flip-stack-member" });
    expect(ui.look.currentHash).toBe("rawHash");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["rawHash", "jpegHash"]);
  });

  it("leaving Look after R lands focus on the pair's cell (flip-aware)", async () => {
    await ui.openLook("jpegHash");
    await ui.perform({ kind: "flip-stack-member" }); // viewing the RAW now
    await ui.leaveLook();
    expect(ui.surface).toBe("grid");
    expect(ui.grid.unitHashes[ui.grid.sel.focus]).toBe("jpegHash"); // the cell
  });
});

describe("the inspector follows the active image (featureset §3)", () => {
  it("tracks focus moves in Grid and ←/→ in Look while open", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    await ui.perform({ kind: "open-inspector", tab: "journal" });
    expect(ui.inspector.hash).toBe("a");
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 1));
    expect(ui.inspector.hash).toBe("b");
    await ui.openLook("b");
    await ui.lookNav(1);
    expect(ui.inspector.hash).toBe("c");
  });

  it("does not load while closed", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    expect(lastCall("image_journal")).toBeUndefined();
  });
});

describe("drag-folder → register-root confirmation (featureset §6)", () => {
  it("drop offers, Esc dismisses FIRST (escape layer 2), nothing registers", async () => {
    ui.offerDrop(["/shoots/iceland"]);
    ui.shell.openContextMenu("gutter", null); // deeper layer stays open
    await ui.escape();
    expect(ui.dropPaths).toBeNull();
    expect(ui.shell.contextMenu).not.toBeNull(); // exactly one layer peeled
    expect(lastCall("add_root")).toBeUndefined();
  });

  it("confirm registers every path and opens the first root", async () => {
    ui.offerDrop(["/shoots/iceland", "/shoots/harbor"]);
    await ui.confirmDrop();
    const added = ipcLog.calls.filter((c) => c.cmd === "add_root");
    expect(added.map((c) => c.args?.path)).toEqual([
      "/shoots/iceland",
      "/shoots/harbor",
    ]);
    expect(ui.dropPaths).toBeNull();
    expect(ui.grid.rootId).toBe("root:/shoots/iceland");
  });

  it("a refused path reports one inline line and keeps the sheet open", async () => {
    ipcLog.failAddRoot = true;
    ui.offerDrop(["/not/a/folder.jpg"]);
    await ui.confirmDrop();
    expect(ui.dropPaths).not.toBeNull();
    expect(ui.dropError).toContain("not a folder");
  });

  it("an empty drop never opens the sheet", () => {
    ui.offerDrop([]);
    expect(ui.dropPaths).toBeNull();
  });
});

describe("roots-changed live propagation (founder dogfood, round 2)", () => {
  // add_root/remove_root emit `roots-changed` with the fresh active-roots
  // snapshot (the settings-changed pattern); App.svelte routes the payload
  // here. The Settings window's edits land in the rail INSTANTLY.
  const root = (id: string): RootDto => ({
    rootId: id,
    displayName: id,
    relPath: "",
    volumeId: "v1",
    online: true,
    absPath: `/${id}`,
  });

  it("a first root added in Settings appears AND opens (nothing was open)", async () => {
    expect(ui.grid.rootId).toBeNull();
    await ui.onRootsChanged([root("r1")]);
    expect(ui.roots.map((r) => r.rootId)).toEqual(["r1"]);
    expect(ui.grid.rootId).toBe("r1"); // the init() rule: first root opens
    expect(lastCall("list_folder")?.args).toMatchObject({ rootId: "r1", folder: "" });
  });

  it("an unrelated add updates the rail without navigating away", async () => {
    ui.grid.rootId = "r1";
    ui.grid.folder = "2026";
    await ui.onRootsChanged([root("r1"), root("r2")]);
    expect(ui.roots).toHaveLength(2);
    expect(ui.grid.rootId).toBe("r1");
    expect(ui.grid.folder).toBe("2026");
    expect(lastCall("list_folder")).toBeUndefined(); // no reload, no jump
  });

  it("removing the open root resets the grid and falls back to the first remaining", async () => {
    ui.grid.rootId = "r2";
    await ui.onRootsChanged([root("r1")]);
    expect(ui.grid.rootId).toBe("r1");
  });

  it("removing the last root returns to first-run (never a dead grid)", async () => {
    ui.grid.rootId = "r1";
    await ui.onRootsChanged([]);
    expect(ui.roots).toEqual([]);
    expect(ui.grid.rootId).toBeNull();
    expect(ui.grid.rawItems).toEqual([]);
  });
});

describe("Tab lights-out hides the NATIVE chrome too (featureset §0, macOS)", () => {
  // The traffic lights are NSButtons (Overlay titlebar) — outside the DOM
  // region gates. The perform sink must hide/show them in lockstep with
  // chromeHidden, and only on macOS (Windows/Linux chrome is all-DOM).
  const stubNavigator = (platform: string, userAgent: string) => {
    Object.defineProperty(window.navigator, "platform", {
      value: platform,
      configurable: true,
    });
    Object.defineProperty(window.navigator, "userAgent", {
      value: userAgent,
      configurable: true,
    });
  };
  afterEach(() => {
    delete (window.navigator as { platform?: string }).platform;
    delete (window.navigator as { userAgent?: string }).userAgent;
  });

  it("macOS: toggling sends set_traffic_lights_hidden true, then false", async () => {
    stubNavigator(
      "MacIntel",
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
    );
    await ui.perform({ kind: "toggle-lights-out" });
    expect(ui.shell.chromeHidden).toBe(true);
    expect(lastCall("set_traffic_lights_hidden")?.args).toEqual({ hidden: true });
    await ui.perform({ kind: "toggle-lights-out" });
    expect(ui.shell.chromeHidden).toBe(false);
    expect(lastCall("set_traffic_lights_hidden")?.args).toEqual({ hidden: false });
  });

  it("Linux: no native call — the custom controls are DOM-gated already", async () => {
    stubNavigator(
      "Linux x86_64",
      "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
    );
    await ui.perform({ kind: "toggle-lights-out" });
    expect(ui.shell.chromeHidden).toBe(true);
    expect(lastCall("set_traffic_lights_hidden")).toBeUndefined();
  });
});

describe("add-root from the rail (founder dogfood, rounds 1+2)", () => {
  it("the picked directory registers through add_root and opens", async () => {
    dialog.nextDir = "/shoots/keflavik";
    await ui.perform({ kind: "add-root" });
    expect(lastCall("add_root")?.args).toEqual({ path: "/shoots/keflavik" });
    expect(ui.grid.rootId).toBe("root:/shoots/keflavik");
  });

  it("cancelling the picker registers nothing", async () => {
    dialog.nextDir = null;
    await ui.perform({ kind: "add-root" });
    expect(lastCall("add_root")).toBeUndefined();
    expect(ui.shell.ingestExpecting).toBe(false); // no scan, no bridge
  });
});

describe("ingest empty-state honesty (founder incident, June 2026)", () => {
  // Between the add/rescan click and the pump's first scanning=true emit,
  // ingest.running still reads false — the optimistic `ingestExpecting`
  // bridge is what keeps the empty grid from lying "No photographs" over
  // a folder about to be walked.
  it("add-root raises the bridge; the first real status event clears it", async () => {
    dialog.nextDir = "/shoots/longwalk";
    expect(ui.shell.ingestExpecting).toBe(false);
    await ui.perform({ kind: "add-root" });
    expect(ui.shell.ingestExpecting).toBe(true); // no event yet: bridge holds
    await ui.onIngestProgress({
      running: true,
      done: 0,
      total: 0,
      errors: 0,
      passes: [],
      scanning: true,
      discovered: 42,
    });
    // The walk-aware status owns the copy now — and carries the count.
    expect(ui.shell.ingestExpecting).toBe(false);
    expect(ui.shell.ingest.scanning).toBe(true);
    expect(ui.shell.ingest.discovered).toBe(42);
  });

  it("rescan raises the bridge; even an instantly-idle event clears it", async () => {
    await ui.perform({ kind: "rescan-root", rootId: "R1" });
    expect(ui.shell.ingestExpecting).toBe(true);
    // An empty root's scan can finish before any running emit lands: the
    // idle event must still stand the bridge down or "Indexing" strands.
    await ui.onIngestProgress({
      running: false,
      done: 0,
      total: 0,
      errors: 0,
      passes: [],
      scanning: false,
      discovered: 0,
    });
    expect(ui.shell.ingestExpecting).toBe(false);
  });

  it("a refused add-root stands the bridge down — no scan will ever run", async () => {
    dialog.nextDir = "/not/a/folder";
    ipcLog.failAddRoot = true;
    await expect(ui.perform({ kind: "add-root" })).rejects.toThrow("not a folder");
    expect(ui.shell.ingestExpecting).toBe(false);
  });

  it("a fully refused drop stands the bridge down too", async () => {
    ipcLog.failAddRoot = true;
    ui.offerDrop(["/not/a/folder.jpg"]);
    await ui.confirmDrop();
    expect(ui.shell.ingestExpecting).toBe(false);
  });
});

describe("mid-scan grid re-list (founder, SMB, June 2026)", () => {
  it("running ingest re-lists on a 2 s throttle; the idle edge re-lists once more", async () => {
    const ui2 = new Ui();
    ui2.grid.rootId = "R1";
    ui2.grid.folder = "";
    const calls = () => ipcLog.calls.filter((c) => c.cmd === "list_folder").length;
    const before = calls();
    const nowSpy = vi.spyOn(Date, "now");

    nowSpy.mockReturnValue(100_000);
    await ui2.onIngestProgress({ running: true, done: 1, total: 10, errors: 0, passes: [], scanning: false, discovered: 0 });
    expect(calls()).toBe(before + 1); // first running tick lists

    nowSpy.mockReturnValue(100_500);
    await ui2.onIngestProgress({ running: true, done: 2, total: 10, errors: 0, passes: [], scanning: false, discovered: 0 });
    expect(calls()).toBe(before + 1); // inside the 2 s throttle: no re-list

    nowSpy.mockReturnValue(103_000);
    await ui2.onIngestProgress({ running: true, done: 5, total: 10, errors: 0, passes: [], scanning: false, discovered: 0 });
    expect(calls()).toBe(before + 2); // throttle elapsed

    await ui2.onIngestProgress({ running: false, done: 10, total: 10, errors: 0, passes: [], scanning: false, discovered: 0 });
    expect(calls()).toBe(before + 3); // running→idle edge: exact final state
    expect(ui2.shell.ingest.running).toBe(false);

    await ui2.onIngestProgress({ running: false, done: 10, total: 10, errors: 0, passes: [], scanning: false, discovered: 0 });
    expect(calls()).toBe(before + 3); // already idle: indicator only
    nowSpy.mockRestore();
  });
});
