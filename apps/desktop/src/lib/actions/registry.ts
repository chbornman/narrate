/**
 * THE registry — aggregator only, FROZEN during parallel work. Stage A/B/C
 * extend their own defs/* files; nobody edits this aggregation. Invariants
 * (ids unique per scope, chord-collision sweep, seat/key coverage) are
 * enforced by tests/registry.test.ts.
 */
import type { ActionDef } from "./types";
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
