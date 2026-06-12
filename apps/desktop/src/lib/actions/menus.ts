/**
 * Context-menu models — pure (seat, ctx) → MenuModel. Queries the registry
 * for defs with seats.includes(seat) && available(ctx); per-seat order
 * tables below are written up front; parametrized defs (Rate ▸ · Sort ▸ ·
 * Size ▸ · Surround ▸) expand into radio submenus. Rows carry NO key
 * strings and NO handlers — items carry Actions into ui.perform, key hints
 * are KeyChords rendered by KeyHint.svelte.
 *
 * "Context menus mirror every keyboard verb" (featureset §6) is true by
 * construction: menus render the same defs the keymap dispatches.
 */
import type { Action } from "../logic/keymap";
import type { ActionContext, ActionDef, KeyChord, MenuSeat } from "./types";
import { REGISTRY } from "./registry";
import type { NavRow } from "../primitives/menu";
import { copyKey } from "../primitives/copyflash.svelte";

export interface MenuRow extends NavRow {
  kind: "item" | "radio" | "submenu" | "separator";
  verb?: string;
  action?: Action;
  keyHint?: KeyChord;
  checked?: boolean;
  disabled?: boolean;
  children?: MenuRow[];
  /** Copy verbs confirm inline (BACKLOG): the shared copy register's key
   * this row flashes its check on; Menu.svelte defers its close for it. */
  flashKey?: string;
}

export interface MenuModel {
  rows: MenuRow[];
}

/** The sort ▾ header control opens the same machinery as the seats. */
export type MenuSurface = MenuSeat | "sort";

/** One order-table entry: a def id, a separator, or a labeled submenu
 * grouping several def ids (Stacks ▸). */
type SeatEntry =
  | Action["kind"]
  | "—"
  | { submenu: string; kinds: Action["kind"][] };

/** Per-seat order tables (featureset §6), written up front. Stage defs
 * land into these seats without touching this file beyond their ids,
 * which are already listed. */
const SEAT_ORDER: Record<MenuSeat, SeatEntry[]> = {
  thumb: [
    "open-look",
    "rate",
    // Collections sit beside Rate: both are gather-while-reviewing verbs
    // (B71: the app should encourage collecting). Remove rides directly
    // under Add — membership is reversible from the menu that gathers.
    "add-to-collection",
    "remove-from-collection",
    "stack-toggle-active",
    "flip-stack-member",
    "—",
    "open-inspector", // Metadata/Journal items (Stage C seats them)
    "—",
    "reveal-in-file-manager",
    "copy-file-path",
    "open-with-default",
  ],
  gutter: [
    "select-all",
    "select-none",
    "—",
    "set-sort",
    "set-thumb-step",
    { submenu: "Stacks", kinds: ["stack-collapse-all", "stack-expand-all"] },
    "set-surround",
    "—",
    // Dogfood round 1 reachability audit: every verb gets a pointer seat.
    "cycle-cell-info",
    "toggle-auto-advance",
    "—",
    "toggle-lights-out",
    "toggle-fullscreen",
    "toggle-cheatsheet",
    "open-settings",
  ],
  // Rebuild previews sits right after Rescan: kin verbs (both re-derive
  // index state from files) with deliberately DIFFERENT semantics — the
  // defs' doc comments hold the distinction (BACKLOG, dogfood round 3).
  "rail-folder": [
    "rail-folder-open",
    "rail-folder-reveal",
    "rescan-root",
    "rebuild-previews",
    "—",
    "add-root",
  ],
  // Journal entry rows (select-from-note): the seat arg is the entry's
  // ordered target list. The row verbs (Correct/Retract/Redact…) stay
  // hover affordances by design (defs/inspector.ts) — not menu rows.
  "journal-row": ["select-journal-targets"],
  "look-backdrop": [
    "zoom-toggle",
    "zoom-fit",
    "zoom-100",
    "—",
    // The grease pencil (P5.1): mode + overlay toggles plus undo (graying
    // on pencilUndoable), pointer-seated.
    "pencil-pen",
    "pencil-undo",
    "cycle-overlay",
    "—",
    // Dogfood round 1 reachability audit (Look-side pointer seats).
    "toggle-filmstrip",
    "flip-stack-member",
    "—",
    "set-surround",
    "—",
    "go-grid",
    "toggle-lights-out",
    "toggle-fullscreen",
    "toggle-cheatsheet",
    "open-settings",
  ],
  "look-toolbar": [], // reserved seat (M2a pencil toolbar)
};

