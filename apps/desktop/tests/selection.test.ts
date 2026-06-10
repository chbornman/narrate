/**
 * Selection model (UI §3.4) — selection IS the write scope, and ORDER
 * MATTERS: CAPTURE §3 records multi target order = selection order.
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
  it("space toggles the focused item", () => {
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
});
