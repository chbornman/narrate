/**
 * THE registry — aggregator only, FROZEN during parallel work. Stage A/B/C
 * extend their own defs/* files; nobody edits this aggregation. Invariants
 * (ids unique per scope, chord-collision sweep, seat/key coverage) are
 * enforced by tests/registry.test.ts.
 */
import type { Action } from "../logic/keymap";
import type { ActionContext, ActionDef } from "./types";
import { GLOBAL_DEFS } from "./defs/global";
import { SEARCH_DEFS } from "./defs/search";
import { RAIL_DEFS } from "./defs/rail";
import { GRID_DEFS } from "./defs/grid";
import { LOOK_DEFS } from "./defs/look";
import { INSPECTOR_DEFS } from "./defs/inspector";

export const REGISTRY: readonly ActionDef[] = [
  ...GLOBAL_DEFS,
  ...SEARCH_DEFS,
  ...RAIL_DEFS,
  ...GRID_DEFS,
  ...LOOK_DEFS,
  ...INSPECTOR_DEFS,
];

/** Registry lookup for display surfaces (tooltips, KeyHint). */
export function defById(id: ActionDef["id"]): ActionDef | undefined {
  return REGISTRY.find((d) => d.id === id);
}

/**
 * Resolve a def's Action exactly as a key press would — availability and
 * enablement gate HERE (on the def), never in a caller. Chrome buttons
 * (titlebar, indicator scope segment) dispatch through this, so a button
 * can never drift from the registry: zero new verbs by construction.
 */
export function resolveAction(
  id: ActionDef["id"],
  ctx: ActionContext,
  arg?: unknown,
): Action | null {
  const def = defById(id);
  if (def === undefined || def.reserved === true) return null;
  if (!def.available(ctx)) return null;
  if (def.enabled !== undefined && !def.enabled(ctx)) return null;
  if (def.toAction !== undefined) return def.toAction(ctx, arg);
  return { kind: def.id } as Action;
}
