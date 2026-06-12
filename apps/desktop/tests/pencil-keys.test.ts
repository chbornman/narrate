/**
 * The grease-pencil registry band (P5.1 — actions/defs/look.ts) through
 * the keymap interpreter. Keymap reconciliation: spec/UI.md §4.4/§11 wins
 * over the P4.2 reserved P/E/V seats — B = sticky toggle, hold-E = eraser,
 * O = overlay, Ctrl+Z = undo. Gating lives on the defs (never in
 * components): B stays live with the overlay hidden (show-and-arm — a
 * bound key must never be dead), the eraser needs pencil mode, undo needs
 * pencil work, and the §11 input suppression holds for every row.
 */
import { describe, expect, it } from "vitest";
import {
  dispatch,
  withDefaults,
  type KeyContext,
  type KeyInput,
} from "../src/lib/logic/keymap";
import { menuModel } from "../src/lib/actions/menus";

const look: KeyContext = {
  surface: "look",
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

describe("B — sticky pencil toggle (UI §4.4: a held key cramps the hand)", () => {
  it("dispatches in Look, never in Grid", () => {
    expect(dispatch(key("b"), look)).toEqual({ kind: "pencil-pen" });
    expect(dispatch(key("b"), { ...look, surface: "grid" })).toBeNull();
  });

  it("stays LIVE while the overlay is hidden (a bound key must never be dead)", () => {
    // The slice's togglePencil shows the paper AND arms the pencil; the
    // keymap's only job is to keep dispatching.
    expect(dispatch(key("b"), { ...look, overlayVisible: false })).toEqual({
      kind: "pencil-pen",
    });
  });

  it("is suppressed while a text input is focused (§11)", () => {
    expect(dispatch(key("b"), { ...look, inputFocused: true })).toBeNull();
  });
});

describe("E — eraser hold (release is the overlay's raw keyup)", () => {
  it("engages only while pencil mode is on", () => {
    expect(dispatch(key("e"), { ...look, pencilMode: true })).toEqual({
      kind: "pencil-eraser",
    });
    expect(dispatch(key("e"), look)).toBeNull();
  });

  it("yields to the rail and to text inputs", () => {
    expect(
      dispatch(key("e"), { ...look, pencilMode: true, railOpen: true, railFocused: true }),
    ).toBeNull();
    expect(dispatch(key("e"), { ...look, pencilMode: true, inputFocused: true })).toBeNull();
  });
});

describe("O — tracing-paper overlay toggle", () => {
  it("dispatches in Look regardless of pencil mode", () => {
    expect(dispatch(key("o"), look)).toEqual({ kind: "cycle-overlay" });
    expect(dispatch(key("o"), { ...look, pencilMode: true })).toEqual({
      kind: "cycle-overlay",
    });
  });
});

describe("Ctrl+Z — pencil undo (CAPTURE §8.5)", () => {
  it("dispatches when there is pencil work (pen down or a stacked stroke)", () => {
    expect(dispatch(key("z", { ctrlOrMeta: true }), { ...look, pencilUndoable: true })).toEqual(
      { kind: "pencil-undo" },
    );
  });

  it("with NOTHING to undo the pencil layer does not swallow the chord", () => {
    expect(dispatch(key("z", { ctrlOrMeta: true }), look)).toBeNull();
  });

  it("stays out of text inputs (the textarea keeps native undo)", () => {
    expect(
      dispatch(key("z", { ctrlOrMeta: true }), {
        ...look,
        pencilUndoable: true,
        inputFocused: true,
      }),
    ).toBeNull();
  });

  it("bare Z stays the zoom toggle — no collision with the chord", () => {
    expect(dispatch(key("z"), { ...look, pencilUndoable: true })).toEqual({
      kind: "zoom-toggle",
    });
  });
});

describe("Space with the pencil on is the PAN key (UI §11), never look-close", () => {
  it("at fit with pencil on, Space does not close Look", () => {
    expect(dispatch(key(" "), { ...look, lookAtFit: true, pencilMode: true })).toBeNull();
  });

  it("at fit with pencil off, Space still closes (the §0 symmetry row)", () => {
    expect(dispatch(key(" "), { ...look, lookAtFit: true })).toEqual({
      kind: "look-close",
    });
  });
});

describe("look-backdrop seating (featureset §6: menus mirror every verb)", () => {
  // U14's keyboard-only exemption for pencil-undo is gone (polish round):
  // the chord's verb holds a seat like every other row, grayed through
  // the SAME enabled gate the keymap uses.
  function undoRow(over: Partial<KeyContext> = {}) {
    const rows = menuModel("look-backdrop", withDefaults({ ...look, ...over })).rows;
    return rows.find((r) => r.verb === "Undo stroke");
  }

  it("carries an Undo stroke row, grayed until pencil work exists", () => {
    const dimmed = undoRow();
    expect(dimmed).toBeDefined();
    expect(dimmed?.disabled).toBe(true);
    const live = undoRow({ pencilUndoable: true });
    expect(live?.disabled).toBe(false);
    expect(live?.action).toEqual({ kind: "pencil-undo" });
    expect(live?.keyHint).toEqual({ key: "z", ctrlOrMeta: true });
  });
});

describe("retired rows (keymap reconciliation)", () => {
  it("P and V dispatch nothing in Look", () => {
    expect(dispatch(key("p"), { ...look, pencilMode: true })).toBeNull();
    expect(dispatch(key("v"), { ...look, pencilMode: true })).toBeNull();
  });
});
