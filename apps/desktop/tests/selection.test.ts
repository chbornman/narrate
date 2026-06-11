/**
 * Selection model (UI §3.4 + featureset §1) — selection IS the write
 * scope, and ORDER MATTERS: CAPTURE §3 records multi target order =
 * selection order. P4.2: focus ≡ ACTIVE (the LrC most-selected model),
 * select-none, Home/End + PageUp/PageDown moves, marquee merge.
 */
import { describe, expect, it } from "vitest";
import * as sel from "../src/lib/logic/selection";

const items = ["a", "b", "c", "d", "e", "f"];

describe("click selection", () => {
  it("plain click selects exactly one and anchors", () => {
    const s = sel.click(sel.EMPTY, items, 2);
    expect(s.order).toEqual(["c"]);
    expect(s.focus).toBe(2);
    expect(s.anchor).toBe(2);
  });

  it("ctrl+click toggles membership, preserving selection order", () => {
    let s = sel.click(sel.EMPTY, items, 4); // e
    s = sel.toggle(s, items, 1); // + b
    s = sel.toggle(s, items, 3); // + d
    expect(s.order).toEqual(["e", "b", "d"]); // CHRONOLOGICAL, not grid order
    s = sel.toggle(s, items, 1); // - b
    expect(s.order).toEqual(["e", "d"]);
  });

  it("shift+click range-extends from the anchor, anchor side first", () => {
    let s = sel.click(sel.EMPTY, items, 1);
    s = sel.rangeTo(s, items, 4);
    expect(s.order).toEqual(["b", "c", "d", "e"]);
    expect(s.anchor).toBe(1);
    // Backwards range keeps the anchor first in selection order.
    s = sel.rangeTo(s, items, 0);
    expect(s.order).toEqual(["b", "a"]);
  });
});

describe("keyboard", () => {
  it("toggle on the active item (Ctrl+Space since D5/§0 — same pure op)", () => {
    let s = sel.click(sel.EMPTY, items, 0);
    s = sel.moveFocus(s, items, 3, "right", false);
    expect(s.focus).toBe(1);
    expect(s.order).toEqual(["a"]); // plain arrows move focus ONLY
    s = sel.toggle(s, items, s.focus);
    expect(s.order).toEqual(["a", "b"]);
  });

  it("arrows are grid-aware vertically", () => {
    let s = sel.click(sel.EMPTY, items, 0);
    s = sel.moveFocus(s, items, 3, "down", false); // 3 columns
    expect(s.focus).toBe(3);
    s = sel.moveFocus(s, items, 3, "up", false);
    expect(s.focus).toBe(0);
  });

  it("shift+arrows extend the selection as a range", () => {
    let s = sel.click(sel.EMPTY, items, 2);
    s = sel.moveFocus(s, items, 6, "right", true);
    s = sel.moveFocus(s, items, 6, "right", true);
    expect(s.order).toEqual(["c", "d", "e"]);
    expect(s.focus).toBe(4);
  });

  it("focus clamps at the edges", () => {
    let s = sel.click(sel.EMPTY, items, 5);
    s = sel.moveFocus(s, items, 3, "right", false);
    expect(s.focus).toBe(5);
    s = sel.moveFocus(s, items, 3, "down", false);
    expect(s.focus).toBe(5);
  });
});

describe("select-all / clear / reconcile", () => {
  it("cmd/ctrl+A selects all in folder, in grid order", () => {
    const s = sel.selectAll(sel.EMPTY, items);
    expect(s.order).toEqual(items);
  });

  it("clear empties the selection but keeps focus (Escape step 4)", () => {
    let s = sel.selectAll(sel.click(sel.EMPTY, items, 3), items);
    s = sel.clear(s);
    expect(s.order).toEqual([]);
    expect(s.focus).toBe(3);
  });

  it("reconcile drops vanished hashes and clamps focus after ingest churn", () => {
    let s = sel.selectAll(sel.EMPTY, items);
    s = { ...s, focus: 5 };
    const next = sel.reconcile(s, ["a", "c", "e"]);
    expect(next.order).toEqual(["a", "c", "e"]);
    expect(next.focus).toBe(2);
  });

  it("selectNone (Ctrl+Shift+A) empties the selection, keeps the ACTIVE image", () => {
    let s = sel.selectAll(sel.click(sel.EMPTY, items, 2), items);
    s = sel.selectNone(s);
    expect(s.order).toEqual([]);
    expect(s.focus).toBe(2); // active survives, like Escape's clear
    expect(s.anchor).toBe(-1);
  });
});

