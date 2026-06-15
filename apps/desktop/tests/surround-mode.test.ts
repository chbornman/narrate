/**
 * Surround MODE (theme/surround.ts + surround-store.svelte.ts): the
 * follow-theme | manual mode, its codec/default, the documented theme→surround
 * mapping, and the store that composes the mode with the live resolved theme.
 *
 * Covers (the gate's named cases):
 *  - default mode is follow-theme;
 *  - follow-theme DERIVES the surround from the theme (dark vs light);
 *  - a theme flip updates the surround while follow-theme;
 *  - manual override wins AND persists.
 *
 * The store derives from the shared `theme` singleton's resolved value, so
 * cases drive that singleton (theme.set / the OS watcher) and read back
 * `store.level`. jsdom ships no matchMedia; the controllable stub mirrors the
 * one in theme.test.ts. localStorage is the in-memory shim (tests/setup).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_SURROUND_MODE,
  parseSurroundMode,
  surroundForTheme,
  SURROUND_MODES,
} from "../src/lib/theme/surround";
import { SurroundStore } from "../src/lib/theme/surround-store.svelte";
import { theme } from "../src/lib/theme/theme-store.svelte";

/** In-memory stand-in for the Tauri cross-window event API (see theme.test.ts).
 * `emit` dispatches synchronously to every listener, so the two-webview surround
 * propagation is testable with no Tauri runtime. */
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

/** Controllable matchMedia stub (same shape as theme.test.ts): seed the OS
 * value, then flip it and notify the watcher to simulate an OS theme switch. */
function installMatchMedia(dark: boolean) {
  const listeners = new Set<() => void>();
  const mql = {
    matches: dark,
    media: "(prefers-color-scheme: dark)",
    addEventListener: (_: string, cb: () => void) => listeners.add(cb),
    removeEventListener: (_: string, cb: () => void) => listeners.delete(cb),
    addListener: (cb: () => void) => listeners.add(cb),
    removeListener: (cb: () => void) => listeners.delete(cb),
    dispatchEvent: () => true,
    onchange: null,
  } as unknown as MediaQueryList;

  vi.stubGlobal("matchMedia", () => mql);
  (window as unknown as { matchMedia: () => MediaQueryList }).matchMedia = () => mql;

  return {
    fire(nextDark: boolean) {
      (mql as { matches: boolean }).matches = nextDark;
      for (const cb of listeners) cb();
    },
  };
}

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
});

afterEach(() => {
  theme.dispose();
  vi.unstubAllGlobals();
});

describe("mode codec & default", () => {
  it("exactly the two modes; follow-theme is the default", () => {
    expect(SURROUND_MODES).toEqual(["follow-theme", "manual"]);
    expect(DEFAULT_SURROUND_MODE).toBe("follow-theme");
  });

  it("round-trips both modes and rejects junk", () => {
    for (const m of SURROUND_MODES) expect(parseSurroundMode(m)).toBe(m);
    expect(parseSurroundMode(null)).toBeNull();
    expect(parseSurroundMode("")).toBeNull();
    expect(parseSurroundMode("auto")).toBeNull();
  });

  it("a fresh store with empty storage defaults to follow-theme", () => {
    installMatchMedia(true);
    const s = new SurroundStore();
    expect(s.mode).toBe("follow-theme");
  });
});

describe("theme→surround mapping (the documented derivation)", () => {
  it("dark theme → a dark surround, light theme → a light surround", () => {
    expect(surroundForTheme("dark")).toBe("dark");
    expect(surroundForTheme("light")).toBe("light");
  });
});

