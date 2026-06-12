/**
 * UI scale (desktop-conventions pass, June 2026): the Cmd+= / Cmd+− /
 * Cmd+0 webview-zoom ladder — stepping clamps at the ends, reset returns
 * to the design size, and the factor persists like every other UI pref
 * (prefs.ts localStorage). The webview set_zoom call itself is a Tauri
 * fact the perform sink guards with try/catch (toggle-fullscreen
 * precedent) — these tests cover the state + persistence half.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { Ui } from "../src/lib/state/app.svelte";
import { UI_ZOOM_STEPS, loadUiZoom } from "../src/lib/state/prefs";

let ui: Ui;
beforeEach(() => {
  localStorage.clear();
  ui = new Ui();
});

describe("ui-zoom ladder", () => {
  it("steps up and down the ladder from the design size", async () => {
    expect(ui.shell.uiZoom).toBe(1);
    await ui.perform({ kind: "ui-zoom", delta: 1 });
    expect(ui.shell.uiZoom).toBe(1.1);
    await ui.perform({ kind: "ui-zoom", delta: -1 });
    await ui.perform({ kind: "ui-zoom", delta: -1 });
    expect(ui.shell.uiZoom).toBe(0.9);
  });

  it("clamps at both ends (no wraparound — the browser convention)", async () => {
    for (let i = 0; i < UI_ZOOM_STEPS.length + 2; i++) {
      await ui.perform({ kind: "ui-zoom", delta: 1 });
    }
    expect(ui.shell.uiZoom).toBe(UI_ZOOM_STEPS[UI_ZOOM_STEPS.length - 1]);
    for (let i = 0; i < UI_ZOOM_STEPS.length + 2; i++) {
      await ui.perform({ kind: "ui-zoom", delta: -1 });
    }
    expect(ui.shell.uiZoom).toBe(UI_ZOOM_STEPS[0]);
  });

  it("Cmd+0 resets to the design size", async () => {
    await ui.perform({ kind: "ui-zoom", delta: 1 });
    await ui.perform({ kind: "ui-zoom", delta: 1 });
    await ui.perform({ kind: "ui-zoom-reset" });
    expect(ui.shell.uiZoom).toBe(1);
  });

  it("persists across instances like every other UI pref", async () => {
    await ui.perform({ kind: "ui-zoom", delta: 1 });
    expect(loadUiZoom()).toBe(1.1);
    const fresh = new Ui();
    fresh.shell.loadPrefs();
    expect(fresh.shell.uiZoom).toBe(1.1);
  });

  it("rejects an off-ladder persisted value (corrupt storage heals to 1)", () => {
    localStorage.setItem("pp.uiZoom", "3.7");
    expect(loadUiZoom()).toBe(1);
  });
});
