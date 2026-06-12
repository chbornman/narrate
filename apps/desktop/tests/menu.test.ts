/**
 * Menu primitives: the pure keyboard-nav controller (primitives/menu.ts)
 * and the seat → MenuModel builder (actions/menus.ts) — rows carry
 * Actions, never key strings or handlers; parametrized defs expand to
 * radio submenus; "menus mirror every keyboard verb" is structural.
 */
import { describe, expect, it } from "vitest";
import { navInit, navKey, rowsAt, type NavRow } from "../src/lib/primitives/menu";
import { menuModel } from "../src/lib/actions/menus";
import { withDefaults } from "../src/lib/logic/keymap";

const rows: NavRow[] = [
  { kind: "item", verb: "Open" },
  { kind: "separator" },
  { kind: "item", verb: "Rate", disabled: true },
  { kind: "submenu", verb: "Sort", children: [
    { kind: "radio", verb: "Newest first" },
    { kind: "radio", verb: "Filename" },
  ] },
  { kind: "item", verb: "Quit" },
];

describe("menu.ts — pure keyboard navigation", () => {
  it("init focuses the first selectable row", () => {
    expect(navInit(rows).focus).toBe(0);
  });

  it("↑↓ skip separators and disabled rows, wrapping", () => {
    let nav = navInit(rows);
    nav = navKey(rows, nav, "ArrowDown").nav;
    expect(nav.focus).toBe(3); // skipped separator + disabled Rate
    nav = navKey(rows, nav, "ArrowDown").nav;
    expect(nav.focus).toBe(4);
    nav = navKey(rows, nav, "ArrowDown").nav;
    expect(nav.focus).toBe(0); // wrap
    nav = navKey(rows, nav, "ArrowUp").nav;
    expect(nav.focus).toBe(4); // wrap backwards
  });

  it("→ opens a submenu, ← closes back to the parent row", () => {
    let nav = navInit(rows);
    nav = { ...nav, focus: 3 };
    nav = navKey(rows, nav, "ArrowRight").nav;
    expect(nav.path).toEqual([3]);
    expect(rowsAt(rows, nav.path)[nav.focus].verb).toBe("Newest first");
    nav = navKey(rows, nav, "ArrowLeft").nav;
    expect(nav.path).toEqual([]);
    expect(nav.focus).toBe(3);
  });

  it("Enter activates an item; on a submenu row it opens the submenu", () => {
    let nav = navInit(rows);
    const r1 = navKey(rows, nav, "Enter");
    expect(r1.activate?.verb).toBe("Open");
    nav = { ...nav, focus: 3 };
    const r2 = navKey(rows, nav, "Enter");
    expect(r2.activate).toBeUndefined();
    expect(r2.nav.path).toEqual([3]);
  });

  it("typeahead focuses by verb prefix and buffers within the window", () => {
    const nav = navInit(rows);
    let r = navKey(rows, nav, "q", 100);
    expect(r.nav.focus).toBe(4); // Quit
    // A fresh prefix after the 600 ms window: "s" then "o" buffer → Sort.
    r = navKey(rows, r.nav, "s", 1000);
    expect(r.nav.focus).toBe(3);
    r = navKey(rows, r.nav, "o", 1100);
    expect(r.nav.focus).toBe(3); // "so" still matches Sort, not Open
    // After the window the buffer resets: bare "o" → Open.
    r = navKey(rows, r.nav, "o", 2000);
    expect(r.nav.focus).toBe(0);
  });

  it("never handles Escape (the escape layer owns closing)", () => {
    const nav = navInit(rows);
    const r = navKey(rows, nav, "Escape");
    expect(r.nav).toEqual(nav);
    expect(r.activate).toBeUndefined();
  });
});

