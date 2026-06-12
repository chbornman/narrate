/**
 * Platform-aware window chrome (UI §2.3): on macOS the window keeps
 * native decorations and the Overlay traffic lights own minimize/
 * maximize/close — Titlebar.svelte must NOT render its custom controls
 * and must reserve the lights' left footprint (a draggable inset);
 * SettingsApp.svelte's drag strip gets the same treatment. Windows/
 * Linux keep the custom controls with no inset, unchanged. The platform
 * probe (logic/platform.ts) reads navigator at call time, so each case
 * stubs platform/userAgent BEFORE mounting.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/svelte";

vi.mock("@tauri-apps/api/core", () => ({
  // list_roots feeds an {#each}; everything else in these suites is
  // null-guarded in the templates.
  invoke: vi.fn(async (cmd: string) => (cmd === "list_roots" ? [] : null)),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    setTitle: async () => {},
    minimize: async () => {},
    toggleMaximize: async () => {},
    close: async () => {},
    setFullscreen: async () => {},
  }),
}));

import Titlebar from "../src/lib/components/shell/Titlebar.svelte";
import SettingsApp from "../src/lib/settings/SettingsApp.svelte";
import { isMac } from "../src/lib/logic/platform";
import { ui } from "../src/lib/state/app.svelte";

const MAC_UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
const LINUX_UA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko)";

/** Own-property shadows over jsdom's prototype getters; delete restores. */
function stubNavigator(platform: string, userAgent: string) {
  Object.defineProperty(window.navigator, "platform", { value: platform, configurable: true });
  Object.defineProperty(window.navigator, "userAgent", { value: userAgent, configurable: true });
}

beforeEach(() => {
  document.body.innerHTML = "";
});

afterEach(() => {
  // Remove the shadowing own props so jsdom's defaults return for the
  // next suite (delete on a configurable own prop reverts to prototype).
  delete (window.navigator as { platform?: string }).platform;
  delete (window.navigator as { userAgent?: string }).userAgent;
});

describe("isMac probe", () => {
  it("detects macOS via navigator.platform", () => {
    stubNavigator("MacIntel", LINUX_UA);
    expect(isMac()).toBe(true);
  });

  it("falls back to userAgent when platform is blank", () => {
    stubNavigator("", MAC_UA);
    expect(isMac()).toBe(true);
  });

  it("is false on Linux", () => {
    stubNavigator("Linux x86_64", LINUX_UA);
    expect(isMac()).toBe(false);
  });
});

describe("Titlebar platform chrome", () => {
  it("macOS: custom window controls are dropped; the traffic-light inset is reserved and draggable", () => {
    stubNavigator("MacIntel", MAC_UA);
    const { container, queryByLabelText } = render(Titlebar, { title: "shoots" });

    // The native lights own these verbs — no duplicate custom buttons.
    expect(queryByLabelText("Minimize")).toBeNull();
    expect(queryByLabelText("Maximize")).toBeNull();
    expect(queryByLabelText("Close")).toBeNull();

    // The inset clears the lights AND stays part of the drag region.
    const inset = container.querySelector(".traffic-inset");
    expect(inset).not.toBeNull();
    expect(inset?.hasAttribute("data-tauri-drag-region")).toBe(true);

    // Registry-backed accessories survive the split untouched.
    expect(queryByLabelText("Toggle source rail")).not.toBeNull();
    expect(queryByLabelText("Search")).not.toBeNull();
  });

  it("Linux: custom window controls render; no inset", () => {
    stubNavigator("Linux x86_64", LINUX_UA);
    const { container, queryByLabelText } = render(Titlebar, { title: "shoots" });

    expect(queryByLabelText("Minimize")).not.toBeNull();
    expect(queryByLabelText("Maximize")).not.toBeNull();
    expect(queryByLabelText("Close")).not.toBeNull();
    expect(container.querySelector(".traffic-inset")).toBeNull();
  });

  it("settings window: macOS drops the custom close and insets the drag strip; Linux keeps it", () => {
    stubNavigator("MacIntel", MAC_UA);
    const mac = render(SettingsApp);
    expect(mac.queryByLabelText("Close")).toBeNull();
    expect(mac.container.querySelector(".drag.mac[data-tauri-drag-region]")).not.toBeNull();

    document.body.innerHTML = "";
    stubNavigator("Linux x86_64", LINUX_UA);
    const linux = render(SettingsApp);
    expect(linux.queryByLabelText("Close")).not.toBeNull();
    expect(linux.container.querySelector(".drag.mac")).toBeNull();
  });

  it("background-jobs pill: hidden when idle, one quiet word with the kind breakdown on hover", () => {
    // BACKLOG "Header shows background jobs": the register is the word
    // appearing; count + kind live in the hover title, never a progress
    // bar. Everything queued in ingest_passes (ingest, rebuilds, doctor
    // re-pends, model backfills) flows through the same passes list.
    stubNavigator("MacIntel", MAC_UA);
    ui.shell.ingest = { running: false, done: 0, total: 0, errors: 0, passes: [] };
    const idle = render(Titlebar, { title: "shoots" });
    expect(idle.container.querySelector(".jobs")).toBeNull();

    document.body.innerHTML = "";
    ui.shell.ingest = {
      running: true,
      done: 3,
      total: 500,
      errors: 0,
      passes: [
        { name: "hash", remaining: 12 },
        { name: "image-embedding", remaining: 485 },
      ],
    };
    const busy = render(Titlebar, { title: "shoots" });
    const pill = busy.container.querySelector(".jobs");
    expect(pill?.textContent).toBe("digesting");
    expect(pill?.getAttribute("title")).toBe(
      "Still digesting — hashing 12 · embedding images 485",
    );
    ui.shell.ingest = { running: false, done: 0, total: 0, errors: 0, passes: [] };
  });

  it("the bar itself stays a drag region on both platforms", () => {
    for (const [platform, ua] of [
      ["MacIntel", MAC_UA],
      ["Linux x86_64", LINUX_UA],
    ] as const) {
      document.body.innerHTML = "";
      stubNavigator(platform, ua);
      const { container } = render(Titlebar, { title: "shoots" });
      expect(container.querySelector(".titlebar[data-tauri-drag-region]")).not.toBeNull();
    }
  });
});