describe("follow-theme derives the surround from the theme", () => {
  it("dark theme yields a dark surround; light theme a light surround", () => {
    installMatchMedia(true);
    theme.init();

    theme.set("dark");
    const s = new SurroundStore(); // follow-theme by default
    expect(s.mode).toBe("follow-theme");
    expect(s.level).toBe("dark");

    theme.set("light");
    expect(s.level).toBe("light"); // the derived level follows the theme
  });

  it("a theme flip updates the surround while follow-theme", () => {
    const os = installMatchMedia(true); // OS dark
    theme.set("system"); // follow the OS
    theme.init(); // arm the watcher so an OS flip repaints

    const s = new SurroundStore();
    expect(s.level).toBe("dark"); // OS dark → dark surround

    os.fire(false); // OS flips to light
    expect(theme.resolved).toBe("light");
    expect(s.level).toBe("light"); // follow-theme followed the flip
  });
});

describe("manual override", () => {
  it("a manual pick wins over the theme and persists, flipping mode to manual", () => {
    installMatchMedia(true);
    theme.init();
    theme.set("dark"); // theme dark → follow-theme would give a dark surround

    const s = new SurroundStore();
    expect(s.level).toBe("dark");

    s.pick("white"); // user pins white
    expect(s.mode).toBe("manual"); // picking implies manual
    expect(s.level).toBe("white"); // manual wins over the dark theme
    expect(localStorage.getItem("pp.surroundMode")).toBe("manual");
    expect(localStorage.getItem("pp.surround")).toBe("white");

    // A fresh store (a relaunch) restores manual + the pinned level.
    const reloaded = new SurroundStore();
    expect(reloaded.mode).toBe("manual");
    expect(reloaded.level).toBe("white");
  });

  it("a theme flip does NOT move the surround while manual", () => {
    const os = installMatchMedia(true);
    theme.set("system");
    theme.init();

    const s = new SurroundStore();
    s.pick("middle"); // pin a level → manual

    os.fire(false); // OS flips light...
    expect(theme.resolved).toBe("light");
    expect(s.level).toBe("middle"); // ...manual level is untouched
  });

  it("switching back to follow-theme restores the derived level; the pin survives", () => {
    installMatchMedia(true);
    theme.init();
    theme.set("light");

    const s = new SurroundStore();
    s.pick("black"); // manual black
    expect(s.level).toBe("black");

    s.setMode("follow-theme");
    expect(s.level).toBe("light"); // derives from the light theme again
    expect(s.manualLevel).toBe("black"); // the pin is remembered

    s.setMode("manual");
    expect(s.level).toBe("black"); // flipping back restores the pinned level
  });
});

describe("cross-window broadcast (main <-> Settings)", () => {
  // The surround MODE and pinned manual LEVEL live in per-webview localStorage,
  // so a change in the Settings window must be broadcast to the main window.
  beforeEach(() => {
    installMatchMedia(true);
    bus.reset();
    (window as unknown as { __TAURI_INTERNALS__: object }).__TAURI_INTERNALS__ = {};
  });
  afterEach(() => {
    delete (window as unknown as { __TAURI_INTERNALS__?: object })
      .__TAURI_INTERNALS__;
  });

  it("a pick()/setMode() in one webview applies in the other", async () => {
    const main = new SurroundStore();
    const settings = new SurroundStore();
    main.init();
    settings.init();
    await Promise.resolve();

    // User pins a manual level in the Settings window.
    settings.pick("white");
    expect(settings.mode).toBe("manual");
    expect(main.mode).toBe("manual"); // main window followed
    expect(main.manualLevel).toBe("white");
    expect(main.level).toBe("white");

    // Flip back to follow-theme in Settings; main follows.
    settings.setMode("follow-theme");
    expect(main.mode).toBe("follow-theme");

    main.dispose();
    settings.dispose();
  });

  it("a disposed window stops following", async () => {
    const main = new SurroundStore();
    const settings = new SurroundStore();
    main.init();
    settings.init();
    await Promise.resolve();

    main.dispose();
    await Promise.resolve(); // unlisten chains off the listen promise
    settings.pick("light");
    expect(main.mode).toBe("follow-theme"); // unchanged: listener gone
    settings.dispose();
  });
});
