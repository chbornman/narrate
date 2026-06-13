/**
 * Surround store (Svelte 5 runes): the single reactive owner of the image
 * backdrop surround. Mirrors the theme store's shape — thin slice over the
 * pure surround.ts (levels, codec, the follow-theme mapping) plus prefs
 * persistence.
 *
 * WHY a standalone store (not folded into the shell slice): the surround now
 * has TWO inputs that must compose reactively — a `follow-theme | manual`
 * MODE, and (under follow-theme) the live resolved interface theme. Deriving
 * the active level from `theme.resolved` (a $state on the shared ThemeStore)
 * means an OS flip or a Settings theme change repaints the backdrop with no
 * extra wiring: the $derived re-runs. The shell slice keeps a thin getter that
 * reads `surround.level`, so App.svelte (`data-surround`) and the set-surround
 * action seats stay exactly as they were.
 *
 * Modes:
 *  - follow-theme (DEFAULT): `level` = surroundForTheme(theme.resolved).
 *  - manual: `level` = the pinned `manualLevel` (persisted at `pp.surround`).
 *
 * The set-surround action calls `pick(level)`, which IMPLIES manual mode — the
 * user reaching for a specific backdrop wants it pinned. Switching back to
 * follow-theme is the Settings toggle (`setMode("follow-theme")`).
 */
import {
  loadSurround,
  loadSurroundMode,
  saveSurround,
  saveSurroundMode,
} from "../state/prefs";
import { theme } from "./theme-store.svelte";
import {
  surroundForTheme,
  type SurroundLevel,
  type SurroundMode,
} from "./surround";

export class SurroundStore {
  /** follow-theme | manual; default follow-theme (loadSurroundMode). */
  mode = $state<SurroundMode>(loadSurroundMode());

  /** The pinned MANUAL level (only consulted while mode === "manual").
   * Reuses the existing pp.surround key so an old install's level survives. */
  manualLevel = $state<SurroundLevel>(loadSurround());

  /** The active surround level the UI paints. Follow-theme derives it from the
   * live resolved theme (so a theme/OS flip repaints automatically); manual
   * returns the pinned level. */
  level = $derived<SurroundLevel>(
    this.mode === "follow-theme" ? surroundForTheme(theme.resolved) : this.manualLevel,
  );

  /** Set the mode (the Settings toggle). Persisted. Switching to follow-theme
   * does NOT clear the pinned manual level — flipping back restores it. */
  setMode(mode: SurroundMode): void {
    this.mode = mode;
    saveSurroundMode(mode);
  }

  /** Pick a specific level — the manual control (the set-surround action's
   * backdrop/gutter right-click, D6). Implies manual mode and persists both. */
  pick(level: SurroundLevel): void {
    this.manualLevel = level;
    saveSurround(level);
    if (this.mode !== "manual") this.setMode("manual");
  }
}

/** Shared singleton — the shell getter and the Settings control use this one. */
export const surround = new SurroundStore();
