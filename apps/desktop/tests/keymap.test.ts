/**
 * Keyboard map dispatch — the interpreter (logic/keymap.ts) over the typed
 * action registry. Every P3.2 row is asserted; the mic row (M) stays
 * reserved and dispatches to NOTHING; the pencil band went live in P5.1
 * (its block below carries the amendment citation); single-letter keys are
 * suppressed while a text input is focused (UI §11).
 *
 * EXACTLY THREE existing expectations are amended in P4.2, each citing its
 * founder decision in place:
 *   (1) Tab → lights-out, `\` → toggle-rail              (D5)
 *   (2) Space in Grid → open-look (§0 symmetric open/close);
 *       selection-toggle moves to Ctrl+Space             (DECISIONS entry 1)
 *   (3) rail arrow routing gates on railFocused, not railOpen
 *       (the rail is push-persistent)                    (DECISIONS entry 3)
 * The P3.2 "J does not exist" guard is RETIRED (not amended): D2 pulls the
 * journal panel into P4.2 — the J/I rows land in Stage C's defs/inspector.ts
 * and are tested in inspector-keys.test.ts, never here.
 *
 * New P4.2 rows are asserted in their own block below; per-stage rows
 * (grid/look/inspector) get their own test files — existing blocks are
 * never edited for them.
 */
import { describe, expect, it } from "vitest";
import { dispatch, type KeyContext, type KeyInput } from "../src/lib/logic/keymap";

const base: KeyContext = {
  surface: "grid",
  searchOpen: false,
  inputFocused: false,
  searchInputFocused: false,
  hasSelection: false,
  railOpen: false,
  debugEnabled: false,
  asrReady: false,
};

const key = (key: string, mods: Partial<KeyInput> = {}): KeyInput => ({
  key,
  ctrlOrMeta: false,
  shift: false,
  ...mods,
});

describe("global rows", () => {
  it("Escape always dispatches escape (never quits)", () => {
    expect(dispatch(key("Escape"), base)).toEqual({ kind: "escape" });
    expect(dispatch(key("Escape"), { ...base, inputFocused: true })).toEqual({
      kind: "escape",
    });
  });

  it("/ and Cmd/Ctrl+F enter Search from anywhere", () => {
    expect(dispatch(key("/"), base)).toEqual({ kind: "open-search" });
    expect(dispatch(key("f", { ctrlOrMeta: true }), base)).toEqual({
      kind: "open-search",
    });
    expect(dispatch(key("f", { ctrlOrMeta: true }), { ...base, surface: "look" })).toEqual(
      { kind: "open-search" },
    );
  });

  it("N summons the typed-note input", () => {
    expect(dispatch(key("n"), base)).toEqual({ kind: "summon-note" });
  });

  it("Cmd/Ctrl+, opens Settings; Cmd/Ctrl+Q quits", () => {
    expect(dispatch(key(",", { ctrlOrMeta: true }), base)).toEqual({
      kind: "open-settings",
    });
    expect(dispatch(key("q", { ctrlOrMeta: true }), base)).toEqual({ kind: "quit" });
  });

  it("F12 toggles the debug panel in dev builds ONLY", () => {
    expect(dispatch(key("F12"), base)).toBeNull();
    expect(dispatch(key("F12"), { ...base, debugEnabled: true })).toEqual({
      kind: "toggle-debug-panel",
    });
  });

  it("M is a RESERVED row — dispatches to nothing in P4.2 (M2b packet)", () => {
    expect(dispatch(key("m"), base)).toBeNull();
    expect(dispatch(key("m"), { ...base, asrReady: true })).toBeNull();
  });
});

describe("suppression while a text input is focused (§11)", () => {
  const typing = { ...base, inputFocused: true };
  it.each(["n", "s", "/", "3", " ", "-", "=", "g", "?", "a"])(
    "single key %j is suppressed",
    (k) => {
      expect(dispatch(key(k), typing)).toBeNull();
    },
  );
  it("modifier chords still work", () => {
    expect(dispatch(key("q", { ctrlOrMeta: true }), typing)).toEqual({ kind: "quit" });
  });
  it("Tab is suppressed while typing (no surprise lights-out)", () => {
    expect(dispatch(key("Tab"), typing)).toBeNull();
  });
});

