/**
 * Inspector rows (actions/defs/inspector.ts) through the real dispatch —
 * featureset §3/D2: `I` opens the Metadata tab, `J` opens the Journal tab
 * from Grid AND Look (the right edge is per-image truth everywhere); the
 * open tab's own key closes (toggle symmetry; Esc closes first via the
 * escape order, tested in escape.test.ts layer 8). The P3.2 "J does not
 * exist" guard retires HERE, per the keymap test charter.
 */
import { describe, expect, it } from "vitest";
import { dispatch, type KeyContext, type KeyInput } from "../src/lib/logic/keymap";
import { menuModel } from "../src/lib/actions/menus";
import { withDefaults } from "../src/lib/logic/keymap";

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

describe("I / J open their tabs (featureset §3, D2)", () => {
  it("I → Metadata, J → Journal, in Grid", () => {
    expect(dispatch(key("i"), base)).toEqual({
      kind: "open-inspector",
      tab: "metadata",
    });
    expect(dispatch(key("j"), base)).toEqual({
      kind: "open-inspector",
      tab: "journal",
    });
  });

  it("…and in Look (same keys everywhere, §0)", () => {
    const look = { ...base, surface: "look" as const };
    expect(dispatch(key("i"), look)).toEqual({
      kind: "open-inspector",
      tab: "metadata",
    });
    expect(dispatch(key("j"), look)).toEqual({
      kind: "open-inspector",
      tab: "journal",
    });
  });

  it("switching tabs while open dispatches the other tab", () => {
    expect(dispatch(key("j"), { ...base, inspectorOpen: "metadata" })).toEqual({
      kind: "open-inspector",
      tab: "journal",
    });
    expect(dispatch(key("i"), { ...base, inspectorOpen: "journal" })).toEqual({
      kind: "open-inspector",
      tab: "metadata",
    });
  });

  it("the OPEN tab's own key closes the inspector (toggle symmetry)", () => {
    expect(dispatch(key("i"), { ...base, inspectorOpen: "metadata" })).toEqual({
      kind: "close-inspector",
    });
    expect(dispatch(key("j"), { ...base, inspectorOpen: "journal" })).toEqual({
      kind: "close-inspector",
    });
  });

  it("single letters are suppressed while a text input is focused (UI §11)", () => {
    expect(dispatch(key("i"), { ...base, inputFocused: true })).toBeNull();
    expect(dispatch(key("j"), { ...base, inputFocused: true })).toBeNull();
  });
});

describe("thumb-menu seating (featureset §6: menus mirror every verb)", () => {
  it("the thumb menu carries the Inspector submenu with Metadata/Journal radios", () => {
    const ctx = withDefaults({ ...base, hasSelection: true, activeHash: "ab".repeat(32) });
    const model = menuModel("thumb", ctx);
    const inspector = model.rows.find((r) => r.verb === "Inspector");
    expect(inspector?.kind).toBe("submenu");
    expect(inspector?.children?.map((c) => c.verb)).toEqual(["Metadata", "Journal"]);
    expect(inspector?.children?.[0].action).toEqual({
      kind: "open-inspector",
      tab: "metadata",
    });
  });

  it("the open tab renders checked (radio state)", () => {
    const ctx = withDefaults({ ...base, inspectorOpen: "journal" });
    const model = menuModel("thumb", ctx);
    const inspector = model.rows.find((r) => r.verb === "Inspector");
    expect(inspector?.children?.map((c) => c.checked === true)).toEqual([false, true]);
  });
});
