/**
 * Stage A grid rows over the one registry (defs/grid.ts) — dispatched
 * through the keymap interpreter, never asserted by editing existing
 * keymap.test.ts blocks. Covers: T cell-info, Home/End and PgUp/PgDn
 * edge/page moves (Shift extends), the R display flip (collapsed pairs
 * only), §11 input suppression, rail-focus gating, and the pointer-seated
 * stack verbs on their menu seats.
 */
import { describe, expect, it } from "vitest";
import { dispatch, withDefaults, type KeyContext, type KeyInput } from "../src/lib/logic/keymap";
import { menuModel, type MenuRow } from "../src/lib/actions/menus";

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

describe("T cycles cell info (featureset §1)", () => {
  it("dispatches in Grid; never in Look; suppressed while typing", () => {
    expect(dispatch(key("t"), base)).toEqual({ kind: "cycle-cell-info" });
    expect(dispatch(key("t"), { ...base, surface: "look" })).toBeNull();
    expect(dispatch(key("t"), { ...base, inputFocused: true })).toBeNull();
  });
});

describe("Home/End and PageUp/PageDown (featureset §1)", () => {
  it("jump the active image to an edge; Shift range-extends", () => {
    expect(dispatch(key("Home"), base)).toEqual({
      kind: "focus-edge",
      edge: "home",
      extend: false,
    });
    expect(dispatch(key("End"), base)).toEqual({
      kind: "focus-edge",
      edge: "end",
      extend: false,
    });
    expect(dispatch(key("End", { shift: true }), base)).toEqual({
      kind: "focus-edge",
      edge: "end",
      extend: true,
    });
  });

  it("page by one viewport of rows; Shift range-extends", () => {
    expect(dispatch(key("PageDown"), base)).toEqual({
      kind: "focus-page",
      dir: "down",
      extend: false,
    });
    expect(dispatch(key("PageUp", { shift: true }), base)).toEqual({
      kind: "focus-page",
      dir: "up",
      extend: true,
    });
  });

  it("stay with the grid surface (nothing in Look; rail focus gates)", () => {
    expect(dispatch(key("Home"), { ...base, surface: "look" })).toBeNull();
    expect(dispatch(key("PageDown"), { ...base, railOpen: true, railFocused: true })).toBeNull();
  });
});

describe("R flips the displayed stack member in Grid (featureset §5)", () => {
  it("fires only on an active COLLAPSED pair", () => {
    expect(dispatch(key("r"), base)).toBeNull(); // no pair active
    expect(
      dispatch(key("r"), { ...base, activeIsPair: true, activePairCollapsed: false }),
    ).toBeNull(); // expanded members show themselves already
    expect(
      dispatch(key("r"), { ...base, activeIsPair: true, activePairCollapsed: true }),
    ).toEqual({ kind: "flip-stack-member" });
  });

  it("is suppressed while typing (single letter, §11)", () => {
    expect(
      dispatch(key("r"), {
        ...base,
        inputFocused: true,
        activeIsPair: true,
        activePairCollapsed: true,
      }),
    ).toBeNull();
  });
});

describe("stack verbs are pointer-seated (featureset §6 menus)", () => {
  const verbs = (rows: MenuRow[]): string[] =>
    rows.flatMap((r) => [
      ...(r.verb !== undefined ? [r.verb] : []),
      ...(r.children !== undefined ? verbs(r.children) : []),
    ]);

  it("the thumb seat carries Collapse / expand stack, enabled only for pairs", () => {
    const onPair = withDefaults({ ...base, hasSelection: true, activeIsPair: true });
    const model = menuModel("thumb", onPair);
    const row = model.rows.find((r) => r.verb === "Collapse / expand stack");
    expect(row).toBeDefined();
    expect(row?.disabled).toBeFalsy();
    expect(row?.action).toEqual({ kind: "stack-toggle-active" });

    const noPair = withDefaults({ ...base, hasSelection: true });
    const grayed = menuModel("thumb", noPair).rows.find(
      (r) => r.verb === "Collapse / expand stack",
    );
    expect(grayed?.disabled).toBe(true); // grayed, never hidden
  });

  it("the gutter's Stacks ▸ submenu carries collapse/expand all", () => {
    const model = menuModel("gutter", withDefaults(base));
    const stacksRow = model.rows.find((r) => r.kind === "submenu" && r.verb === "Stacks");
    expect(stacksRow).toBeDefined();
    expect(verbs(stacksRow?.children ?? [])).toEqual(["Collapse all", "Expand all"]);
  });
});
