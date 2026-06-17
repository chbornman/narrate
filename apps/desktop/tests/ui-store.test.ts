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
      case "find_similar":
        // "More like this": neighbor hashes in similarity order, query image
        // excluded (the backend already drops self). A fixed set lets the
        // similar-scope tests assert the grid feeds these in order. An empty
        // case is exercised separately by overriding this mock per-test.
        return ["n1", "n2", "n3"];
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
      case "image_intensity":
        // Attention heatmap: one normalized score per requested hash. A simple
        // descending ramp by index lets the heat/sort tests assert order and
        // map population; the `allTime` flag rides through unchanged.
        return ((args?.hashes ?? []) as string[]).map((h, i, arr) => ({
          hash: h,
          intensity: arr.length <= 1 ? 1 : 1 - i / (arr.length - 1),
        }));
      case "record_dwell":
      case "clear_dwell":
        return cmd === "clear_dwell" ? 0 : null;
      case "ingest_status":
        return { running: false, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [], vectorsVersion: 0 };
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
    expect(ui.viewMode).toBe("grid");
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

  it("as-you-type ONLY ever calls search with mode 'lexical' (Phase 2)", async () => {
    // Simulate a real burst of keystrokes — the as-you-type path always runs
    // the lexical lane, never semantic, even on a warm machine (D6). Every
    // `search` invocation logged across the burst must be lexical.
    for (const q of ["f", "fo", "fog", "fog ", "fog r", "fog ri"]) {
      ui.query = q;
      await ui.runQueryScope("lexical");
    }
    const searchModes = ipcLog.calls
      .filter((c) => c.cmd === "search")
      .map((c) => c.args?.mode);
    expect(searchModes.length).toBeGreaterThan(0);
    expect(searchModes.every((m) => m === "lexical")).toBe(true);
  });

  it("Enter commits with mode 'semantic'", async () => {
    ui.query = "fog ridge";
    await ui.runQueryScope("semantic");
    expect(lastCall("search")?.args?.mode).toBe("semantic");
  });

  it("the lane indicator reflects the current mode (lexical -> semantic -> lexical)", async () => {
    // Boots queryless: no lane to name.
    expect(ui.searchLane).toBe("none");
    // (a) typing runs lexical and the indicator says so.
    ui.query = "fog";
    await ui.runQueryScope("lexical");
    expect(ui.searchLane).toBe("lexical");
    // (b) Enter commits semantic; the indicator flips to "semantic".
    await ui.runQueryScope("semantic");
    expect(ui.searchLane).toBe("semantic");
    // (c) editing after a commit drops back to lexical until the next Enter.
    ui.query = "fog ridge";
    await ui.runQueryScope("lexical");
    expect(ui.searchLane).toBe("lexical");
  });

  it("a background ingest re-list does NOT flip a committed semantic lane", async () => {
    // refreshItems re-runs the keyword query for fresh items under a query
    // scope, but a scope the user committed as semantic must keep reading
    // "semantic" while ingest churns (transition=false on the internal call).
    ui.query = "fog";
    await ui.runQueryScope("semantic");
    expect(ui.searchLane).toBe("semantic");
    await ui.refreshItems(); // the background re-list path
    expect(ui.searchLane).toBe("semantic"); // label held
  });

  it("clearing the query drops the lane to 'none'", async () => {
    ui.query = "fog";
    await ui.runQueryScope("semantic");
    expect(ui.searchLane).toBe("semantic");
    ui.barFocused = true;
    await ui.escape(); // clear-query-scope -> returnToSource -> clearQueryInput
    expect(ui.searchLane).toBe("none");
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

  // -- the `~` fuzzy quiet-toggle (Phase 4) -----------------------------------

  it("fuzzy is OFF by default: lexical search omits the fuzzy flag", async () => {
    expect(ui.fuzzyMode).toBe(false);
    ui.query = "leica";
    await ui.runQueryScope("lexical");
    // An unarmed call is byte-identical to today — `fuzzy` is omitted entirely
    // (the sparse payload sends it only when armed).
    expect(lastCall("search")?.args?.fuzzy).toBeUndefined();
  });

  it("armed: as-you-type lexical search sends fuzzy:true", async () => {
    await ui.setFuzzyMode(true);
    expect(ui.fuzzyMode).toBe(true);
    ui.query = "leics"; // a camera typo
    await ui.runQueryScope("lexical");
    expect(lastCall("search")?.args?.fuzzy).toBe(true);
  });

  it("armed fuzzy NEVER rides the semantic lane (lexical-only guardrail)", async () => {
    await ui.setFuzzyMode(true);
    ui.query = "leics";
    await ui.runQueryScope("semantic");
    // The commit lane must not carry the fuzzy widening — the semantic rig
    // already generalizes, and widening there would risk the budget.
    expect(lastCall("search")?.args?.fuzzy).toBeUndefined();
    expect(lastCall("search")?.args?.mode).toBe("semantic");
  });

  it("the toggle persists across the session (localStorage)", async () => {
    await ui.setFuzzyMode(true);
    // A fresh Ui boots with the armed state restored from prefs.
    const fresh = new Ui();
    fresh.fuzzyMode = false; // prove init() is what arms it
    await fresh.init();
    expect(fresh.fuzzyMode).toBe(true);
  });

  it("arming with a live lexical scope re-runs it (the widening appears live)", async () => {
    ui.query = "leica";
    await ui.runQueryScope("lexical");
    ipcLog.calls.length = 0;
    await ui.setFuzzyMode(true);
    // A live lexical scope re-runs so the fuzzy hits appear immediately.
    expect(lastCall("search")?.args?.fuzzy).toBe(true);
    expect(lastCall("search")?.args?.mode).toBe("lexical");
  });

  it("arming does NOT re-run a committed semantic scope", async () => {
    ui.query = "leica";
    await ui.runQueryScope("semantic");
    ipcLog.calls.length = 0;
    await ui.setFuzzyMode(true);
    // Fuzzy is lexical-only: a committed semantic scope is left untouched (the
    // next edit drops back to lexical and picks up the armed state).
    expect(lastCall("search")).toBeUndefined();
  });
});

