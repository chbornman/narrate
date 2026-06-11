/**
 * REVIEW SCRATCH (will be deleted): does Space-at-fit close leave
 * ui.look.spaceHeld stale? App.svelte's window keydown registers FIRST
 * (app mount); LookStage's registers later (Look open). Within one
 * synchronous keydown dispatch: App's handler performs look-close →
 * leaveLook → look.close() (resets spaceHeld) — and THEN LookStage's
 * still-attached handler runs and re-engages the slice fact.
 */
import { describe, expect, it, vi } from "vitest";

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

describe("REVIEW: Space-at-fit close vs the slice spaceHeld tracker", () => {
  it("spaceHeld must NOT be left engaged after Space closes Look", () => {
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
});