describe("menus.ts — seat models over the registry", () => {
  const ctx = withDefaults({
    surface: "grid",
    searchOpen: false,
    inputFocused: false,
    searchInputFocused: false,
    hasSelection: true,
    selectionCount: 2,
    activeHash: "ab".repeat(32),
    railOpen: true,
    debugEnabled: false,
    asrReady: false,
  });

  it("rows carry Actions and KeyChords — never key strings or handlers", () => {
    const model = menuModel("gutter", ctx);
    expect(model.rows.length).toBeGreaterThan(0);
    const walk = (rs: typeof model.rows) => {
      for (const r of rs) {
        if (r.kind === "item" || r.kind === "radio") {
          expect(r.action).toBeDefined();
          expect(typeof r.action).toBe("object"); // an Action, not a callback
        }
        if (r.children !== undefined) walk(r.children);
      }
    };
    walk(model.rows);
  });

  it("parametrized defs render as radio submenus with the current value checked", () => {
    const model = menuModel("gutter", ctx);
    const sort = model.rows.find((r) => r.kind === "submenu" && r.verb === "Sort");
    expect(sort?.children?.every((c) => c.kind === "radio")).toBe(true);
    const checked = sort?.children?.filter((c) => c.checked === true);
    expect(checked?.length).toBe(1);
    expect(checked?.[0].action).toEqual({ kind: "set-sort", mode: "capture-desc" });
    const surround = model.rows.find((r) => r.kind === "submenu" && r.verb === "Surround");
    expect(surround?.children?.length).toBe(5); // the D6 enumeration, closed
  });

  it("the rail-folder seat threads its argument into the rows' Actions", () => {
    const model = menuModel("rail-folder", ctx, { rootId: "r1", folder: "2026" });
    expect(model.rows.map((r) => r.verb)).toEqual([
      "Open",
      "Show in file manager",
      "Rescan",
      "Rebuild previews…",
      undefined, // separator before the rail's own verb
      "Add folder…",
    ]);
    expect(model.rows[0].action).toEqual({
      kind: "rail-folder-open",
      rootId: "r1",
      folder: "2026",
    });
    expect(model.rows[2].action).toEqual({ kind: "rescan-root", rootId: "r1" });
    expect(model.rows[3].action).toEqual({
      kind: "rebuild-previews",
      rootId: "r1",
    });
    expect(model.rows[5].action).toEqual({ kind: "add-root" });
  });

  it("Add to collection renders as a submenu of collection names threading ids", () => {
    const withCollections = {
      ...ctx,
      collections: [
        { id: "01A", name: "Quiet Hours" },
        { id: "01B", name: "Fog Series" },
      ],
    };
    const model = menuModel("thumb", withCollections);
    const sub = model.rows.find(
      (r) => r.kind === "submenu" && r.verb === "Add to collection",
    );
    expect(sub?.children?.map((c) => c.verb)).toEqual(["Quiet Hours", "Fog Series"]);
    expect(sub?.children?.[1].action).toEqual({
      kind: "add-to-collection",
      id: "01B",
    });
    // No collections → the verb does not exist (availability gate): the
    // rail's Collections tab is where creation lives, not a dead submenu.
    expect(
      menuModel("thumb", ctx).rows.some((r) => r.verb === "Add to collection"),
    ).toBe(false);
  });

  it("membership marks: Add checkmarks current memberships; Remove lists only them", () => {
    const withMembership = {
      ...ctx,
      collections: [
        { id: "01A", name: "Quiet Hours" },
        { id: "01B", name: "Fog Series" },
      ],
      activeMemberships: ["01B"],
    };
    const model = menuModel("thumb", withMembership);
    const add = model.rows.find(
      (r) => r.kind === "submenu" && r.verb === "Add to collection",
    );
    // The active image's truth: Fog Series is checked, Quiet Hours not.
    expect(add?.children?.map((c) => c.checked)).toEqual([false, true]);
    const remove = model.rows.find(
      (r) => r.kind === "submenu" && r.verb === "Remove from collection",
    );
    expect(remove?.children?.map((c) => c.verb)).toEqual(["Fog Series"]);
    expect(remove?.children?.[0].action).toEqual({
      kind: "remove-from-collection",
      id: "01B",
    });
    // Not a member of anything → no Remove submenu (availability gate):
    // membership is a one-way door no longer, but never a dead verb.
    const noMembership = { ...withMembership, activeMemberships: [] };
    expect(
      menuModel("thumb", noMembership).rows.some(
        (r) => r.verb === "Remove from collection",
      ),
    ).toBe(false);
  });

  it("the sort ▾ pseudo-seat is the same machinery, flat", () => {
    const model = menuModel("sort", ctx);
    expect(model.rows.length).toBe(4); // the complete v1 sort set
    expect(model.rows.every((r) => r.kind === "radio")).toBe(true);
  });

  it("unavailable defs are absent; disabled defs render grayed", () => {
    const noSel = { ...ctx, hasSelection: false };
    const model = menuModel("gutter", noSel);
    const selectNone = model.rows.find((r) => r.verb === "Select none");
    expect(selectNone?.disabled).toBe(true); // enabled() gating → grayed
  });
});
