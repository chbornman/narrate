/**
 * Keyboard map dispatch — the interpreter (logic/keymap.ts) over the typed
 * action registry. Every P3.2 row is asserted; the mic is live (P6.4,
 * two-gesture June 2026) and sits on SPACE since June 12 2026 — "like a
 * Zoom call"; M returned to the reserved pool. The pencil band went live
 * in P5.1 (its block below carries the amendment citation); typing keys
 * are suppressed while a text input is focused (UI §11).
 *
 * EXACTLY THREE existing expectations are amended in P4.2, each citing its
 * founder decision in place:
 *   (1) Tab → lights-out, `\` → toggle-rail              (D5)
 *   (2) Space in Grid → open-look (§0 symmetric open/close);
 *       selection-toggle moves to Ctrl+Space             (DECISIONS entry 1)
 *       [re-amended June 12 2026: Space is the mic key; open-look is
 *        Enter-only — see the grid block below]
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
  viewMode: "grid",
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
    expect(dispatch(key("f", { ctrlOrMeta: true }), { ...base, viewMode: "look" })).toEqual(
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

  it("Space begins the two-gesture mic press — but ONLY while the supervised ASR is ready", () => {
    expect(dispatch(key(" "), base)).toBeNull();
    // Keydown dispatches mic-press (tap-vs-hold resolves at the raw
    // keyup — logic/michold.ts); the pointer toggle is the same def
    // resolved with arg "toggle" (Indicator.svelte).
    expect(dispatch(key(" "), { ...base, asrReady: true })).toEqual({ kind: "mic-press" });
  });

  it("M is back in the reserved pool (June 12 2026) — it dispatches NOTHING", () => {
    expect(dispatch(key("m"), base)).toBeNull();
    expect(dispatch(key("m"), { ...base, asrReady: true })).toBeNull();
  });

  it("Space keeps typing spaces — the §11 suppression covers it even with ASR ready", () => {
    // The rule keys on "the chord can type" (match.ts), not on single
    // LETTERS specifically, so " " is suppressed with no special case.
    expect(
      dispatch(key(" "), { ...base, asrReady: true, inputFocused: true }),
    ).toBeNull();
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
    expect(dispatch(key("5"), { ...base, viewMode: "look" })).toEqual({
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
    expect(dispatch(key("Tab"), { ...base, viewMode: "look" })).toEqual({
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

  // RE-AMENDED (2) — June 12 2026: Space is 100% the microphone key, so
  // the P4.2 Space-opens-Look chord is withdrawn; open-look is Enter-only
  // (plus double-click). Ctrl+Space selection-toggle SURVIVES: it is a
  // modifier chord, a different chord shape from the mic's plain Space.
  it("Space no longer opens Look (it is the mic key); Ctrl+Space still toggles selection", () => {
    expect(dispatch(key(" "), base)).toBeNull(); // ASR not ready: inert, never open-look
    expect(dispatch(key(" "), { ...base, asrReady: true })).toEqual({
      kind: "mic-press",
    });
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
    expect(dispatch(key("g"), { ...base, viewMode: "look" })).toEqual({
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
  const look = { ...base, viewMode: "look" as const };
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
  it("Space is the mic key in Look too (June 12 2026) — close is Esc's job alone", () => {
    // The look-close row is GONE: Space dispatches the mic when ASR is
    // ready and nothing otherwise; Esc closes via the escape ladder.
    expect(dispatch(key(" "), look)).toBeNull();
    expect(dispatch(key(" "), { ...look, asrReady: true })).toEqual({
      kind: "mic-press",
    });
    expect(dispatch(key("Escape"), look)).toEqual({ kind: "escape" });
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

describe("search bar (M3 search-as-scope): no search-scope keymap rows", () => {
  // The overlay's search SCOPE is retired — the query is a grid scope now,
  // and the always-visible header bar is a focused text input that handles
  // its own keys locally (Enter commits the semantic lane, Backspace drops a
  // chip). So no chord dispatches a search action; Enter/arrows/Backspace
  // either fall through to the grid (when not in an input) or to the input.
  const barFocused = { ...base, searchInputFocused: true, inputFocused: true };
  it("Enter/arrows/Backspace no longer dispatch search actions from the keymap", () => {
    expect(dispatch(key("Enter"), barFocused)).toBeNull();
    expect(dispatch(key("ArrowDown"), barFocused)).toBeNull();
    expect(dispatch(key("Backspace"), { ...barFocused, queryEmpty: true })).toBeNull();
  });
  it("`/` and Cmd+F resolve to open-search (which focuses the bar)", () => {
    // `/` types while an input is focused (§11 suppression); from the grid
    // it focuses the bar. Cmd+F works even in an input (worksInInput).
    expect(dispatch(key("/"), base)).toEqual({ kind: "open-search" });
    expect(dispatch({ key: "f", ctrlOrMeta: true, shift: false }, barFocused)).toEqual({
      kind: "open-search",
    });
  });
  it("letters keep typing into the input (§11)", () => {
    expect(dispatch(key("n"), barFocused)).toBeNull();
    expect(dispatch(key("3"), barFocused)).toBeNull();
  });
});