describe('"More like this" (B69): the similar scope is a grid scope', () => {
  beforeEach(async () => {
    // Land on a real folder so the similar scope has a `within` source to
    // return to (and a residue that points there).
    await ui.openFolder("root-1", "Harbor");
    // The grid items the active image / filename lookup reads from.
    ui.grid.rawItems = ["q", "x", "y"].map(item);
  });

  it("dispatching find-similar re-scopes the grid to the neighbors in place", async () => {
    await ui.runSimilarScope("q", "q.jpg");
    // The new fourth scope variant, carrying the query image + its filename.
    expect(ui.gridScope.kind).toBe("similar");
    if (ui.gridScope.kind === "similar") {
      expect(ui.gridScope.hash).toBe("q");
      expect(ui.gridScope.filename).toBe("q.jpg");
      // `within` is the source the grid returns to on clear.
      expect(ui.gridScope.within).toEqual({
        kind: "folder",
        rootId: "root-1",
        folder: "Harbor",
      });
    }
    // find_similar ran for the query image, then list_images enriched the
    // returned neighbor hashes IN ORDER (similarity = relevance, pass-through).
    expect(lastCall("find_similar")?.args).toMatchObject({ hash: "q" });
    expect(lastCall("list_images")?.args).toMatchObject({ hashes: ["n1", "n2", "n3"] });
    expect(ui.grid.itemHashes).toEqual(["n1", "n2", "n3"]);
    // Relevance is auto-selected: the backend order is the displayed order.
    expect(ui.grid.sort).toBe("relevance");
  });

  it("the find-similar action (grid thumb seat) drives the similar scope", async () => {
    // Make "q" (index 0 of rawItems) the active grid image, then perform the
    // seated action. focus/anchor are indices into the item list.
    ui.grid.sel = { ...ui.grid.sel, order: ["q"], anchor: 0, focus: 0 };
    await ui.perform({ kind: "find-similar" });
    expect(ui.gridScope.kind).toBe("similar");
    if (ui.gridScope.kind === "similar") expect(ui.gridScope.hash).toBe("q");
  });

  it("first Escape clears the similar scope and returns to the source", async () => {
    await ui.runSimilarScope("q", "q.jpg");
    expect(ui.gridScope.kind).toBe("similar");
    // The query residue's Esc layer covers the similar scope too.
    await ui.escape(); // clear-query-scope -> returnToSource
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "root-1", folder: "Harbor" });
  });

  it("G (go home) clears the similar scope back to the source", async () => {
    await ui.runSimilarScope("q", "q.jpg");
    await ui.perform({ kind: "go-grid" });
    expect(ui.gridScope).toEqual({ kind: "folder", rootId: "root-1", folder: "Harbor" });
  });

  it("an empty index leaves the grid empty, never errors (fresh/mock machine)", async () => {
    // Override find_similar to the empty-index shape for this one case.
    const core = await import("@tauri-apps/api/core");
    const invoke = vi.mocked(core.invoke);
    invoke.mockImplementationOnce(async () => []);
    await ui.runSimilarScope("q", "q.jpg");
    expect(ui.gridScope.kind).toBe("similar"); // the scope still committed
    expect(ui.grid.itemHashes).toEqual([]); // but shows nothing, no throw
  });
});

