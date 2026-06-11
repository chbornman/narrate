/**
 * Space-at-fit close vs the slice spaceHeld tracker (P5.1-polish review).
 *
 * App.svelte's window keydown registers FIRST (app mount); LookStage's
 * registers later (Look open). Within ONE synchronous keydown dispatch:
 * App's handler performs look-close → leaveLook → look.close() (resets
 * spaceHeld) — and THEN LookStage's still-attached handler runs. Without
 * the surface gate in stageOwnsRawKeys it re-engaged the slice fact, the
 * stage unmounted before any keyup, and the NEXT Look visit started with
 * the pencil overlay's pointer yielded until a stray Space tap or blur.
 * Guarded twice: stageOwnsRawKeys checks ui.surface, and look.open()
 * clears the flag.
 */
import { describe, expect, it, vi } from "vitest";

// jsdom has no ResizeObserver; Svelte's bind:clientWidth needs one. The
// shim never fires — the stage just measures a 0-width viewport, which the
// raw-key pipeline under test never consults.
class ResizeObserverShim {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver ??= ResizeObserverShim as typeof ResizeObserver;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    switch (cmd) {
      case "set_scope":
        return { kind: "single", count: 1, previewHashes: [] };
      case "image_journal":
      case "list_folder":
        return [];
      case "image_metadata":
        return { orientation: 1 };
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { render } from "@testing-library/svelte";
import { ui } from "../src/lib/state/app.svelte";
import { dispatch } from "../src/lib/logic/keymap";
import LookStage from "../src/lib/components/look/LookStage.svelte";

const HASH = "ab".repeat(32);

describe("Space-at-fit close leaves the slice spaceHeld released", () => {
  it("spaceHeld is NOT left engaged after Space closes Look", () => {
    // Surrogate for App.svelte's onKeydown — identical synchronous path.
    function appOnKeydown(e: KeyboardEvent) {
      const ctx = ui.actionContext({
        inputFocused: false,
        searchInputFocused: false,
      });
      const action = dispatch(
        { key: e.key, ctrlOrMeta: e.ctrlKey || e.metaKey, shift: e.shiftKey },
        ctx,
      );
      if (action === null) return;
      e.preventDefault();
      void ui.perform(action);
    }
    window.addEventListener("keydown", appOnKeydown); // App registers first

    ui.surface = "look";
    ui.look.open([{ display: HASH, alt: null }], 0);
    ui.look.atFit = true;
    ui.look.pencilMode = false;

    render(LookStage); // LookStage's svelte:window keydown registers second

    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true }),
    );

    window.removeEventListener("keydown", appOnKeydown);

    expect(ui.surface).toBe("grid"); // the registry row closed Look
    expect(ui.look.spaceHeld).toBe(false); // stale-true = the bug
  });

  it("a fresh Look visit never starts with spaceHeld engaged (open() clears)", () => {
    ui.look.spaceHeld = true; // however a prior visit wedged it
    ui.look.open([{ display: HASH, alt: null }], 0);
    expect(ui.look.spaceHeld).toBe(false);
  });
});
