/**
 * Interface theme (theme/theme.ts + theme-store.svelte.ts — BACKLOG "Full
 * interface themes"): the resolver (system → OS), explicit-wins, persistence
 * round-trip, data-theme application, and the matchMedia listener that
 * repaints ONLY while the preference is `system`.
 *
 * jsdom ships no matchMedia, so each case installs a controllable stub: a fake
 * MediaQueryList whose `.matches` we flip and whose registered `change`
 * listeners we fire by hand. localStorage is the in-memory shim (tests/setup).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  applyTheme,
  parseTheme,
  resolveTheme,
  systemTheme,
  watchSystemTheme,
  THEME_MODES,
  type ThemeMode,
} from "../src/lib/theme/theme";
import { ThemeStore } from "../src/lib/theme/theme-store.svelte";

/** In-memory stand-in for the Tauri cross-window event API. `emit` dispatches
 * SYNCHRONOUSLY to every registered listener (including the sender's own — the
 * real broadcast echoes back to the emitting window too), so the two-webview
 * theme propagation is testable with no Tauri runtime. Hoisted so the vi.mock
 * factory can close over it. */
const bus = vi.hoisted(() => {
  const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
  return {
    emit: (name: string, payload: unknown) => {
      for (const cb of [...(listeners.get(name) ?? [])]) cb({ payload });
      return Promise.resolve();
    },
    listen: (name: string, cb: (e: { payload: unknown }) => void) => {
      const set = listeners.get(name) ?? new Set();
      set.add(cb);
      listeners.set(name, set);
      return Promise.resolve(() => set.delete(cb));
    },
    reset: () => listeners.clear(),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ emit: bus.emit, listen: bus.listen }));

/** A minimal, controllable matchMedia stub. `dark` seeds the initial OS value;
 * `fire()` flips it and dispatches `change` to every registered listener — the
 * exact shape watchSystemTheme subscribes to. */
function installMatchMedia(dark: boolean) {
  const listeners = new Set<() => void>();
  const mql = {
    matches: dark,
    media: "(prefers-color-scheme: dark)",
    addEventListener: (_: string, cb: () => void) => listeners.add(cb),
    removeEventListener: (_: string, cb: () => void) => listeners.delete(cb),
    // legacy fallbacks exist on the type but the modern path is what runs here
    addListener: (cb: () => void) => listeners.add(cb),
    removeListener: (cb: () => void) => listeners.delete(cb),
    dispatchEvent: () => true,
    onchange: null,
  } as unknown as MediaQueryList;

  vi.stubGlobal("matchMedia", () => mql);
  // window.matchMedia and the bare global both resolve to the stub in jsdom.
  (window as unknown as { matchMedia: () => MediaQueryList }).matchMedia = () => mql;

  return {
    /** Flip the OS preference and notify listeners (an OS theme switch). */
    fire(nextDark: boolean) {
      (mql as { matches: boolean }).matches = nextDark;
      for (const cb of listeners) cb();
    },
    listenerCount: () => listeners.size,
  };
}

const dataTheme = () => document.documentElement.getAttribute("data-theme");

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("modes & codec", () => {
  it("exactly the three modes, system default", () => {
    expect(THEME_MODES).toEqual(["system", "light", "dark"]);
  });

  it("round-trips every mode and rejects junk", () => {
    for (const m of THEME_MODES) expect(parseTheme(m)).toBe(m);
    expect(parseTheme(null)).toBeNull();
    expect(parseTheme("")).toBeNull();
    expect(parseTheme("sepia")).toBeNull();
  });
});

describe("resolver: system follows the OS, explicit wins", () => {
  it("system resolves to the OS value (dark)", () => {
    installMatchMedia(true);
    expect(systemTheme()).toBe("dark");
    expect(resolveTheme("system")).toBe("dark");
  });

  it("system resolves to the OS value (light)", () => {
    installMatchMedia(false);
    expect(systemTheme()).toBe("light");
    expect(resolveTheme("system")).toBe("light");
  });

  it("an explicit preference wins over the OS", () => {
    installMatchMedia(true); // OS says dark...
    expect(resolveTheme("light")).toBe("light"); // ...explicit light still wins
    installMatchMedia(false); // OS says light...
    expect(resolveTheme("dark")).toBe("dark"); // ...explicit dark still wins
  });

  it("missing matchMedia falls back to dark (the design default)", () => {
    vi.stubGlobal("matchMedia", undefined);
    (window as unknown as { matchMedia: unknown }).matchMedia = undefined;
    expect(systemTheme()).toBe("dark");
  });
});

