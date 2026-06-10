/**
 * Grid selection model (spec/UI.md §3.4 + featureset §1) as pure functions
 * over immutable state. The selection IS the write scope; ORDER MATTERS —
 * CAPTURE §3: multi target order = selection order, recorded as
 * event_targets.position.
 *
 * ACTIVE vs SELECTED (featureset §1, the LrC "most-selected" model):
 * `focus` IS the active image — Look, the inspector, and `R` member-flip
 * act on it; the selected set is the write scope. "focus" is kept as the
 * field name for continuity; read it as "active index" everywhere.
 *
 * Click selects one · Shift+Click range-extends · Cmd/Ctrl+Click toggles ·
 * Cmd/Ctrl+A all · Ctrl+Shift+A none · Escape clears · arrows move the
 * active image · Shift+arrows extend · Ctrl+Space toggles the active item ·
 * Home/End/PgUp/PgDn jump · marquee merges (additive with Ctrl).
 */

export interface SelState {
  /** Selected hashes in selection order. */
  order: string[];
  /** ACTIVE index into the current item list, -1 = none (focus ≡ active). */
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

/** Ctrl+Shift+A (featureset §1) — same semantics as Escape's clear: the
 * selection empties, the active image stays active. */
export function selectNone(s: SelState): SelState {
  return clear(s);
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

/** Home/End (featureset §1): jump the active image to an edge; with
 * `extend`, range-select from the anchor like Shift+arrows. */
export function moveEdge(
  s: SelState,
  items: string[],
  edge: "home" | "end",
  extend: boolean,
): SelState {
  if (items.length === 0) return s;
  const next = edge === "home" ? 0 : items.length - 1;
  if (extend) {
    const from = s.focus < 0 ? next : s.focus;
    const anchored = s.anchor >= 0 ? s : { ...s, anchor: from };
    return rangeTo(anchored, items, next);
  }
  return { ...s, focus: next };
}

/** PageUp/PageDown: move by one viewport of rows (`rows` × `cols`),
 * clamped to the ends. Same extend semantics as arrows. */
export function movePage(
  s: SelState,
  items: string[],
  cols: number,
  rows: number,
  dir: "up" | "down",
  extend: boolean,
): SelState {
  if (items.length === 0) return s;
  const delta = (dir === "up" ? -1 : 1) * Math.max(1, cols) * Math.max(1, rows);
  const from = s.focus < 0 ? 0 : s.focus;
  const next = Math.max(0, Math.min(items.length - 1, from + delta));
  if (extend) {
    const anchored = s.anchor >= 0 ? s : { ...s, anchor: from };
    return rangeTo(anchored, items, next);
  }
  return { ...s, focus: next };
}

/**
 * Marquee entry point (featureset §1): the hit set replaces the selection,
 * or — Ctrl held — merges ADDITIVELY: existing selection order is
 * preserved, new hits append in grid order (CAPTURE §3 selection-order
 * recording stays truthful). The last hit becomes active.
 */
export function marqueeMerge(
  s: SelState,
  items: string[],
  hits: readonly number[],
  additive: boolean,
): SelState {
  const valid = [...new Set(hits)]
    .filter((i) => i >= 0 && i < items.length)
    .sort((a, b) => a - b);
  if (valid.length === 0) return additive ? s : clear(s);
  const hitHashes = valid.map((i) => items[i]);
  const order = additive
    ? [...s.order, ...hitHashes.filter((h) => !s.order.includes(h))]
    : hitHashes;
  const focus = valid[valid.length - 1];
  return { order, focus, anchor: focus };
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
