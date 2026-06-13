/**
 * The composition root (Ui) against mocked IPC: selection → reported scope
 * (the UI performs no scope logic — it reports and renders the echo, UI
 * §3.4), rating-key semantics over the echoed scope, the note transient,
 * auto-advance wiring (logic/advance.ts), the Search overlay's return
 * point (I1), and the new P4.2 cross-slice flows (G, lights-out survival).
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
      case "list_images":
        // Enrich the (fused-order) result hashes into GridItems. runQueryScope
        // feeds these to the grid under relevance sort (the factory is
        // hoisted, so the item shape is inlined rather than reusing item()).
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
      case "ingest_status":
        return { running: false, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0 };
      case "toggle_mic":
      case "set_mic": {
        // Echo the §11 indicator the way the core does — set_mic lands
        // the DESIRED state (the idempotent primitive); the toggle tests
        // below only assert which command fired, so a fixed armed echo
        // suffices for toggle_mic.
        const armed = cmd === "set_mic" ? args?.armed === true : true;
        return {
          currentScope: { kind: "session", count: 0, previewHashes: [] },
          mic: armed ? "armedIdle" : "disarmed",
          streamingUtterance: null,
          degraded: { asrUnavailable: false },
        };
      }
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";
import * as sel from "../src/lib/logic/selection";
import { MIC_HOLD_MS } from "../src/lib/logic/michold";

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
  ui.grid.rawItems = ["a", "b", "c", "d"].map(item);
});

describe("selection → scope reporting (CAPTURE §3, UI §3.4)", () => {
  it("reports ordered targets and renders the core's echo", async () => {
    let s = sel.click(sel.EMPTY, ui.grid.unitHashes, 2); // c (sorted by filename)
    s = sel.toggle(s, ui.grid.unitHashes, 0); // + a
    await ui.applySelection(s);
    expect(lastCall("set_scope")?.args?.targets).toEqual(["c", "a"]);
    expect(ui.shell.scope).toEqual({ kind: "multi", count: 2, previewHashes: ["c", "a"] });
  });

  it("clearing the selection reports session scope (zero targets)", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    await ui.applySelection(sel.clear(ui.grid.sel));
    expect(lastCall("set_scope")?.args?.targets).toEqual([]);
    expect(ui.shell.scope.kind).toBe("session");
  });

  it("entering Look narrows scope to the viewed image; leaving restores", async () => {
    await ui.applySelection(sel.selectAll(sel.EMPTY, ui.grid.unitHashes));
    await ui.openLook("b");
    expect(lastCall("set_scope")?.args?.targets).toEqual(["b"]);
    expect(ui.shell.scope.kind).toBe("single");
    await ui.leaveLook();
    expect(lastCall("set_scope")?.args?.targets).toEqual(["a", "b", "c", "d"]);
    expect(ui.shell.scope.count).toBe(4);
  });
});

describe("rating keys over the echoed scope (CAPTURE §10, C6)", () => {
  it("session scope: rating keys do nothing", async () => {
    await ui.rate(3);
    expect(lastCall("set_rating")).toBeUndefined();
  });

  it("with a selection, one keystroke → one command (multi applies to all)", async () => {
    await ui.applySelection(sel.selectAll(sel.EMPTY, ui.grid.unitHashes));
    await ui.rate(3);
    expect(lastCall("set_rating")?.args).toEqual({ value: 3 });
  });

  it("0 is sent as an explicit clear, not suppressed", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
    await ui.rate(0);
    expect(lastCall("set_rating")?.args).toEqual({ value: 0 });
  });
});

describe("auto-advance wiring (featureset §4, D7 default OFF)", () => {
  it("OFF by default: rating a single selection does not advance", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 1));
    await ui.rate(3);
    expect(ui.grid.sel.focus).toBe(1);
  });

  it("ON: a single-selection rating advances the active image in Grid", async () => {
    ui.autoAdvance = true;
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 1));
    await ui.rate(3);
    expect(ui.grid.sel.focus).toBe(2);
    expect(ui.grid.sel.order).toEqual(["c"]); // single-selection invariant
    expect(ui.shell.scope.count).toBe(1); // scope re-reported
  });

  it("ON: a multi-select rating NEVER advances or destroys the selection", async () => {
    ui.autoAdvance = true;
    await ui.applySelection(sel.selectAll(sel.EMPTY, ui.grid.unitHashes));
    await ui.rate(4);
    expect(ui.grid.sel.order.length).toBe(4);
  });

  it("ON: rating in Look advances within the navigation set", async () => {
    ui.autoAdvance = true;
    await ui.openLook("b");
    await ui.rate(5);
    expect(ui.look.currentHash).toBe("c");
  });

  it("ON: a note submitted from Look advances; from Grid it does not", async () => {
    ui.autoAdvance = true;
    await ui.openLook("a");
    ui.summonNote();
    await ui.submitNote("the hand is the whole picture");
    expect(ui.look.currentHash).toBe("b");
    await ui.leaveLook();
    ui.summonNote();
    await ui.submitNote("session thought");
    expect(ui.surface).toBe("grid");
  });
});

describe("typed-note transient (UI §6)", () => {
  it("summon snapshots the scope; submit sends the text and vanishes", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 1));
    ui.summonNote();
    expect(ui.shell.note.open).toBe(true);
    expect(ui.shell.note.snapshot?.count).toBe(1);
    await ui.submitNote("the hand is the whole picture");
    expect(ui.shell.note.open).toBe(false);
    expect(lastCall("add_note")?.args).toEqual({
      text: "the hand is the whole picture",
    });
  });

  it("a selection change while the input is open cancels it (scope frozen at summon)", async () => {
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 1));
    ui.summonNote();
    await ui.applySelection(sel.selectAll(sel.EMPTY, ui.grid.unitHashes));
    expect(ui.shell.note.open).toBe(false);
    // ...and no note was sent.
    expect(lastCall("add_note")).toBeUndefined();
  });
});

describe("search-as-scope (M3): the query is a grid scope", () => {
  beforeEach(async () => {
    // Land the grid on a real folder so a query has a `within` source to
    // return to (and to assert the residue points there).
    await ui.openFolder("root-1", "Harbor");
  });

  it("committing a query re-scopes the grid in place (relevance sort)", async () => {
    ui.query = "fog ba";
    ui.chips = [{ type: "rating", op: "gte", value: 3 }];
    await ui.runQueryScope("semantic");
    expect(ui.gridScope.kind).toBe("query");
    // The semantic lane was forced on the wire, and relevance auto-selected.
    expect(lastCall("search")?.args).toMatchObject({
      query: "fog ba",
      filters: [{ type: "rating", op: "gte", value: 3 }],
      mode: "semantic",
    });
    expect(ui.grid.sort).toBe("relevance");
  });

  it("as-you-type runs the LEXICAL lane (the <100 ms guardrail)", async () => {
    ui.query = "fog";
    await ui.runQueryScope("lexical");
    expect(lastCall("search")?.args?.mode).toBe("lexical");
  });

  it("an empty bar is no scope — it returns the grid to its source", async () => {
    ui.query = "fog";
    await ui.runQueryScope("lexical");
    expect(ui.gridScope.kind).toBe("query");
    ui.query = ""; // dropped below the threshold, no chips
    await ui.runQueryScope("lexical");
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "root-1", folder: "Harbor" });
  });

  it("a sub-threshold keystroke does NOT erase the in-progress query text", async () => {
    // As-you-type fires runQueryScope on every keystroke. Typing the first
    // character (below MIN_QUERY_CHARS) must leave the bar's text intact —
    // the input is bind:value'd to ui.query, so clearing it here would erase
    // the character under the user (regression guard).
    ui.query = "f"; // one char: below the 2-char threshold, no chips
    await ui.runQueryScope("lexical");
    expect(ui.query).toBe("f"); // text survives; the grid just hasn't scoped
    expect(ui.gridScope.kind).toBe("folder"); // no query scope formed yet
  });

  it("first Escape clears the query scope and returns to the source", async () => {
    ui.query = "fog";
    await ui.runQueryScope("lexical");
    ui.barFocused = true;
    await ui.escape(); // clear-query-scope
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "root-1", folder: "Harbor" });
    expect(ui.query).toBe(""); // the bar cleared so you SEE where you land
  });

  it("removing a chip re-runs the live lexical query (UI §5.1)", async () => {
    ui.query = "fog";
    ui.chips = [
      { type: "rating", op: "gte", value: 3 },
      { type: "has_strokes", value: true },
    ];
    await ui.runQueryScope("lexical");
    ipcLog.calls.length = 0;
    await ui.removeChip(1);
    expect(ui.chips).toEqual([{ type: "rating", op: "gte", value: 3 }]);
    expect(lastCall("search")?.args?.mode).toBe("lexical");
  });

  it("`/` focuses the bar (no overlay)", () => {
    const before = ui.focusBarRequest;
    void ui.perform({ kind: "open-search" });
    expect(ui.focusBarRequest).toBe(before + 1);
  });
});

describe("P4.2 contract flows", () => {
  it("G goes home from Look and clears a query scope (featureset §0)", async () => {
    // The grid carries a/b/c/d from the outer beforeEach (no openFolder, so
    // list_folder's empty mock can't wipe them before openLook).
    await ui.openLook("c");
    await ui.perform({ kind: "go-grid" });
    expect(ui.surface).toBe("grid");
    // The same image stays active back in the grid.
    expect(ui.grid.unitHashes[ui.grid.sel.focus]).toBe("c");
    // And from a query scope, G returns to the underlying source (a folder).
    ui.query = "fog";
    await ui.runQueryScope("lexical");
    expect(ui.gridScope.kind).toBe("query");
    await ui.perform({ kind: "go-grid" });
    expect(ui.gridScope.kind).toBe("folder");
  });

  // AMENDED by the layout-architecture round (founder, June 12 2026):
  // lights-out is a SNAPSHOT-RESTORE — hiding records the open-panel set
  // and CLOSES the panels (they render from `open` alone now); Tab again
  // restores exactly that set.
  it("lights-out snapshots open state and Tab twice restores it", async () => {
    ui.shell.railOpen = true;
    await ui.perform({ kind: "toggle-lights-out" });
    expect(ui.shell.chromeHidden).toBe(true);
    expect(ui.shell.railOpen).toBe(false); // closed; recorded in the snapshot
    await ui.perform({ kind: "toggle-lights-out" });
    expect(ui.shell.chromeHidden).toBe(false);
    expect(ui.shell.railOpen).toBe(true);
  });

  // Since June 12 2026 no key produces look-close (Space is the mic; Esc
  // routes through the escape ladder), but the Action keeps its perform
  // semantics — pointer paths and the frozen union both rely on it.
  it("Look entry/close is symmetric: look-close ≡ Escape", async () => {
    await ui.openLook("b");
    expect(ui.surface).toBe("look");
    await ui.perform({ kind: "look-close" });
    expect(ui.surface).toBe("grid");
    expect(ui.grid.unitHashes[ui.grid.sel.focus]).toBe("b"); // same image active
  });
});

describe("Space two-gesture mic (CAPTURE §6.4 — tap toggles, hold is push-to-talk)", () => {
  // michold.ts takes time as data, so hold gestures are simulated by
  // REWINDING the recorded press timestamp past the threshold — no fake
  // timers, no Date.now mocking (the confirmhold.test.ts spirit).
  const rewindPress = () => {
    ui.micHold = { ...ui.micHold, pressedAt: Date.now() - MIC_HOLD_MS };
  };
  const setMicCalls = () => ipcLog.calls.filter((c) => c.cmd === "set_mic");

  it("press from disarmed arms IMMEDIATELY; a quick release keeps it armed (tap = toggle on)", async () => {
    await ui.perform({ kind: "mic-press" });
    expect(lastCall("set_mic")?.args).toEqual({ armed: true });
    expect(ui.shell.mic).toBe("armedIdle");
    await ui.micRelease(); // released within the same tick: a tap
    expect(setMicCalls()).toHaveLength(1); // no disarm shipped
    expect(ui.shell.mic).toBe("armedIdle");
  });

  it("hold from disarmed is push-to-talk: release DISARMS explicitly (never a blind toggle)", async () => {
    await ui.perform({ kind: "mic-press" });
    rewindPress();
    await ui.micRelease();
    expect(lastCall("set_mic")?.args).toEqual({ armed: false });
    expect(ui.shell.mic).toBe("disarmed");
  });

  it("from armed: press is silent, a hold is inert, and only a TAP toggles off", async () => {
    ui.shell.mic = "armedIdle"; // armed earlier via toggle
    await ui.perform({ kind: "mic-press" });
    expect(setMicCalls()).toHaveLength(0); // press decides nothing from armed
    rewindPress();
    await ui.micRelease(); // hold from armed: PTT only applies from disarmed
    expect(setMicCalls()).toHaveLength(0);
    expect(ui.shell.mic).toBe("armedIdle");
    await ui.perform({ kind: "mic-press" });
    await ui.micRelease(); // immediate release = tap → toggle OFF
    expect(lastCall("set_mic")?.args).toEqual({ armed: false });
    expect(ui.shell.mic).toBe("disarmed");
  });

  it("auto-repeat keydowns are absorbed: one arm, and the FIRST press times the hold", async () => {
    await ui.perform({ kind: "mic-press" });
    rewindPress();
    await ui.perform({ kind: "mic-press" }); // auto-repeat mid-hold
    expect(setMicCalls()).toHaveLength(1); // armed once
    await ui.micRelease(); // still measured from the first press: PTT
    expect(lastCall("set_mic")?.args).toEqual({ armed: false });
  });

  it("window blur mid-gesture disarms the mic THIS gesture opened (a hold never wedges)", async () => {
    await ui.perform({ kind: "mic-press" });
    await ui.micWindowBlur();
    expect(lastCall("set_mic")?.args).toEqual({ armed: false });
    expect(ui.shell.mic).toBe("disarmed");
  });

  it("a stray keyup (the press was suppressed while typing) drives no IPC", async () => {
    await ui.micRelease();
    expect(setMicCalls()).toHaveLength(0);
  });

  it("the pointer form (indicator click) stays a plain toggle on toggle_mic", async () => {
    await ui.perform({ kind: "toggle-mic" });
    expect(lastCall("toggle_mic")).toBeDefined();
    expect(setMicCalls()).toHaveLength(0);
  });
});

describe("Tab lights-out snapshot-restore (founder, June 12 2026)", () => {
  it("restores exactly the open set: rail + inspector tab + filmstrip", async () => {
    ui.shell.railOpen = true;
    ui.inspector.openTab("journal");
    ui.look.filmstrip = true;
    await ui.perform({ kind: "toggle-lights-out" });
    // Everything closes — panels render from their own open flags.
    expect(ui.shell.railOpen).toBe(false);
    expect(ui.inspector.open).toBe(false);
    expect(ui.look.filmstrip).toBe(false);
    await ui.perform({ kind: "toggle-lights-out" });
    // ... and comes back exactly as it was, tab included.
    expect(ui.shell.railOpen).toBe(true);
    expect(ui.inspector.open).toBe("journal");
    expect(ui.look.filmstrip).toBe(true);
  });

  it("nothing open → nothing restored (never a fixed default set)", async () => {
    ui.shell.railOpen = false;
    expect(ui.inspector.open).toBe(false);
    await ui.perform({ kind: "toggle-lights-out" });
    await ui.perform({ kind: "toggle-lights-out" });
    expect(ui.shell.railOpen).toBe(false);
    expect(ui.inspector.open).toBe(false);
    expect(ui.look.filmstrip).toBe(false);
  });

  it("lights-out never rewrites the panels' standing prefs", async () => {
    ui.shell.toggleRail(); // user intent: open (persists "1")
    expect(localStorage.getItem("pp.railOpen")).toBe("1");
    ui.look.toggleFilmstrip(); // user intent: shown (persists "1")
    expect(localStorage.getItem("pp.filmstrip")).toBe("1");
    await ui.perform({ kind: "toggle-lights-out" });
    // The snapshot close is NOT a toggle: a quit while hidden must
    // relaunch with the user's standing layout.
    expect(localStorage.getItem("pp.railOpen")).toBe("1");
    expect(localStorage.getItem("pp.filmstrip")).toBe("1");
  });

  it("hiding does not steal or grant rail keyboard focus on restore", async () => {
    ui.shell.toggleRail(); // open + focused (the \ handoff)
    expect(ui.shell.railFocused).toBe(true);
    await ui.perform({ kind: "toggle-lights-out" });
    expect(ui.shell.railFocused).toBe(false); // hidden rail can't hold keys
    await ui.perform({ kind: "toggle-lights-out" });
    expect(ui.shell.railOpen).toBe(true);
    expect(ui.shell.railFocused).toBe(false); // restore is layout, not focus
  });
});
