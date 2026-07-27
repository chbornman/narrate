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
import type { FolderDelta, GridItem, RootDto, ScopeView } from "../src/lib/types/dto";

const ipcLog = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  failAddRoot: false,
  folderDelta: null as FolderDelta | null,
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
        // add_root now returns an AddRootOutcome (refuse + alias): the happy
        // path is `added` carrying the root.
        return {
          kind: "added",
          root: {
            rootId: `root:${args?.path as string}`,
            displayName: String(args?.path),
            relPath: "",
            volumeId: "v1",
            online: true,
            absPath: String(args?.path),
            archived: false,
          },
        };
      case "image_journal":
        return [];
      case "image_metadata":
        return null;
      case "list_folder":
      case "folder_tree":
      case "list_roots":
      case "list_archived_roots":
        return [];
      case "list_folder_delta":
        return (
          ipcLog.folderDelta ?? {
            fromRevision: Number(args?.sinceRevision ?? 0),
            toRevision: Number(args?.sinceRevision ?? 0) + 1,
            reset: false,
            upserts: [],
            removedHashes: [],
          }
        );
      case "ingest_status":
        return { running: false, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [], vectorsVersion: 0, imagesVersion: 0, journalVersion: 0 };
      case "search":
        // Fused-order result hashes; the test seeds them via failSearch-free
        // default of three results r1..r3 (overridable per-test isn't needed).
        // When the ⚙ popover asks for it (includeDebug), echo a DebugScores so
        // the Phase 3 per-cell provenance path has data to surface.
        return {
          query: { raw: args?.query, filters: args?.filters ?? [], dropped: [], fallback: false },
          images: ["r1", "r2", "r3"].map((h) => ({
            image_hash: h,
            preview: h,
            score: 1,
            provenance: { type: "filter_only" },
            last_annotated_ts: null,
            debug:
              args?.includeDebug === true
                ? { per_signal: [["S1AnnotationChunk", 1, 0.9]], fused: 1 }
                : null,
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

import {
  Ui,
  INGEST_EXPECT_TIMEOUT_MS,
  INGEST_RELIST_DEBOUNCE_MS,
} from "../src/lib/state/app.svelte";
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
  ipcLog.folderDelta = null;
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
    expect(ui.viewMode).toBe("look");
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
    expect(ui.viewMode).toBe("grid");
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
    archived: false,
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

  it("a one-shot preview-only choice rides the command without changing defaults", async () => {
    dialog.nextDir = "/shoots/contact-sheet";
    await ui.addRootFromPicker("preview-only");
    expect(lastCall("add_root")?.args).toEqual({
      path: "/shoots/contact-sheet",
      policy: "preview-only",
    });
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
      discovered: 42, offlineVolumes: [], vectorsVersion: 0, imagesVersion: 0, journalVersion: 0
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
      discovered: 0, offlineVolumes: [], vectorsVersion: 0, imagesVersion: 0, journalVersion: 0
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

  // The §6e strand (AUDIT-FRONTEND-COUPLING A2): a rescan/add that returns Ok
  // but emits NO ingest-progress (deleted path, zero-change rescan) used to
  // leave "Indexing…" stranded forever. The watchdog stands it down.
  describe("watchdog: a silent ingest no-op cannot strand the bridge", () => {
    beforeEach(() => vi.useFakeTimers());
    afterEach(() => vi.useRealTimers());

    it("auto-clears after the deadline when no event ever arrives", async () => {
      // A rescan whose backend returns Ok but emits nothing: bridge raised,
      // no onIngestProgress will ever land.
      await ui.perform({ kind: "rescan-root", rootId: "R1" });
      expect(ui.shell.ingestExpecting).toBe(true);
      // Just before the deadline it still holds (don't blink early).
      vi.advanceTimersByTime(INGEST_EXPECT_TIMEOUT_MS - 1);
      expect(ui.shell.ingestExpecting).toBe(true);
      // At the deadline the watchdog stands the lie down — the strand is fixed.
      vi.advanceTimersByTime(1);
      expect(ui.shell.ingestExpecting).toBe(false);
    });

    it("a real ingest-progress event clears the flag AND cancels the watchdog", async () => {
      await ui.perform({ kind: "rescan-root", rootId: "R1" });
      expect(ui.shell.ingestExpecting).toBe(true);
      // The healthy path: a real status lands first and takes over the copy.
      await ui.onIngestProgress({
        running: true,
        done: 0,
        total: 0,
        errors: 0,
        passes: [],
        scanning: true,
        discovered: 7, offlineVolumes: [], vectorsVersion: 0, imagesVersion: 0, journalVersion: 0
      });
      expect(ui.shell.ingestExpecting).toBe(false);
      // The watchdog must be CANCELLED: advancing past the deadline must not
      // fire a late spurious clear (it would no-op here, but a leaked timer
      // could clear a NEW expect raised in between — so prove it's gone).
      vi.advanceTimersByTime(INGEST_EXPECT_TIMEOUT_MS * 2);
      expect(ui.shell.ingestExpecting).toBe(false);
    });
  });
});

describe("mid-scan incremental grid catch-up — Seam 1 imagesVersion handshake", () => {
  // The grid moved off the old 2 s wall-clock throttle + App.svelte's redundant
  // setInterval onto the `imagesVersion` data-version contract: re-list when,
  // and only when, the image-set version ADVANCES (a NEW image committed),
  // debounced so a burst coalesces; the running→idle edge re-lists once more,
  // immediately, for an exact settled state. previewReady flips reach the grid
  // on their own `previews-changed` channel, NOT through this path.
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  /** One ingest status; only the fields this contract reads matter. */
  const status = (over: {
    running: boolean;
    imagesVersion: number;
  }) => ({
    running: over.running,
    done: 0,
    total: 10,
    errors: 0,
    passes: [],
    scanning: false,
    discovered: 0,
    offlineVolumes: [],
    vectorsVersion: 0,
    imagesVersion: over.imagesVersion,
    journalVersion: 0,
  });

  it("requests a folder delta when imagesVersion advances and does no work on an unrelated event", async () => {
    const ui2 = new Ui();
    ui2.grid.rootId = "R1";
    ui2.grid.folder = "";
    const calls = () => ipcLog.calls.filter((c) => c.cmd === "list_folder_delta").length;
    const before = calls();

    // First event seeds the baseline version — no re-list on a first sighting.
    await ui2.onIngestProgress(status({ running: true, imagesVersion: 0 }));
    expect(calls()).toBe(before);

    // A NEW image lands: version advances 0→1. The re-list is debounced, so it
    // has NOT fired synchronously; it fires once the debounce window elapses.
    await ui2.onIngestProgress(status({ running: true, imagesVersion: 1 }));
    expect(calls()).toBe(before); // still pending inside the debounce window
    await vi.advanceTimersByTimeAsync(INGEST_RELIST_DEBOUNCE_MS);
    expect(calls()).toBe(before + 1); // one coalesced re-list

    // An UNRELATED event (progress ticks, same imagesVersion) does NO grid work:
    // the old throttle re-listed on every running tick; the version handshake
    // re-lists only on a real image change.
    await ui2.onIngestProgress(status({ running: true, imagesVersion: 1 }));
    await vi.advanceTimersByTimeAsync(INGEST_RELIST_DEBOUNCE_MS);
    expect(calls()).toBe(before + 1); // unchanged: no false refresh
  });

  it("coalesces a burst of advances into one delta request", async () => {
    const ui2 = new Ui();
    ui2.grid.rootId = "R1";
    ui2.grid.folder = "";
    const calls = () => ipcLog.calls.filter((c) => c.cmd === "list_folder_delta").length;
    const before = calls();

    await ui2.onIngestProgress(status({ running: true, imagesVersion: 0 })); // seed
    // A fast scan bumps the version repeatedly inside one debounce window.
    for (let v = 1; v <= 5; v++)
      await ui2.onIngestProgress(status({ running: true, imagesVersion: v }));
    expect(calls()).toBe(before); // all coalesced, still pending
    await vi.advanceTimersByTimeAsync(INGEST_RELIST_DEBOUNCE_MS);
    expect(calls()).toBe(before + 1); // the burst collapsed to a single re-list
  });

  it("applies delta upserts without replacing the stable ingest order", async () => {
    const ui2 = new Ui();
    ui2.grid.rootId = "R1";
    ui2.grid.folder = "";
    ui2.grid.setItems([item("a"), item("b")]);
    ipcLog.folderDelta = {
      fromRevision: 0,
      toRevision: 7,
      reset: false,
      upserts: [item("c")],
      removedHashes: [],
    };

    await ui2.onIngestProgress(status({ running: true, imagesVersion: 0 }));
    await ui2.onIngestProgress(status({ running: true, imagesVersion: 1 }));
    await vi.advanceTimersByTimeAsync(INGEST_RELIST_DEBOUNCE_MS);
    expect(ui2.grid.items.map((entry) => entry.hash)).toEqual(["a", "b", "c"]);
    expect(lastCall("list_folder_delta")?.args?.sinceRevision).toBe(0);
  });

  it("the running→idle edge re-lists immediately and cancels any pending debounce", async () => {
    const ui2 = new Ui();
    ui2.grid.rootId = "R1";
    ui2.grid.folder = "";
    const calls = () => ipcLog.calls.filter((c) => c.cmd === "list_folder").length;
    const before = calls();

    await ui2.onIngestProgress(status({ running: true, imagesVersion: 0 })); // seed
    await ui2.onIngestProgress(status({ running: true, imagesVersion: 3 })); // arms debounce
    expect(calls()).toBe(before); // debounce pending, not yet fired

    // Scan settles: the idle edge re-lists ONCE, immediately, for the exact
    // final state — and must cancel the pending debounce so the burst does not
    // also fire a second, redundant list_folder after.
    await ui2.onIngestProgress(status({ running: false, imagesVersion: 3 }));
    expect(calls()).toBe(before + 1); // exact final state, un-debounced
    expect(ui2.shell.ingest.running).toBe(false);
    await vi.advanceTimersByTimeAsync(INGEST_RELIST_DEBOUNCE_MS * 2);
    expect(calls()).toBe(before + 1); // pending debounce was cancelled

    // Already idle, version unchanged: indicator only, no grid work.
    await ui2.onIngestProgress(status({ running: false, imagesVersion: 3 }));
    await vi.advanceTimersByTimeAsync(INGEST_RELIST_DEBOUNCE_MS);
    expect(calls()).toBe(before + 1);
  });
});

describe("ranking-signal toggles plumb to the semantic search (Phase 3)", () => {
  const searchArgs = () => lastCall("search")?.args;

  it("all-on omits the weights payload (today's default fusion preserved)", async () => {
    ui.query = "fog";
    await ui.runQueryScope("semantic");
    // Default all-on: no weights key, no includeDebug (popover closed).
    expect(searchArgs()).not.toHaveProperty("weights");
    expect(searchArgs()).not.toHaveProperty("includeDebug");
  });

  it("an unchecked signal sends weight 0 in the payload; semantic lane only", async () => {
    ui.query = "fog";
    // Turn S4 off (no live scope yet, so this does not re-run).
    await ui.setSignal("s4", false);
    await ui.runQueryScope("semantic");
    expect(searchArgs()?.weights).toEqual({ s1: 1.0, s2: 1.0, s3_each: 0.5, s4: 0.0 });
    expect(searchArgs()?.mode).toBe("semantic");
  });

  it("the LEXICAL lane never carries weights (the <100ms budget is untouched)", async () => {
    ui.query = "fog";
    await ui.setSignal("s4", false); // a non-default toggle is set
    await ui.runQueryScope("lexical");
    expect(searchArgs()?.mode).toBe("lexical");
    expect(searchArgs()).not.toHaveProperty("weights");
    expect(searchArgs()).not.toHaveProperty("includeDebug");
  });

  it("opening the ⚙ popover lights include_debug and surfaces per-signal debug", async () => {
    ui.query = "fog";
    await ui.runQueryScope("semantic"); // commit a semantic scope first
    // Opening the popover re-runs the live semantic scope WITH debug.
    await ui.setRankingPopover(true);
    expect(searchArgs()?.includeDebug).toBe(true);
    expect(ui.resultDebug.get("r1")?.per_signal?.[0]?.[0]).toBe("S1AnnotationChunk");
    // Closing it drops debug again (only paid while tuning).
    await ui.setRankingPopover(false);
    expect(ui.resultDebug.size).toBe(0);
  });

  it("toggling a signal does NOT re-run when no semantic scope is live", async () => {
    // A fresh ui in a plain folder scope: flipping a toggle persists but runs
    // no search (semantic-lane-only; the lexical path stays untouched).
    ipcLog.calls.length = 0;
    await ui.setSignal("s2", false);
    expect(ipcLog.calls.some((c) => c.cmd === "search")).toBe(false);
  });
});
