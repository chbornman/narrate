/**
 * Interface theme (BACKLOG "Full interface themes"): system | light | dark.
 *
 * The app is dark-first; the chrome tokens live in tokens.css, with a light
 * override under [data-theme="light"]. This module owns ONE job: resolve the
 * preference to a concrete light|dark, write it to data-theme on the root,
 * and keep `system` tracking the OS via prefers-color-scheme.
 *
 * WHY a standalone module (not the app store): theme must apply equally to
 * BOTH webviews (the main window via main.ts and the Settings window via
 * settings-main.ts), before any Svelte component mounts, with no flash. It is
 * also deliberately decoupled from app.svelte.ts so the chrome theme and the
 * per-image [data-surround] backdrop stay orthogonal — surround retunes the
 * image backdrop tokens; theme retunes the chrome. They never collide because
 * they write different attributes (data-surround vs data-theme) on different
 * elements.
 */

export type ThemeMode = "system" | "light" | "dark";

/** The concrete result after resolving `system` against the OS. */
export type ResolvedTheme = "light" | "dark";

export const THEME_MODES: readonly ThemeMode[] = ["system", "light", "dark"];

export const DEFAULT_THEME: ThemeMode = "system";

export const THEME_LABELS: Record<ThemeMode, string> = {
  system: "System",
  light: "Light",
  dark: "Dark",
};

/** Persistence codec: stored string → mode, anything else → null. */
export function parseTheme(v: string | null): ThemeMode | null {
  return (THEME_MODES as readonly string[]).includes(v ?? "")
    ? (v as ThemeMode)
    : null;
}

/** The OS color-scheme query. Read through a getter so tests can stub
 * matchMedia per-case without a module-load race. */
function prefersDarkQuery(): MediaQueryList | null {
  // jsdom and very old webviews may lack matchMedia entirely; treat a missing
  // API as "dark" (the app's design default) rather than throwing.
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return null;
  }
  return window.matchMedia("(prefers-color-scheme: dark)");
}

/** Resolve the OS preference to a concrete theme. Missing API → dark. */
export function systemTheme(): ResolvedTheme {
  const q = prefersDarkQuery();
  // No matchMedia → assume dark (the dark-first design default).
  return q === null || q.matches ? "dark" : "light";
}

/**
 * Resolve a preference to the concrete theme to render: an explicit
 * light/dark wins; `system` defers to the OS.
 */
export function resolveTheme(mode: ThemeMode): ResolvedTheme {
  return mode === "system" ? systemTheme() : mode;
}

/** Write the resolved theme to the document root so the [data-theme] CSS
 * blocks take effect. Also sync the native color-scheme so form controls,
 * scrollbars, and the canvas default backdrop match (no light scrollbar on a
 * dark app). Guarded for non-DOM (test) contexts. */
function applyResolved(resolved: ResolvedTheme): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  root.setAttribute("data-theme", resolved);
  // color-scheme drives UA-rendered surfaces (scrollbars, form widgets).
  root.style.colorScheme = resolved;
}

/**
 * Apply a preference now: resolve and write data-theme. Returns the concrete
 * theme that was applied (handy for callers/tests).
 */
export function applyTheme(mode: ThemeMode): ResolvedTheme {
  const resolved = resolveTheme(mode);
  applyResolved(resolved);
  return resolved;
}

/**
 * Subscribe to OS color-scheme changes. Re-applies ONLY while the current
 * preference is `system` (an explicit light/dark must not be overridden when
 * the OS flips). `getMode` is a live getter so the listener always reads the
 * current preference, not a stale snapshot. `onApply` (optional) is handed the
 * freshly-resolved theme after each repaint, so a reactive store can keep its
 * own mirror of the resolved value in sync. Returns an unsubscribe fn; a
 * no-op if matchMedia is unavailable.
 */
export function watchSystemTheme(
  getMode: () => ThemeMode,
  onApply?: (resolved: ResolvedTheme) => void,
): () => void {
  const q = prefersDarkQuery();
  if (q === null) return () => {};
  const onChange = () => {
    if (getMode() !== "system") return;
    const resolved = systemTheme();
    applyResolved(resolved);
    onApply?.(resolved);
  };
  // addEventListener is the modern API; older WebKit only has addListener.
  if (typeof q.addEventListener === "function") {
    q.addEventListener("change", onChange);
    return () => q.removeEventListener("change", onChange);
  }
  // eslint-disable-next-line @typescript-eslint/no-deprecated
  q.addListener(onChange);
  // eslint-disable-next-line @typescript-eslint/no-deprecated
  return () => q.removeListener(onChange);
}
