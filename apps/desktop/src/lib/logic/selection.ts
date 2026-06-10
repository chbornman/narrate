/**
 * Grid selection model (spec/UI.md §3.4) as pure functions over immutable
 * state. The selection IS the write scope; ORDER MATTERS — CAPTURE §3:
 * multi target order = selection order, recorded as event_targets.position.
 *
 * Click selects one · Shift+Click range-extends · Cmd/Ctrl+Click toggles ·
 * Cmd/Ctrl+A selects all · Escape clears · arrows move focus · Shift+arrows
 * extend · Space toggles the focused item.
 */

export interface SelState {
  /** Selected hashes in selection order. */
  order: string[];
  /** Focused index into the current item list, -1 = none. */
  focus: number;
  /** Range anchor index for shift-selection, -1 = none. */
  anchor: number;
}

export const EMPTY: SelState = { order: [], focus: -1, anchor: -1 };

export function isSelected(s: SelState, hash: string): boolean {
  return s.order.includes(hash);
}

/** Plain click: select exactly this item; focus + anchor follow. */
export function click(_s: SelState, items: string[], i: number): SelState {
  if (i < 0 || i >= items.length) return EMPTY;
  return { order: [items[i]], focus: i, anchor: i };
}

/**
 * Shift+Click / Shift+arrow range: selection = the visual range between the
 * anchor and the target, in grid order (anchor side first).
 */
export function rangeTo(s: SelState, items: string[], i: number): SelState {
  if (i < 0 || i >= items.length) return s;
  const anchor = s.anchor >= 0 ? s.anchor : i;
  const [lo, hi] = anchor <= i ? [anchor, i] : [i, anchor];
  let range = items.slice(lo, hi + 1);
  if (anchor > i) range = range.reverse(); // anchor side first
  return { order: range, focus: i, anchor };
}

/** Cmd/Ctrl+Click and Space: toggle one item; selection order preserved. */
export function toggle(s: SelState, items: string[], i: number): SelState {
  if (i < 0 || i >= items.length) return s;
  const hash = items[i];
  const order = s.order.includes(hash)
    ? s.order.filter((h) => h !== hash)
    : [...s.order, hash];
  return { order, focus: i, anchor: i };
}

export function selectAll(s: SelState, items: string[]): SelState {
  return { order: [...items], focus: s.focus, anchor: s.anchor };
}

/** Escape in Grid with a selection (UI §2.2 step 4). Focus survives. */
export function clear(s: SelState): SelState {
  return { order: [], focus: s.focus, anchor: -1 };
}

export type FocusDir = "left" | "right" | "up" | "down";

/**
 * Arrow navigation. `cols` is the current column count (vertical moves are
 * grid-aware). Plain arrows move focus only; with `extend` the selection
 * becomes the anchor→focus range (Shift+arrows).
 */
export function moveFocus(
  s: SelState,
  items: string[],
  cols: number,
  dir: FocusDir,
  extend: boolean,
): SelState {
  if (items.length === 0) return s;
  const delta =
    dir === "left" ? -1 : dir === "right" ? 1 : dir === "up" ? -cols : cols;
  const from = s.focus < 0 ? 0 : s.focus;
  let next = s.focus < 0 ? 0 : from + delta;
  if (next < 0 || next >= items.length) {
    next = Math.max(0, Math.min(items.length - 1, next));
    if (next === from && s.focus >= 0 && !extend) return s;
  }
  if (extend) {
    const anchored = s.anchor >= 0 ? s : { ...s, anchor: from };
    return rangeTo(anchored, items, next);
  }
  return { ...s, focus: next };
}

/** Prune selection/focus after the item list changes (ingest, relink). */
export function reconcile(s: SelState, items: string[]): SelState {
  const present = new Set(items);
  const order = s.order.filter((h) => present.has(h));
  const focus = s.focus >= items.length ? items.length - 1 : s.focus;
  const anchor = s.anchor >= items.length ? -1 : s.anchor;
  return order.length === s.order.length && focus === s.focus && anchor === s.anchor
    ? s
    : { order, focus, anchor };
}
