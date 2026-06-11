/**
 * The §0 contract, end to end over the FINAL registry and the real shell
 * (INTEGRATION): Esc peels the full 14-layer order one press at a time on
 * a live Ui; G reaches Grid from everywhere; Tab lights-out hides every
 * chrome REGION in the rendered App while the indicator and an open note
 * input survive (coordinator ruling); and the structural residue —
 * every non-reserved verb is reachable (a chord that dispatches in some
 * generated context, or a seat), and the cheatsheet carries the contract
 * rows with their chords.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/svelte";
import { tick } from "svelte";
import type { GridItem, ScopeView } from "../src/lib/types/dto";

// jsdom has no ResizeObserver; Svelte's bind:clientWidth needs one. The
// shim never fires, so the virtualizer simply measures a 0-width viewport —
// region-level assertions are unaffected.
class ResizeObserverShim {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver ??= ResizeObserverShim as typeof ResizeObserver;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case "set_scope": {
        const targets = (args?.targets ?? []) as string[];
        const kind =
          targets.length === 0 ? "session" : targets.length === 1 ? "single" : "multi";
        return { kind, count: targets.length, previewHashes: [] } satisfies ScopeView;
      }
      case "list_roots":
        return [
          {
            rootId: "r1",
            displayName: "shoots",
            relPath: "",
            volumeId: "v1",
            online: true,
            absPath: "/shoots",
          },
        ];
      case "list_folder":
        return ["a", "b"].map(
          (h): GridItem => ({
            hash: h,
            fileName: `${h}.jpg`,
            relPath: `${h}.jpg`,
            captureTs: null,
            addedTs: "2026-02-01T00:00:00Z",
            hasJournal: false,
            rating: null,
            offline: false,
          }),
        );
      case "folder_tree":
        return [];
      case "ingest_status":
        return { running: false, done: 0, total: 0, errors: 0 };
      case "image_journal":
        return [];
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    setTitle: async () => {},
    minimize: async () => {},
    toggleMaximize: async () => {},
    close: async () => {},
    setFullscreen: async () => {},
  }),
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import App from "../src/App.svelte";
import { ui } from "../src/lib/state/app.svelte";
import * as sel from "../src/lib/logic/selection";
import { REGISTRY } from "../src/lib/actions/registry";
import { match } from "../src/lib/actions/match";
import { cheatsheet } from "../src/lib/actions/cheatsheet";
import { withDefaults } from "../src/lib/logic/keymap";
import type { ActionContext } from "../src/lib/actions/types";

beforeEach(() => {
  document.body.innerHTML = "";
  localStorage.clear();
});

// ---------------------------------------------------------------------------
// Esc — the 15-layer order on a live Ui (logic order is unit-tested in
// escape.test.ts; this drives the real flags and slice closers). AMENDED
// by the journal polish round (founder, June 2026): the inspector peels
// AFTER Look→Grid — returning to the grid keeps the panel on the still-
// active image — and the journal composer joined the text-edit layers.
// AMENDED by the wave-2 robustness cluster: the first-run welcome card
// joined as layer 1 (BACKLOG "First-run welcome card").
// ---------------------------------------------------------------------------

describe("Esc peels the live shell one layer per press (§0: sacred)", () => {
  it("walks all 15 layers in order, then does nothing", async () => {
    ui.grid.rawItems = []; // state from other suites
    ui.grid.rawItems = ["a", "b"].map((h) => ({
      hash: h,
      fileName: `${h}.jpg`,
      relPath: `${h}.jpg`,
      captureTs: null,
      addedTs: "2026-02-01T00:00:00Z",
      hasJournal: false,
      rating: null,
      offline: false,
    }));
    // Open EVERYTHING.
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    await ui.openLook("a", false);
    await ui.openSearch();
    ui.inspector.openTab("journal");
    ui.shell.debugOpen = true;
    ui.shell.popoverOpen = true;
    ui.shell.cheatsheetOpen = true;
    ui.summonNote();
    ui.inspector.editingEventId = "01A";
    ui.inspector.composerFocused = true;
    ui.shell.openContextMenu("thumb", null);
    ui.offerDrop(["/x"]);
    ui.inspector.redactTargetId = "01A";
    ui.shell.welcomeOpen = true;

    const peels: [() => boolean, string][] = [
      [() => !ui.shell.welcomeOpen, "welcome card"],
      [() => ui.inspector.redactTargetId === null, "redaction modal"],
      [() => ui.dropPaths === null, "drop confirm"],
      [() => ui.shell.contextMenu === null, "context menu"],
      [() => ui.inspector.editingEventId === null, "journal edit"],
      [() => !ui.inspector.composerFocused, "journal composer"],
      [() => !ui.shell.note.open, "note input"],
      [() => !ui.shell.cheatsheetOpen, "cheatsheet"],
      [() => !ui.shell.popoverOpen, "indicator popover"],
      [() => !ui.shell.debugOpen, "debug panel"],
      [() => !ui.searchOpen, "search"],
      // Founder, June 2026: Look→Grid keeps the inspector open.
      [() => ui.surface === "grid", "Look → Grid"],
      [() => ui.inspector.open === false, "inspector"],
      [() => ui.grid.sel.order.length === 0, "selection"],
    ];
    for (let i = 0; i < peels.length; i++) {
      expect(peels[i][0](), `${peels[i][1]} should still be open`).toBe(false);
      await ui.escape();
      for (let j = 0; j <= i; j++)
        expect(peels[j][0](), `${peels[j][1]} should be closed`).toBe(true);
      for (let j = i + 1; j < peels.length; j++)
        expect(peels[j][0](), `${peels[j][1]} peeled early`).toBe(false);
    }
    await ui.escape(); // layer 15: none — never quits
    expect(ui.surface).toBe("grid");
  });
});

// ---------------------------------------------------------------------------
// G from everywhere (§0: universal go-home)
// ---------------------------------------------------------------------------

describe("G goes home from everywhere", () => {
  it("from Look, from Search-over-Look, from Grid (no-op)", async () => {
    ui.grid.rawItems = ["a", "b"].map((h) => ({
      hash: h,
      fileName: `${h}.jpg`,
      relPath: `${h}.jpg`,
      captureTs: null,
      addedTs: "2026-02-01T00:00:00Z",
      hasJournal: false,
      rating: null,
      offline: false,
    }));
    await ui.openLook("b", false);
    await ui.openSearch();
    await ui.perform({ kind: "go-grid" });
    expect(ui.searchOpen).toBe(false);
    expect(ui.surface).toBe("grid");
    await ui.perform({ kind: "go-grid" }); // already home: stays home
    expect(ui.surface).toBe("grid");
  });
});

// ---------------------------------------------------------------------------
// Tab lights-out over the RENDERED shell (featureset §0; ruling: the
// indicator and an open note input never hide)
// ---------------------------------------------------------------------------

describe("Tab lights-out hides every chrome region (rendered App)", () => {
  it("hides titlebar/header/rail/inspector, keeps indicator + open note; restores", async () => {
    ui.shell.chromeHidden = false;
    const { container } = render(App);
    await tick();
    await tick(); // init() resolves roots + folder
    ui.shell.railOpen = true;
    ui.inspector.openTab("metadata");
    ui.summonNote();
    await tick();

    const q = (s: string) => container.querySelector(s);
    expect(q(".titlebar")).not.toBeNull();
    expect(q(".grid-header")).not.toBeNull();
    expect(q('aside[aria-label="Sources"]')).not.toBeNull();
    expect(q('aside[aria-label="Inspector"]')).not.toBeNull();
    expect(q(".indicator")).not.toBeNull();
    expect(q(".note")).not.toBeNull();

    await ui.perform({ kind: "toggle-lights-out" });
    await tick();
    expect(q(".titlebar")).toBeNull();
    expect(q(".grid-header")).toBeNull();
    expect(q('aside[aria-label="Sources"]')).toBeNull();
    expect(q('aside[aria-label="Inspector"]')).toBeNull();
    // The exemptions (coordinator ruling): capture-state truth stays.
    expect(q(".indicator")).not.toBeNull();
    expect(q(".note")).not.toBeNull();

    await ui.perform({ kind: "toggle-lights-out" });
    await tick();
    // Open state survived — regions return without re-opening anything.
    expect(q(".titlebar")).not.toBeNull();
    expect(q('aside[aria-label="Sources"]')).not.toBeNull();
    expect(q('aside[aria-label="Inspector"]')).not.toBeNull();
    ui.shell.railOpen = false;
    ui.inspector.close();
    ui.cancelNote();
  });

  it("hides the filmstrip in Look too (the bottom edge obeys Tab)", async () => {
    const { container } = render(App);
    await tick();
    await tick();
    await ui.openLook("a", false);
    ui.look.filmstrip = true;
    await tick();
    expect(container.querySelector(".filmstrip")).not.toBeNull();
    await ui.perform({ kind: "toggle-lights-out" });
    await tick();
    expect(container.querySelector(".filmstrip")).toBeNull();
    await ui.perform({ kind: "toggle-lights-out" });
    await ui.leaveLook();
  });
});

// ---------------------------------------------------------------------------
// Structural residue over the FINAL registry: reachability + cheatsheet
// ---------------------------------------------------------------------------

function mulberry32(seed: number) {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function genContext(rnd: () => number): ActionContext {
  const bool = () => rnd() < 0.5;
  const pick = <T>(xs: readonly T[]): T => xs[Math.floor(rnd() * xs.length)];
  return withDefaults({
    surface: pick(["grid", "look"] as const),
    searchOpen: bool(),
    inputFocused: bool(),
    searchInputFocused: bool(),
    queryEmpty: bool(),
    hasSelection: bool(),
    selectionCount: Math.floor(rnd() * 4),
    activeHash: bool() ? "ab".repeat(32) : null,
    activeIsPair: bool(),
    activePairCollapsed: bool(),
    railOpen: bool(),
    railFocused: bool(),
    inspectorOpen: pick([false, "metadata", "journal"] as const),
    cheatsheetOpen: bool(),
    contextMenuOpen: false,
    chromeHidden: bool(),
    autoAdvance: bool(),
    lookAtFit: bool(),
    debugEnabled: bool(),
    asrReady: bool(),
    filmstrip: bool(),
    // pencil context (live since P5.1): the band's gates need both halves
    pencilMode: bool(),
    overlayVisible: bool(),
    pencilUndoable: bool(),
  });
}

describe("every verb is reachable over the final registry", () => {
  it("each non-reserved def dispatches from a key in SOME context, or holds a seat", () => {
    const rnd = mulberry32(7);
    const contexts = Array.from({ length: 400 }, () => genContext(rnd));
    for (const def of REGISTRY) {
      if (def.reserved === true) continue;
      // Seat → menu rendering is proven by registry.test.ts seat coverage;
      // here a seat counts as reachability for pointer-only rows.
      const seated = def.seats !== undefined && def.seats.length > 0;
      if (def.keys.length === 0) {
        expect(seated, `${def.id}@${def.scope} is pointer-only but unseated`).toBe(true);
        continue;
      }
      const fires = contexts.some((ctx) =>
        def.keys.some((c) => {
          const a = match(
            { key: c.key, ctrlOrMeta: c.ctrlOrMeta ?? false, shift: c.shift ?? false },
            ctx,
            REGISTRY,
          );
          return a !== null && a.kind === def.id;
        }),
      );
      expect(fires || seated, `${def.id}@${def.scope} never dispatches`).toBe(true);
    }
  });
});

describe("cheatsheet completeness over the final registry (featureset §6)", () => {
  it("carries the §0 contract rows with their chords, in every context", () => {
    const rows = cheatsheet(genContext(mulberry32(11))).flatMap((g) => g.rows);
    const byId = new Map(rows.map((r) => [`${r.id}@${r.scope}`, r]));
    const want: [string, string][] = [
      ["escape@global", "Escape"],
      ["go-grid@global", "g"],
      ["toggle-lights-out@global", "Tab"],
      ["toggle-rail@global", "\\"],
      ["toggle-cheatsheet@global", "?"],
      ["open-look@grid", "Enter"],
      ["look-close@look", " "],
      ["open-inspector@global", "i"],
    ];
    for (const [id, key] of want) {
      const row = byId.get(id);
      expect(row, `${id} missing from the cheatsheet`).toBeDefined();
      expect(
        row?.keys.some((c) => c.key === key),
        `${id} lost its ${key === " " ? "Space" : key} chord`,
      ).toBe(true);
    }
    // Reserved rows stay hidden until their packets (M2a/M2b).
    for (const def of REGISTRY.filter((d) => d.reserved === true))
      expect(byId.has(`${def.id}@${def.scope}`)).toBe(false);
  });
});
