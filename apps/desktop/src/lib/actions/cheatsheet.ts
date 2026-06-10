/**
 * The `?`/F1 keyboard-map overlay model — pure (ctx) → groups. Rows ≡ the
 * registry minus reserved rows (registry.test.ts asserts the equivalence);
 * rows unavailable in the current context render DIMMED, never hidden
 * (featureset §6: the full key table, no tour).
 */
import type { ActionContext, ActionDef, KeyChord } from "./types";
import { REGISTRY } from "./registry";

export interface CheatRow {
  id: ActionDef["id"];
  scope: ActionDef["scope"];
  verb: string;
  /** Long form when the def provides one. */
  label: string;
  keys: KeyChord[];
  /** false → render dimmed (never hidden). */
  available: boolean;
}

export interface CheatGroup {
  group: ActionDef["group"];
  label: string;
  rows: CheatRow[];
}

const GROUP_ORDER: { group: ActionDef["group"]; label: string }[] = [
  { group: "contract", label: "The contract" },
  { group: "grid", label: "Grid" },
  { group: "look", label: "Look" },
  { group: "panels", label: "Panels" },
  { group: "capture", label: "Capture" },
  { group: "search", label: "Search" },
  { group: "system", label: "System" },
];

export function cheatsheet(ctx: ActionContext): CheatGroup[] {
  const groups: CheatGroup[] = [];
  for (const { group, label } of GROUP_ORDER) {
    const rows: CheatRow[] = [];
    for (const def of REGISTRY) {
      if (def.group !== group || def.reserved === true) continue;
      rows.push({
        id: def.id,
        scope: def.scope,
        verb: def.verb,
        label: def.label ?? def.verb,
        keys: def.keys,
        available: def.available(ctx),
      });
    }
    if (rows.length > 0) groups.push({ group, label, rows });
  }
  return groups;
}
