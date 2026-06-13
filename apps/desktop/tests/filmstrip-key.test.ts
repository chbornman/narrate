/**
 * F is TOTAL (founder, June 12 2026 — layout-architecture round): the
 * filmstrip is a center-column bottom panel shared by Grid and Look, and
 * its toggle row moved to defs/global.ts (scope "global"). This file
 * carries the new availability; the pre-existing Look-side F expectation
 * in keymap.test.ts is never edited (it keeps passing — a global row
 * matches in Look too).
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

describe("F toggles the filmstrip on BOTH surfaces", () => {
  it("dispatches toggle-filmstrip in Grid (the founder's 'works sometimes' fix)", () => {
    expect(dispatch(key("f"), base)).toEqual({ kind: "toggle-filmstrip" });
  });

  it("dispatches toggle-filmstrip in Look (unchanged)", () => {
    expect(dispatch(key("f"), { ...base, viewMode: "look" })).toEqual({
      kind: "toggle-filmstrip",
    });
  });

  it("still yields to a focused rail on either surface", () => {
    const railFocused = { ...base, railOpen: true, railFocused: true };
    expect(dispatch(key("f"), railFocused)).toBeNull();
    expect(dispatch(key("f"), { ...railFocused, viewMode: "look" })).toBeNull();
  });

  it("is suppressed while a text input is focused (§11)", () => {
    expect(dispatch(key("f"), { ...base, inputFocused: true })).toBeNull();
  });

  it("Ctrl/Cmd+F stays Search — the chord is distinct from bare F", () => {
    expect(dispatch(key("f", { ctrlOrMeta: true }), base)).toEqual({
      kind: "open-search",
    });
  });
});