function defsForSeat(seat: MenuSeat): ActionDef[] {
  return REGISTRY.filter((d) => d.seats?.includes(seat) && d.reserved !== true);
}

function rowForDef(def: ActionDef, ctx: ActionContext, arg?: unknown): MenuRow | null {
  if (!def.available(ctx)) return null;
  const disabled = def.enabled !== undefined && !def.enabled(ctx);
  if (def.options !== undefined) {
    // Parametrized def → radio submenu.
    const children: MenuRow[] = def.options(ctx).map((o) => ({
      kind: "radio",
      verb: o.label,
      checked: o.checked === true,
      disabled,
      action: actionFor(def, ctx, o.arg),
    }));
    return { kind: "submenu", verb: def.verb, disabled, children };
  }
  const action = actionFor(def, ctx, arg);
  if (action === undefined) return null;
  return {
    kind: "item",
    verb: def.verb,
    keyHint: def.keys[0],
    disabled,
    action,
    // Toggle rows render their ON state (predicate on the def, never here).
    checked: def.checked?.(ctx),
    // Copy verbs key into the shared confirmation register: def id +
    // active hash (the perform sink rebuilds the SAME key from the
    // action kind, which is the def id by construction). WHY the hash:
    // scoping the check to the copied image keeps it truthful when the
    // user moves on to another image inside the flash window.
    flashKey:
      def.copyConfirm === true && ctx.activeHash !== null
        ? copyKey(def.id, ctx.activeHash)
        : undefined,
  };
}

function actionFor(
  def: ActionDef,
  ctx: ActionContext,
  arg?: unknown,
): Action | undefined {
  if (def.toAction !== undefined) return def.toAction(ctx, arg) ?? undefined;
  return { kind: def.id } as Action;
}

/**
 * Build the menu for a seat. `arg` is the seat argument (the rail-folder
 * row, a journal entry id) threaded into parametrized defs.
 */
export function menuModel(
  surface: MenuSurface,
  ctx: ActionContext,
  arg?: unknown,
): MenuModel {
  if (surface === "sort") {
    // sort ▾: the set-sort radio group, flat.
    const def = REGISTRY.find((d) => d.id === "set-sort");
    if (def === undefined || def.options === undefined || !def.available(ctx))
      return { rows: [] };
    return {
      rows: def.options(ctx).map((o) => ({
        kind: "radio" as const,
        verb: o.label,
        checked: o.checked === true,
        action: actionFor(def, ctx, o.arg),
      })),
    };
  }

  const seated = defsForSeat(surface);
  const byId = new Map(seated.map((d) => [d.id, d]));
  const rows: MenuRow[] = [];
  for (const entry of SEAT_ORDER[surface]) {
    if (entry === "—") {
      if (rows.length > 0 && rows[rows.length - 1].kind !== "separator")
        rows.push({ kind: "separator" });
      continue;
    }
    if (typeof entry === "object") {
      const children: MenuRow[] = [];
      for (const kind of entry.kinds) {
        const def = byId.get(kind);
        if (def === undefined) continue;
        const row = rowForDef(def, ctx, arg);
        if (row !== null) children.push(row);
      }
      if (children.length > 0)
        rows.push({ kind: "submenu", verb: entry.submenu, children });
      continue;
    }
    const def = byId.get(entry);
    if (def === undefined) continue; // stage def not landed yet
    const row = rowForDef(def, ctx, arg);
    if (row !== null) rows.push(row);
  }
  // No trailing/leading separators.
  while (rows.length > 0 && rows[rows.length - 1].kind === "separator") rows.pop();
  while (rows.length > 0 && rows[0].kind === "separator") rows.shift();
  return { rows };
}