describe("applyTheme writes data-theme on the root", () => {
  it("light/dark land on <html> and color-scheme syncs", () => {
    installMatchMedia(false);
    applyTheme("dark");
    expect(dataTheme()).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    applyTheme("light");
    expect(dataTheme()).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
  });

  it("system applies the resolved OS value", () => {
    installMatchMedia(false); // OS light
    applyTheme("system");
    expect(dataTheme()).toBe("light");
  });
});

describe("store: persistence round-trip", () => {
  it("set() persists, and a fresh store boots from storage", () => {
    installMatchMedia(true);
    const a = new ThemeStore();
    a.init();
    a.set("light");
    expect(localStorage.getItem("pp.theme")).toBe("light");
    expect(dataTheme()).toBe("light");

    // A fresh store (a relaunch) restores the saved preference.
    const b = new ThemeStore();
    expect(b.mode).toBe("light");
    b.init();
    expect(dataTheme()).toBe("light");
    a.dispose();
    b.dispose();
  });

  it("absent/junk storage falls back to system", () => {
    installMatchMedia(true);
    localStorage.setItem("pp.theme", "sepia"); // corrupt value
    const s = new ThemeStore();
    expect(s.mode).toBe("system");
  });
});

describe("matchMedia listener", () => {
  it("repaints when the OS flips and the preference is system", () => {
    const os = installMatchMedia(true); // OS dark
    const s = new ThemeStore(); // mode = system
    s.init();
    expect(dataTheme()).toBe("dark");

    os.fire(false); // OS flips to light
    expect(dataTheme()).toBe("light");
    expect(s.resolved).toBe("light");
    s.dispose();
  });

  it("does NOT repaint on an OS flip when an explicit theme is set", () => {
    const os = installMatchMedia(true); // OS dark
    const s = new ThemeStore();
    s.init();
    s.set("light"); // explicit light pins the chrome
    expect(dataTheme()).toBe("light");

    os.fire(false); // OS flips...
    expect(dataTheme()).toBe("light"); // ...explicit light is untouched
    os.fire(true);
    expect(dataTheme()).toBe("light");
    s.dispose();
  });

  it("dispose() removes the listener (no leak across relaunches)", () => {
    const os = installMatchMedia(true);
    const s = new ThemeStore();
    s.init();
    expect(os.listenerCount()).toBe(1);
    s.dispose();
    expect(os.listenerCount()).toBe(0);
  });

  it("a watcher reads the mode live, not a snapshot", () => {
    const os = installMatchMedia(true);
    let mode: ThemeMode = "system";
    const unwatch = watchSystemTheme(() => mode);

    os.fire(false); // system → repaints light
    expect(dataTheme()).toBe("light");

    mode = "dark"; // user switched to explicit dark (live getter sees it)
    document.documentElement.setAttribute("data-theme", "dark");
    os.fire(true); // OS flips, but mode is now explicit → no repaint
    expect(dataTheme()).toBe("dark");
    unwatch();
  });
});

describe("cross-window broadcast (main <-> Settings)", () => {
  // The app runs two webviews, each with its own ThemeStore. A change in one
  // must reach the other over the `theme-changed` event — the bug was the main
  // window NOT following a switch made in the Settings window.
  beforeEach(() => {
    installMatchMedia(true); // stable OS dark; isolate the broadcast path
    bus.reset();
    // inTauri() gates the emit/listen; make it true so the store wires them.
    (window as unknown as { __TAURI_INTERNALS__: object }).__TAURI_INTERNALS__ = {};
  });
  afterEach(() => {
    delete (window as unknown as { __TAURI_INTERNALS__?: object })
      .__TAURI_INTERNALS__;
  });

  it("a set() in one webview applies in the other (and self-echo doesn't loop)", async () => {
    const main = new ThemeStore();
    const settings = new ThemeStore();
    main.init();
    settings.init();
    await Promise.resolve(); // flush the listen() promises

    // User flips the toggle in the Settings window.
    settings.set("light");
    expect(settings.mode).toBe("light");
    // The MAIN window followed — the bug fix. (If the sender's self-echo looped,
    // the synchronous emit would have recursed and this test would hang.)
    expect(main.mode).toBe("light");
    expect(dataTheme()).toBe("light");

    // And back the other way.
    main.set("dark");
    expect(settings.mode).toBe("dark");
    expect(dataTheme()).toBe("dark");

    main.dispose();
    settings.dispose();
  });

  it("a disposed window stops following theme changes", async () => {
    const main = new ThemeStore();
    const settings = new ThemeStore();
    main.init();
    settings.init();
    await Promise.resolve();

    main.dispose(); // main window closed
    await Promise.resolve(); // the unlisten chains off the listen promise
    settings.set("light");
    expect(main.mode).toBe("system"); // unchanged: its listener is gone
    settings.dispose();
  });
});
