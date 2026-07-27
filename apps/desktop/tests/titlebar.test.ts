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
import { fireEvent, render, waitFor } from "@testing-library/svelte";

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
import { invoke } from "@tauri-apps/api/core";
import { isMac } from "../src/lib/logic/platform";
import { ui } from "../src/lib/state/app.svelte";
import type { ApplicationHealth } from "../src/lib/types/dto";

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
  vi.mocked(invoke).mockClear();
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

  it("keeps Settings globally reachable from the titlebar", async () => {
    stubNavigator("Linux x86_64", LINUX_UA);
    const rendered = render(Titlebar, { title: "shoots" });

    await fireEvent.click(rendered.getByLabelText("Settings"));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("open_settings_window", { tab: null });
    });
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

  it("Library-status indicator (digest visibility): settled when idle, working when a pass has queued units", () => {
    // The header-center indicator REPLACES the old "digesting" text. Settled
    // shows the calm "Library settled"; a still-draining pass shows the
    // working register (stage label + counts). The offline warning that used
    // to sit beside the text now lives inside this indicator's panel.
    stubNavigator("MacIntel", MAC_UA);
    ui.shell.ingest = { running: false, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [], vectorsVersion: 0, imagesVersion: 0, journalVersion: 0 };
    const idle = render(Titlebar, { title: "shoots" });
    // The indicator is always present (a status region); the OLD jobs text is gone.
    expect(idle.container.querySelector(".jobs")).toBeNull();
    expect(idle.container.querySelector(".libstatus")).not.toBeNull();
    expect(idle.getByText("Library settled")).not.toBeNull();

    document.body.innerHTML = "";
    ui.shell.ingest = {
      running: true,
      done: 3,
      total: 500,
      errors: 0,
      passes: [
        { name: "hash", remaining: 12, done: 488, total: 500, ratePerSec: 4 },
        { name: "image-embedding", remaining: 485, done: 15, total: 500, ratePerSec: 0 },
      ],
      scanning: false,
      discovered: 0, offlineVolumes: [], vectorsVersion: 0, imagesVersion: 0, journalVersion: 0
    };
    const busy = render(Titlebar, { title: "shoots" });
    // Working register: the current (first working) stage's label shows. hash
    // has a positive rate -> it is the working stage the collapsed pill names.
    expect(busy.getByText("Hashing")).not.toBeNull();
    expect(busy.container.querySelector(".pill.working")).not.toBeNull();
    ui.shell.ingest = { running: false, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [], vectorsVersion: 0, imagesVersion: 0, journalVersion: 0 };
  });

  it("uses the backend health projection for the header attention state", () => {
    stubNavigator("MacIntel", MAC_UA);
    const health = {
      issues: [
        {
          id: "disk:wal",
          subsystem: "database",
          title: "Database maintenance is blocked",
          blocking: true,
          summary: "A reader is holding the WAL open.",
          lastError: "reader busy",
          lastErrorAtMs: 1_700_000_000_000,
          action: { kind: "reveal-logs", label: "Reveal logs", targetId: null },
        },
      ],
    } as ApplicationHealth;

    const rendered = render(Titlebar, { title: "shoots", health });
    expect(rendered.getByText("Health action required")).not.toBeNull();
    expect(rendered.container.querySelector(".pill.blocked")).not.toBeNull();
  });

  it("keeps the health popover actionable and opens the System settings tab", async () => {
    stubNavigator("Linux x86_64", LINUX_UA);
    const health = {
      issues: [
        {
          id: "model:clip",
          subsystem: "models",
          title: "Image search model needs verification",
          blocking: false,
          summary: "Partial files need attention.",
          lastError: null,
          lastErrorAtMs: null,
          action: {
            kind: "verify-model",
            label: "Verify model",
            targetId: "clip",
          },
        },
      ],
    } as ApplicationHealth;
    const rendered = render(Titlebar, { title: "shoots", health });

    await fireEvent.pointerEnter(rendered.container.querySelector(".libstatus")!);
    await fireEvent.click(rendered.getByRole("button", { name: "Open health settings" }));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("open_settings_window", { tab: "system" });
    });
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
