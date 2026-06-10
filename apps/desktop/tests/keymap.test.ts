/**
 * Keyboard map dispatch (UI §11 — single source of truth). Every M1 row is
 * asserted; later-packet keys must dispatch to NOTHING; single-letter keys
 * are suppressed while a text input is focused.
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

  it("M does nothing in M1 (ASR never ready)", () => {
    expect(dispatch(key("m"), base)).toBeNull();
  });

  it("J (journal panel — later packet) does not exist", () => {
    expect(dispatch(key("j"), base)).toBeNull();
    expect(dispatch(key("j"), { ...base, surface: "look" })).toBeNull();
  });
});

describe("suppression while a text input is focused (§11)", () => {
  const typing = { ...base, inputFocused: true };
  it.each(["n", "s", "/", "3", " ", "-", "="])(
    "single key %j is suppressed",
    (k) => {
      expect(dispatch(key(k), typing)).toBeNull();
    },
  );
  it("modifier chords still work", () => {
    expect(dispatch(key("q", { ctrlOrMeta: true }), typing)).toEqual({ kind: "quit" });
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
  it("Tab toggles the rail; arrows navigate the rail while open", () => {
    expect(dispatch(key("Tab"), base)).toEqual({ kind: "toggle-rail" });
    expect(dispatch(key("ArrowDown"), { ...base, railOpen: true })).toEqual({
      kind: "rail-nav",
      dir: "down",
    });
    expect(dispatch(key("Enter"), { ...base, railOpen: true })).toEqual({
      kind: "rail-enter",
    });
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
  it("Space toggles selection on the focused item", () => {
    expect(dispatch(key(" "), base)).toEqual({ kind: "toggle-select-focused" });
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

describe("look rows", () => {
  const look = { ...base, surface: "look" as const };
  it("←/→ are prev/next", () => {
    expect(dispatch(key("ArrowLeft"), look)).toEqual({ kind: "look-nav", delta: -1 });
    expect(dispatch(key("ArrowRight"), look)).toEqual({ kind: "look-nav", delta: 1 });
  });
  it("Z toggles fit/100%; +/- step; Cmd/Ctrl+0 fits", () => {
    expect(dispatch(key("z"), look)).toEqual({ kind: "zoom-toggle" });
    expect(dispatch(key("+"), look)).toEqual({ kind: "zoom-step", delta: 1 });
    expect(dispatch(key("-"), look)).toEqual({ kind: "zoom-step", delta: -1 });
    expect(dispatch(key("0", { ctrlOrMeta: true }), look)).toEqual({ kind: "zoom-fit" });
  });
  it("F toggles the filmstrip", () => {
    expect(dispatch(key("f"), look)).toEqual({ kind: "toggle-filmstrip" });
  });
  it("B/E/O (pencil — M2a packet) do not exist yet", () => {
    expect(dispatch(key("b"), look)).toBeNull();
    expect(dispatch(key("e"), look)).toBeNull();
    expect(dispatch(key("o"), look)).toBeNull();
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
  it("Backspace maps to chip removal (caller gates on empty input)", () => {
    expect(dispatch(key("Backspace"), search)).toEqual({ kind: "remove-last-chip" });
  });
  it("letters keep typing into the input", () => {
    expect(dispatch(key("n"), search)).toBeNull();
    expect(dispatch(key("3"), search)).toBeNull();
  });
});