describe("rating keys 0–5 (CAPTURE §10, C6)", () => {
  it("grid with a selection rates; without, does nothing (session scope)", () => {
    expect(dispatch(key("3"), { ...base, hasSelection: true })).toEqual({
      kind: "rate",
      value: 3,
    });
    expect(dispatch(key("3"), base)).toBeNull();
  });

  it("0 dispatches an explicit clear (value 0, distinct from no-op)", () => {
    expect(dispatch(key("0"), { ...base, hasSelection: true })).toEqual({
      kind: "rate",
      value: 0,
    });
  });

  it("Look always rates the viewed image", () => {
    expect(dispatch(key("5"), { ...base, surface: "look" })).toEqual({
      kind: "rate",
      value: 5,
    });
  });

  it("6–9 are not rating keys", () => {
    expect(dispatch(key("6"), { ...base, hasSelection: true })).toBeNull();
  });
});

describe("grid rows", () => {
  it("Enter opens the focused image in Look", () => {
    expect(dispatch(key("Enter"), base)).toEqual({ kind: "open-look" });
  });

  // AMENDED (1) — D5: Tab = lights-out everywhere; rail-only toggle moves
  // to `\` (featureset §0 supersedes the P3.2 Tab=rail binding).
  it("Tab toggles lights-out; \\ toggles the rail (D5)", () => {
    expect(dispatch(key("Tab"), base)).toEqual({ kind: "toggle-lights-out" });
    expect(dispatch(key("Tab"), { ...base, surface: "look" })).toEqual({
      kind: "toggle-lights-out",
    });
    expect(dispatch(key("\\"), base)).toEqual({ kind: "toggle-rail" });
  });

  // AMENDED (3) — rail arrows/Enter gate on railFocused, not railOpen: the
  // rail is push-PERSISTENT now (DECISIONS entry 3), so openness no longer
  // implies keyboard focus.
  it("arrows/Enter route to the rail only while it has FOCUS", () => {
    const railFocused = { ...base, railOpen: true, railFocused: true };
    expect(dispatch(key("ArrowDown"), railFocused)).toEqual({
      kind: "rail-nav",
      dir: "down",
    });
    expect(dispatch(key("Enter"), railFocused)).toEqual({ kind: "rail-enter" });
    // Open but unfocused: keys stay with the grid.
    const railOpenOnly = { ...base, railOpen: true, railFocused: false };
    expect(dispatch(key("ArrowDown"), railOpenOnly)).toEqual({
      kind: "focus-move",
      dir: "down",
      extend: false,
    });
    expect(dispatch(key("Enter"), railOpenOnly)).toEqual({ kind: "open-look" });
  });

  it("arrows move focus; Shift+arrows extend", () => {
    expect(dispatch(key("ArrowRight"), base)).toEqual({
      kind: "focus-move",
      dir: "right",
      extend: false,
    });
    expect(dispatch(key("ArrowDown", { shift: true }), base)).toEqual({
      kind: "focus-move",
      dir: "down",
      extend: true,
    });
  });

  // AMENDED (2) — featureset §0 symmetric open/close: Space opens Look
  // (supersedes UI.md §3.4 Space-toggle); keyboard selection-toggle moves
  // to Ctrl+Space (not a hot-path per-image verb, so the no-chorded-verbs
  // guardrail is not violated — DECISIONS entry 1).
  it("Space opens Look; Ctrl+Space toggles selection on the active item", () => {
    expect(dispatch(key(" "), base)).toEqual({ kind: "open-look" });
    expect(dispatch(key(" ", { ctrlOrMeta: true }), base)).toEqual({
      kind: "toggle-select-focused",
    });
  });

  it("Cmd/Ctrl+A selects all in folder", () => {
    expect(dispatch(key("a", { ctrlOrMeta: true }), base)).toEqual({
      kind: "select-all",
    });
  });

  it("S opens the sort menu; -/= step thumbnail size", () => {
    expect(dispatch(key("s"), base)).toEqual({ kind: "open-sort-menu" });
    expect(dispatch(key("-"), base)).toEqual({ kind: "thumb-size", delta: -1 });
    expect(dispatch(key("="), base)).toEqual({ kind: "thumb-size", delta: 1 });
  });
});

