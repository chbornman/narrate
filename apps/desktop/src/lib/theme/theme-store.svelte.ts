/**
 * Theme store (Svelte 5 runes): the single reactive owner of the interface
 * theme preference. Thin by design — the resolve/apply/matchMedia logic lives
 * in theme.ts (pure + DOM); this slice holds the `$state`, persists through
 * prefs, and arms the OS watcher.
 *
 * One instance PER WEBVIEW (the module-level `theme` export). It is deliberately
 * NOT folded into app.svelte.ts: the theme applies to both webviews (main +
 * Settings windows) and is orthogonal to the per-image [data-surround] backdrop.
 * The main and Settings windows each have their OWN instance (separate JS
 * contexts), so a change in one is broadcast to the other over the
 * `theme-changed` Tauri event (see THEME_EVENT) — that is what keeps the
 * segmented control and the main window's chrome in lockstep.
 */
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";

import { loadTheme, saveTheme } from "../state/prefs";
import {
  applyTheme,
  resolveTheme,
  watchSystemTheme,
  type ResolvedTheme,
  type ThemeMode,
} from "./theme";

/** Cross-window theme broadcast. The app runs TWO webviews (main + Settings),
 * each with its own JS context and localStorage, so a `set()` in one only
 * repaints that window. We mirror the `settings-changed` pattern: the changing
 * window emits this event and every window applies it (and persists it to its
 * own localStorage, which is otherwise isolated). Without this, changing the
 * theme in Settings left the main window stale (founder dogfood). */
const THEME_EVENT = "theme-changed";

/** True only inside the Tauri runtime. The cross-window emit/listen are no-ops
 * under vitest/jsdom (no `__TAURI_INTERNALS__`), so the store stays usable in
 * unit tests without mocking the event API. */
function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export class ThemeStore {
  /** The user's preference: system | light | dark. */
  mode = $state<ThemeMode>(loadTheme());

  /** The concrete theme currently painted (what data-theme resolved to). */
  resolved = $state<ResolvedTheme>(resolveTheme(this.mode));

  #unwatch: (() => void) | null = null;
  /** The pending/resolved cross-window listener handle. Held as the PROMISE (not
   * the resolved fn) so dispose() can unlisten even when called before the async
   * `listen` resolves — it just chains the unlisten onto the promise. */
  #crossListen: Promise<UnlistenFn> | null = null;

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
    // Listen for a theme change made in the OTHER webview and apply it here, so
    // the main window follows a switch made in the Settings window (and vice
    // versa). The setter's own emit echoes back to this window too, but
    // #applyExternal no-ops when the mode already matches, so there is no loop.
    if (inTauri()) {
      // Drop any prior listener (a second init) before arming a fresh one.
      void this.#crossListen?.then((un) => un());
      this.#crossListen = listen<ThemeMode>(THEME_EVENT, (e) =>
        this.#applyExternal(e.payload),
      );
    }
  }

  /** Persist + repaint + refresh the resolved value for `mode`. The shared core
   * of both the local setter and the cross-window apply. */
  #applyLocal(mode: ThemeMode): void {
    this.mode = mode;
    saveTheme(mode);
    this.resolved = applyTheme(mode);
  }

  /** Apply a theme change that arrived from another window. Skips when it
   * matches the current mode — that both ignores the setter's self-echo (no
   * loop) and avoids a redundant repaint. */
  #applyExternal(mode: ThemeMode): void {
    if (mode === this.mode) return;
    this.#applyLocal(mode);
  }

  /** Set the preference: persist, repaint, refresh the resolved value, and
   * broadcast to the other webview so it follows suit. */
  set(mode: ThemeMode): void {
    this.#applyLocal(mode);
    // Fire-and-forget: the broadcast is best-effort chrome sync, never blocking.
    if (inTauri()) void emit(THEME_EVENT, mode);
  }

  /** Tear down the OS watcher and cross-window listener (tests; webview
   * teardown). */
  dispose(): void {
    this.#unwatch?.();
    this.#unwatch = null;
    // Chain the unlisten onto the promise so a dispose() before `listen`
    // resolves still tears the listener down once it does.
    void this.#crossListen?.then((un) => un());
    this.#crossListen = null;
  }
}

/** Shared singleton — every webview and the Settings control use this one. */
export const theme = new ThemeStore();
