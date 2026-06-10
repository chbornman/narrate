/**
 * Escape = strictly back-one-layer (UI §2.2, DECISIONS I1), exact order:
 * transients → search → look → selection → nothing. Never quits.
 */
import { describe, expect, it } from "vitest";
import { escapeAction, type EscapeContext } from "../src/lib/logic/escape";

const none: EscapeContext = {
  noteInputOpen: false,
  sortMenuOpen: false,
  indicatorPopoverOpen: false,
  debugPanelOpen: false,
  searchOpen: false,
  surface: "grid",
  hasSelection: false,
};

describe("escape layering", () => {
  it("(1) closes an open transient before anything else", () => {
    // Worst case: everything open at once — the note input wins.
    const everything: EscapeContext = {
      noteInputOpen: true,
      sortMenuOpen: true,
      indicatorPopoverOpen: true,
      debugPanelOpen: true,
      searchOpen: true,
      surface: "look",
      hasSelection: true,
    };
    expect(escapeAction(everything)).toBe("close-note-input");
    expect(escapeAction({ ...everything, noteInputOpen: false })).toBe(
      "close-sort-menu",
    );
    expect(
      escapeAction({ ...everything, noteInputOpen: false, sortMenuOpen: false }),
    ).toBe("close-indicator-popover");
    expect(
      escapeAction({
        ...everything,
        noteInputOpen: false,
        sortMenuOpen: false,
        indicatorPopoverOpen: false,
      }),
    ).toBe("close-debug-panel");
  });

  it("(2) leaves Search back to the invoking surface", () => {
    expect(escapeAction({ ...none, searchOpen: true, surface: "look" })).toBe(
      "leave-search",
    );
  });

  it("(3) leaves Look back to Grid", () => {
    expect(escapeAction({ ...none, surface: "look" })).toBe("leave-look");
    // Search closes BEFORE Look even when both are active.
    expect(escapeAction({ ...none, surface: "look", searchOpen: true })).toBe(
      "leave-search",
    );
  });

  it("(4) clears a grid selection", () => {
    expect(escapeAction({ ...none, hasSelection: true })).toBe("clear-selection");
  });

  it("at the bottom of the stack does nothing — NEVER quits", () => {
    expect(escapeAction(none)).toBe("none");
  });

  it("transients close before search; search before selection", () => {
    expect(
      escapeAction({ ...none, sortMenuOpen: true, searchOpen: true, hasSelection: true }),
    ).toBe("close-sort-menu");
    expect(escapeAction({ ...none, searchOpen: true, hasSelection: true })).toBe(
      "leave-search",
    );
  });
});