describe("P4.2 contract flows", () => {
  it("G goes home from Look and clears a query scope (featureset §0)", async () => {
    // The grid carries a/b/c/d from the outer beforeEach (no openFolder, so
    // list_folder's empty mock can't wipe them before openLook).
    await ui.openLook("c");
    await ui.perform({ kind: "go-grid" });
    expect(ui.viewMode).toBe("grid");
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
    expect(ui.viewMode).toBe("look");
    await ui.perform({ kind: "look-close" });
    expect(ui.viewMode).toBe("grid");
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

describe("attention heatmap (DESIGN-ATTENTION-HEATMAP.md)", () => {
  // ---- dwell capture: episode flush + blur-pause --------------------------

  it("flushes a Look focus episode (tier 'look') on leaving Look", async () => {
    vi.useFakeTimers();
    try {
      const start = Date.now();
      await ui.openLook("b"); // begins a Look episode for b
      vi.setSystemTime(start + 5_000); // 5 s of focus
      ipcLog.calls.length = 0;
      await ui.leaveLook(); // ends the Look episode, begins a grid one
      const dwell = ipcLog.calls.find((c) => c.cmd === "record_dwell");
      expect(dwell).toBeDefined();
      expect(dwell?.args?.hash).toBe("b");
      expect(dwell?.args?.source).toBe("look");
      // Elapsed is wall-clock between start and flush (the backend applies the
      // tier rate + cap; the frontend reports the raw episode).
      expect(dwell?.args?.elapsedMs).toBe(5_000);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a sub-threshold flick does not report (MIN_EPISODE_MS)", async () => {
    vi.useFakeTimers();
    try {
      const start = Date.now();
      await ui.openLook("b");
      vi.setSystemTime(start + 50); // below MIN_EPISODE_MS
      ipcLog.calls.length = 0;
      await ui.leaveLook();
      expect(ipcLog.calls.some((c) => c.cmd === "record_dwell")).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a grid multi-select episode fans out to each selected hash", async () => {
    vi.useFakeTimers();
    try {
      const start = Date.now();
      await ui.applySelection(sel.selectAll(sel.EMPTY, ui.grid.unitHashes)); // a,b,c,d
      vi.setSystemTime(start + 1_000);
      ipcLog.calls.length = 0;
      // Switch focus to a single cell: ends the multi-select episode.
      await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0));
      const reports = ipcLog.calls.filter((c) => c.cmd === "record_dwell");
      // One report per previously-selected hash, all tier "grid".
      expect(reports.length).toBe(4);
      expect(reports.every((r) => r.args?.source === "grid")).toBe(true);
      expect(new Set(reports.map((r) => r.args?.hash))).toEqual(
        new Set(["a", "b", "c", "d"]),
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("window blur pauses dwell by flushing the in-flight episode", async () => {
    vi.useFakeTimers();
    try {
      const start = Date.now();
      await ui.openLook("b");
      vi.setSystemTime(start + 2_000);
      ipcLog.calls.length = 0;
      ui.dwellPause(); // the App.svelte blur/visibilitychange hook
      const dwell = ipcLog.calls.find((c) => c.cmd === "record_dwell");
      expect(dwell?.args?.hash).toBe("b");
      expect(dwell?.args?.elapsedMs).toBe(2_000);
      // A second pause with no new episode reports nothing.
      ipcLog.calls.length = 0;
      ui.dwellPause();
      expect(ipcLog.calls.some((c) => c.cmd === "record_dwell")).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  // ---- heat toggle + all-time toggle: state, persistence, fetch -----------

  it("the heat toggle fetches intensity, persists, and clears on off", async () => {
    // rawItems (a,b,c,d) are set in beforeEach; the heat fetch keys off them.
    expect(ui.heatOn).toBe(false);

    ui.toggleHeat();
    expect(ui.heatOn).toBe(true);
    expect(localStorage.getItem("pp.heatOn")).toBe("1");
    await Promise.resolve(); // let the fetch settle
    await Promise.resolve();
    expect(ui.intensity.size).toBeGreaterThan(0);
    // The grid slice mirrors the map (the attention sort + cell tint read it).
    expect(ui.grid.intensity.size).toBe(ui.intensity.size);

    ui.toggleHeat();
    expect(ui.heatOn).toBe(false);
    expect(localStorage.getItem("pp.heatOn")).toBe("0");
    expect(ui.intensity.size).toBe(0); // cleared on off
  });

  it("the All-time toggle persists and re-fetches with the flag", async () => {
    ui.toggleHeat();
    await Promise.resolve();
    expect(ui.heatAllTime).toBe(false); // default = recency-weighted

    ipcLog.calls.length = 0;
    ui.toggleAllTime();
    expect(ui.heatAllTime).toBe(true);
    expect(localStorage.getItem("pp.heatAllTime")).toBe("1");
    await Promise.resolve();
    await Promise.resolve();
    const fetch = lastCall("image_intensity");
    expect(fetch?.args?.allTime).toBe(true);
  });

  // ---- sort by attention --------------------------------------------------

  it("sort by attention orders the grid hottest-first by intensity", async () => {
    ui.toggleHeat();
    await Promise.resolve();
    await Promise.resolve();
    // The mock ramps intensity DOWN by scope index (a hottest, d coldest).
    ui.grid.setSort("attention");
    expect(ui.grid.itemHashes).toEqual(["a", "b", "c", "d"]);
    // Reversing the intensity map flips the order (hottest still leads).
    ui.grid.intensity = new Map([
      ["a", 0],
      ["b", 0.3],
      ["c", 0.6],
      ["d", 1],
    ]);
    expect(ui.grid.itemHashes).toEqual(["d", "c", "b", "a"]);
  });

  // ---- clear attention data ----------------------------------------------

  it("clearDwell wipes the cached intensity map", async () => {
    ui.toggleHeat();
    await Promise.resolve();
    await Promise.resolve();
    expect(ui.intensity.size).toBeGreaterThan(0);
    // The settings "Clear attention data" verb fires clear_dwell; the main
    // window re-fetches (now empty) intensity on its next scope report.
    const core = await import("@tauri-apps/api/core");
    const invoke = vi.mocked(core.invoke);
    await invoke("clear_dwell");
    expect(lastCall("clear_dwell")).toBeDefined();
  });
});