describe("edge & page moves (featureset §1)", () => {
  it("Home/End jump the active image to the edges", () => {
    let s = sel.click(sel.EMPTY, items, 3);
    s = sel.moveEdge(s, items, "home", false);
    expect(s.focus).toBe(0);
    expect(s.order).toEqual(["d"]); // plain moves never change the selection
    s = sel.moveEdge(s, items, "end", false);
    expect(s.focus).toBe(5);
  });

  it("Shift+Home range-extends from the anchor", () => {
    let s = sel.click(sel.EMPTY, items, 2);
    s = sel.moveEdge(s, items, "home", true);
    expect(s.order).toEqual(["c", "b", "a"]); // anchor side first
    expect(s.focus).toBe(0);
  });

  it("PageUp/PageDown move by a viewport of rows, clamped", () => {
    let s = sel.click(sel.EMPTY, items, 0);
    s = sel.movePage(s, items, 2, 2, "down", false); // 2 cols × 2 rows = 4
    expect(s.focus).toBe(4);
    s = sel.movePage(s, items, 2, 2, "down", false);
    expect(s.focus).toBe(5); // clamped at the end
    s = sel.movePage(s, items, 2, 2, "up", false);
    expect(s.focus).toBe(1);
  });

  it("Shift+PageDown extends as a range", () => {
    let s = sel.click(sel.EMPTY, items, 1);
    s = sel.movePage(s, items, 3, 1, "down", true);
    expect(s.order).toEqual(["b", "c", "d", "e"]);
  });
});

describe("marquee merge (featureset §1: gutter drag; Ctrl = additive)", () => {
  it("plain marquee replaces the selection with the hits, in grid order", () => {
    const s0 = sel.click(sel.EMPTY, items, 5);
    const s = sel.marqueeMerge(s0, items, [3, 1, 2], false);
    expect(s.order).toEqual(["b", "c", "d"]);
    expect(s.focus).toBe(3); // last hit becomes active
  });

  it("Ctrl-marquee merges ADDITIVELY, preserving existing selection order", () => {
    let s = sel.click(sel.EMPTY, items, 4); // e first (order matters)
    s = sel.marqueeMerge(s, items, [0, 1], true);
    expect(s.order).toEqual(["e", "a", "b"]); // e kept first; hits appended
  });

  it("an additive merge never duplicates already-selected items", () => {
    let s = sel.click(sel.EMPTY, items, 1); // b
    s = sel.marqueeMerge(s, items, [1, 2], true);
    expect(s.order).toEqual(["b", "c"]);
  });

  it("an empty plain marquee clears; an empty additive marquee is a no-op", () => {
    const s0 = sel.selectAll(sel.EMPTY, items);
    expect(sel.marqueeMerge(s0, items, [], false).order).toEqual([]);
    expect(sel.marqueeMerge(s0, items, [], true).order).toEqual(items);
  });
});

describe("multi-select inspector label (founder, June 2026)", () => {
  it("reads 'N selected' from two targets up — the anchor view's honesty caveat", () => {
    expect(sel.multiSelectLabel(2)).toBe("2 selected");
    expect(sel.multiSelectLabel(5)).toBe("5 selected");
  });

  it("is silent for a single image or none (no caveat needed)", () => {
    expect(sel.multiSelectLabel(1)).toBeNull();
    expect(sel.multiSelectLabel(0)).toBeNull();
  });
});
