/**
 * Theme store (Svelte 5 runes): the single reactive owner of the interface
 * theme preference. Thin by design — the resolve/apply/matchMedia logic lives
 * in theme.ts (pure + DOM); this slice holds the `$state`, persists through
 * prefs, and arms the OS watcher.
 *
 * One instance is shared across the app (the module-level `theme` export). It
 * is deliberately NOT folded into app.svelte.ts: the theme applies to both
 * webviews (main + Settings windows) and is orthogonal to the per-image
 * [data-surround] backdrop. Settings reads/writes this same instance so the
 * segmented control and the live chrome stay in lockstep.
 */
import { loadTheme, saveTheme } from "../state/prefs";
import {
  applyTheme,
  resolveTheme,
  watchSystemTheme,
  type ResolvedTheme,
  type ThemeMode,
} from "./theme";

export class ThemeStore {
  /** The user's preference: system | light | dark. */
  mode = $state<ThemeMode>(loadTheme());

  /** The concrete theme currently painted (what data-theme resolved to). */
  resolved = $state<ResolvedTheme>(resolveTheme(this.mode));

  #unwatch: (() => void) | null = null;

  /** Paint the persisted preference and arm the OS watcher. Idempotent: a
   * second init re-applies without stacking listeners. Call once per webview
   * at boot (from main.ts / settings-main.ts). */
  init(): void {
    this.resolved = applyTheme(this.mode);
    this.#unwatch?.();
    // The watcher reads `this.mode` live, so it only repaints while the
    // preference is `system` — an explicit light/dark is never clobbered by
    // an OS flip. The onApply callback mirrors the resolved value back onto
    // the reactive `resolved` field so any UI showing it updates too.
    this.#unwatch = watchSystemTheme(
      () => this.mode,
      (resolved) => (this.resolved = resolved),
    );
  }

  /** Set the preference: persist, repaint, and refresh the resolved value. */
  set(mode: ThemeMode): void {
    this.mode = mode;
    saveTheme(mode);
    this.resolved = applyTheme(mode);
  }

  /** Tear down the OS watcher (tests; webview teardown). */
  dispose(): void {
    this.#unwatch?.();
    this.#unwatch = null;
  }
}

/** Shared singleton — every webview and the Settings control use this one. */
export const theme = new ThemeStore();