describe("new P4.2 contract rows (featureset §0/§4/§6/§7)", () => {
  it("G goes home (Grid) from anywhere", () => {
    expect(dispatch(key("g"), base)).toEqual({ kind: "go-grid" });
    expect(dispatch(key("g"), { ...base, surface: "look" })).toEqual({
      kind: "go-grid",
    });
  });

  it("? and F1 toggle the keyboard map", () => {
    expect(dispatch(key("?", { shift: true }), base)).toEqual({
      kind: "toggle-cheatsheet",
    });
    expect(dispatch(key("F1"), base)).toEqual({ kind: "toggle-cheatsheet" });
  });

  it("A toggles auto-advance; F11 fullscreen", () => {
    expect(dispatch(key("a"), base)).toEqual({ kind: "toggle-auto-advance" });
    expect(dispatch(key("F11"), base)).toEqual({ kind: "toggle-fullscreen" });
  });

  it("Ctrl+Shift+A selects none (with a selection to clear)", () => {
    expect(
      dispatch(key("a", { ctrlOrMeta: true, shift: true }), {
        ...base,
        hasSelection: true,
      }),
    ).toEqual({ kind: "select-none" });
  });

  it("while a context menu is open, only Escape dispatches (menu owns keys)", () => {
    const menuOpen = { ...base, contextMenuOpen: true };
    expect(dispatch(key("ArrowDown"), menuOpen)).toBeNull();
    expect(dispatch(key("Enter"), menuOpen)).toBeNull();
    expect(dispatch(key("Escape"), menuOpen)).toEqual({ kind: "escape" });
  });
});

describe("look rows", () => {
  const look = { ...base, surface: "look" as const };
  it("←/→ are prev/next", () => {
    expect(dispatch(key("ArrowLeft"), look)).toEqual({ kind: "look-nav", delta: -1 });
    expect(dispatch(key("ArrowRight"), look)).toEqual({ kind: "look-nav", delta: 1 });
  });
  it("Z toggles fit/100%; +/- step; Cmd/Ctrl+0 fits; Cmd/Ctrl+1 is 100%", () => {
    expect(dispatch(key("z"), look)).toEqual({ kind: "zoom-toggle" });
    expect(dispatch(key("+"), look)).toEqual({ kind: "zoom-step", delta: 1 });
    expect(dispatch(key("-"), look)).toEqual({ kind: "zoom-step", delta: -1 });
    expect(dispatch(key("0", { ctrlOrMeta: true }), look)).toEqual({ kind: "zoom-fit" });
    expect(dispatch(key("1", { ctrlOrMeta: true }), look)).toEqual({ kind: "zoom-100" });
  });
  it("F toggles the filmstrip", () => {
    expect(dispatch(key("f"), look)).toEqual({ kind: "toggle-filmstrip" });
  });
  it("Space closes Look at fit; while zoomed it is the pan key, not a verb", () => {
    expect(dispatch(key(" "), { ...look, lookAtFit: true })).toEqual({
      kind: "look-close",
    });
    expect(dispatch(key(" "), { ...look, lookAtFit: false })).toBeNull();
  });
  it("the pencil band is LIVE (P5.1 keymap reconciliation: spec wins — UI §4.4/§11 put the toggle on B, hold-E erases, O overlays; the reserved P and V rows retired)", () => {
    // Amended by P5.1: this block previously asserted the P4.2 reserved
    // rows dispatched to nothing. The packet lights the band up on the
    // SPEC's keys; P and V now intentionally dispatch nothing in Look.
    expect(dispatch(key("b"), look)).toEqual({ kind: "pencil-pen" });
    expect(dispatch(key("p"), look)).toBeNull();
    expect(dispatch(key("v"), look)).toBeNull();
    expect(dispatch(key("o"), look)).toEqual({ kind: "cycle-overlay" });
    // E is the eraser HOLD, eligible only while the pencil is on.
    expect(dispatch(key("e"), look)).toBeNull();
    expect(dispatch(key("e"), { ...look, pencilMode: true })).toEqual({
      kind: "pencil-eraser",
    });
  });
});

describe("search overlay rows", () => {
  const search = { ...base, searchOpen: true, searchInputFocused: true, inputFocused: true };
  it("Enter opens the focused result; arrows move result focus", () => {
    expect(dispatch(key("Enter"), search)).toEqual({ kind: "search-open-result" });
    expect(dispatch(key("ArrowDown"), search)).toEqual({
      kind: "search-nav",
      dir: "down",
    });
  });
  it("←/→ keep moving the text cursor while the input is focused", () => {
    expect(dispatch(key("ArrowLeft"), search)).toBeNull();
    expect(
      dispatch(key("ArrowLeft"), { ...search, searchInputFocused: false, inputFocused: false }),
    ).toEqual({ kind: "search-nav", dir: "left" });
  });
  it("Backspace maps to chip removal only when the query is empty (def-gated)", () => {
    expect(dispatch(key("Backspace"), { ...search, queryEmpty: true })).toEqual({
      kind: "remove-last-chip",
    });
    expect(dispatch(key("Backspace"), { ...search, queryEmpty: false })).toBeNull();
  });
  it("letters keep typing into the input", () => {
    expect(dispatch(key("n"), search)).toBeNull();
    expect(dispatch(key("3"), search)).toBeNull();
  });
});
