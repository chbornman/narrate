/**
 * Pure menu keyboard-navigation controller (paired with Menu.svelte; the
 * Svelte wrapper stays thin by construction). App-agnostic: works over any
 * row shape with kind/verb/disabled/children. ↑↓ skip separators and
 * disabled rows, → opens a submenu, ← closes it, Enter activates,
 * single-character typeahead focuses by verb prefix. Escape is NOT
 * handled here — the escape layer (logic/escape.ts, layer 2) closes menus.
 */

export interface NavRow {
  kind: "item" | "radio" | "submenu" | "separator";
  verb?: string;
  disabled?: boolean;
  children?: NavRow[];
}

export interface MenuNav {
  /** Indices of the open submenu chain ([] = top level). */
  path: number[];
  /** Focused index within the rows at `path`. */
  focus: number;
  typed: string;
  typedAt: number;
}

export const TYPEAHEAD_MS = 600;

const selectable = (r: NavRow) => r.kind !== "separator" && r.disabled !== true;

export function rowsAt(rows: readonly NavRow[], path: readonly number[]): readonly NavRow[] {
  let cur = rows;
  for (const i of path) cur = cur[i]?.children ?? [];
  return cur;
}

function firstSelectable(rows: readonly NavRow[]): number {
  const i = rows.findIndex(selectable);
  return i;
}

export function navInit(rows: readonly NavRow[]): MenuNav {
  return { path: [], focus: firstSelectable(rows), typed: "", typedAt: 0 };
}

function step(rows: readonly NavRow[], from: number, dir: 1 | -1): number {
  if (rows.length === 0) return -1;
  let i = from;
  for (let n = 0; n < rows.length; n++) {
    i = (i + dir + rows.length) % rows.length;
    if (selectable(rows[i])) return i;
  }
  return from;
}

export interface NavResult {
  nav: MenuNav;
  /** Row to activate (Enter on an item/radio). */
  activate?: NavRow;
}

export function navKey(
  rows: readonly NavRow[],
  nav: MenuNav,
  key: string,
  now = 0,
): NavResult {
  const level = rowsAt(rows, nav.path);
  const focused = level[nav.focus];

  if (key === "ArrowDown")
    return { nav: { ...nav, focus: step(level, nav.focus, 1), typed: "" } };
  if (key === "ArrowUp")
    return { nav: { ...nav, focus: step(level, nav.focus, -1), typed: "" } };

  if (key === "ArrowRight") {
    if (focused?.kind === "submenu" && focused.children !== undefined) {
      const path = [...nav.path, nav.focus];
      return {
        nav: { path, focus: firstSelectable(focused.children), typed: "", typedAt: now },
      };
    }
    return { nav };
  }

  if (key === "ArrowLeft") {
    if (nav.path.length > 0) {
      const path = nav.path.slice(0, -1);
      return { nav: { path, focus: nav.path[nav.path.length - 1], typed: "", typedAt: now } };
    }
    return { nav };
  }

  if (key === "Enter") {
    if (focused === undefined || !selectable(focused)) return { nav };
    if (focused.kind === "submenu")
      return navKey(rows, nav, "ArrowRight", now);
    return { nav, activate: focused };
  }

  // Typeahead: printable single characters, buffer resets after TYPEAHEAD_MS.
  if (key.length === 1 && key !== " ") {
    const typed =
      now - nav.typedAt > TYPEAHEAD_MS ? key.toLowerCase() : nav.typed + key.toLowerCase();
    const hit = level.findIndex(
      (r) => selectable(r) && (r.verb ?? "").toLowerCase().startsWith(typed),
    );
    return {
      nav: { ...nav, typed, typedAt: now, focus: hit >= 0 ? hit : nav.focus },
    };
  }

  return { nav };
}
