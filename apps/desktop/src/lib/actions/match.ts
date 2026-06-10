/**
 * Pure key → action resolution over the registry. Owns the UI §11
 * input-suppression rule and scope precedence; logic/keymap.ts `dispatch`
 * is a thin wrapper over `match`.
 *
 * Resolution:
 *  1. While the context menu is open, the menu controller owns navigation
 *     keys; only Escape dispatches (escape layer 2 closes the menu).
 *  2. A def is eligible when its scope applies (search defs only with the
 *     overlay open; grid/look defs only on their surface with the overlay
 *     closed; inspector defs only while the inspector is open).
 *  3. While a text input is focused, a chord fires only if the def opts in
 *     (`worksInInput`) AND the chord cannot type (modifier chord or a
 *     non-typing key) — single letters always keep typing (§11).
 *  4. available() then enabled() gate; precedence search > inspector >
 *     surface > global resolves cross-scope chord reuse. Same-level
 *     collisions are bugs (registry.test.ts sweeps for them).
 */
import type { KeyInput, Action } from "../logic/keymap";
import type { ActionContext, ActionDef, KeyChord } from "./types";

/** Keys that never insert text — exempt from suppression when the def
 * opts in (Enter/arrows/Backspace stay eligible for the search overlay). */
const NON_TYPING = /^(Escape|Enter|Backspace|ArrowUp|ArrowDown|ArrowLeft|ArrowRight|F\d{1,2})$/;

function keyEq(a: string, b: string): boolean {
  return a.length === 1 && b.length === 1
    ? a.toLowerCase() === b.toLowerCase()
    : a === b;
}

export function chordMatches(c: KeyChord, e: KeyInput): boolean {
  if (!keyEq(c.key, e.key)) return false;
  if ((c.ctrlOrMeta ?? false) !== e.ctrlOrMeta) return false;
  if (c.shift !== undefined && c.shift !== e.shift) return false;
  return true;
}

function scopeEligible(scope: ActionDef["scope"], ctx: ActionContext): boolean {
  switch (scope) {
    case "global":
      return true;
    case "search":
      return ctx.searchOpen;
    case "grid":
      return !ctx.searchOpen && ctx.surface === "grid";
    case "look":
      return !ctx.searchOpen && ctx.surface === "look";
    case "inspector":
      return !ctx.searchOpen && ctx.inspectorOpen !== false;
  }
}

const PRIORITY: Record<ActionDef["scope"], number> = {
  search: 3,
  inspector: 2,
  grid: 1,
  look: 1,
  global: 0,
};

export interface Candidate {
  def: ActionDef;
  chord: KeyChord;
  priority: number;
}

/** All eligible (def, chord) pairs for this input, highest priority first.
 * Exported for the registry collision sweep (registry.test.ts). */
export function candidates(
  e: KeyInput,
  ctx: ActionContext,
  defs: readonly ActionDef[],
): Candidate[] {
  const out: Candidate[] = [];
  for (const def of defs) {
    if (def.reserved) continue; // dispatch to nothing until their packet
    if (!scopeEligible(def.scope, ctx)) continue;
    const chord = def.keys.find((c) => chordMatches(c, e));
    if (chord === undefined) continue;
    if (
      ctx.inputFocused &&
      !(def.worksInInput === true && (chord.ctrlOrMeta === true || NON_TYPING.test(chord.key)))
    )
      continue;
    if (!def.available(ctx)) continue;
    if (def.enabled !== undefined && !def.enabled(ctx)) continue;
    out.push({ def, chord, priority: PRIORITY[def.scope] });
  }
  out.sort((a, b) => b.priority - a.priority);
  return out;
}

export function match(
  e: KeyInput,
  ctx: ActionContext,
  defs: readonly ActionDef[],
): Action | null {
  // (1) menu open: the menu controller owns keys; only Escape escapes.
  if (ctx.contextMenuOpen && e.key !== "Escape") return null;
  for (const { def, chord } of candidates(e, ctx, defs)) {
    const action =
      def.toAction !== undefined
        ? def.toAction(ctx, chord.arg)
        : ({ kind: def.id } as Action);
    if (action !== null) return action;
  }
  return null;
}
