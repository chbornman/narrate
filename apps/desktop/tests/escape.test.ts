/**
 * Escape = strictly back-one-layer (featureset §0: Esc is sacred), the
 * full 15-layer order, exhaustively: welcome card → redaction modal →
 * drop-confirm → context menu → inline journal correction → journal
 * composer → note input → cheatsheet → indicator popover → debug panel →
 * search → Look→Grid → inspector → clear selection → none. Never quits.
 *
 * AMENDED BY INTEGRATION: the drag-folder drop-confirm sheet (featureset
 * §6) joins as layer 2 — it is scrim-covered topmost chrome like the
 * modal, and the Sheet contract promises Esc dismisses while Esc routes
 * only through this order (recorded in DECISIONS, UI build U-entries).
 *
 * AMENDED BY THE JOURNAL POLISH ROUND (founder, June 2026): Esc from Look
 * back to Grid must NOT close the inspector — "when we return to the
 * grid, an image will still be selected", its panel content with it. The
 * inspector layer moved BELOW search and Look→Grid (every other layer's
 * relative order unchanged); the journal composer joined as a text-edit
 * layer beside the inline correction (§0: text inputs exit first).
 *
 * AMENDED BY THE WAVE-2 ROBUSTNESS CLUSTER (BACKLOG "First-run welcome
 * card"): the storage-story modal joins as layer 1, ABOVE the redaction
 * modal — it is shown before anything else can open, scrim-covered like
 * the one destructive modal; every other layer's relative order is
 * unchanged.
 */
import { describe, expect, it } from "vitest";
import { escapeAction, type EscapeContext } from "../src/lib/logic/escape";

const none: EscapeContext = {
  welcomeCardOpen: false,
  redactionModalOpen: false,
  dropConfirmOpen: false,
  contextMenuOpen: false,
  journalEditOpen: false,
  journalComposerFocused: false,
  noteInputOpen: false,
  cheatsheetOpen: false,
  indicatorPopoverOpen: false,
  debugPanelOpen: false,
  rankingPopoverOpen: false,
  inspectorOpen: false,
  queryScopeActive: false,
  searchBarFocused: false,
  viewMode: "grid",
  hasSelection: false,
};

/** Every layer open at once — peeling must follow the exact order. */
const everything: EscapeContext = {
  welcomeCardOpen: true,
  redactionModalOpen: true,
  dropConfirmOpen: true,
  contextMenuOpen: true,
  journalEditOpen: true,
  journalComposerFocused: true,
  noteInputOpen: true,
  cheatsheetOpen: true,
  indicatorPopoverOpen: true,
  debugPanelOpen: true,
  rankingPopoverOpen: true,
  inspectorOpen: true,
  queryScopeActive: true,
  searchBarFocused: true,
  viewMode: "look",
  hasSelection: true,
};

// (flag to clear, expected action) in layer order. The single search layer
// split into two (M3 search-as-scope): first Esc clears the query SCOPE,
// the next blurs the always-visible bar — the two-press sequence the design
// specifies, both above Look→Grid.
const LAYERS: [keyof EscapeContext, ReturnType<typeof escapeAction>][] = [
  ["welcomeCardOpen", "close-welcome-card"],
  ["redactionModalOpen", "close-redaction-modal"],
  ["dropConfirmOpen", "close-drop-confirm"],
  ["contextMenuOpen", "close-context-menu"],
  ["journalEditOpen", "close-journal-edit"],
  ["journalComposerFocused", "blur-journal-composer"],
  ["noteInputOpen", "close-note-input"],
  ["cheatsheetOpen", "close-cheatsheet"],
  ["indicatorPopoverOpen", "close-indicator-popover"],
  ["debugPanelOpen", "close-debug-panel"],
  ["rankingPopoverOpen", "close-ranking-popover"],
  ["queryScopeActive", "clear-query-scope"],
  ["searchBarFocused", "blur-search-bar"],
  ["viewMode", "leave-look"],
  ["inspectorOpen", "close-inspector"],
  ["hasSelection", "clear-selection"],
];

describe("the 15-layer order, exhaustively", () => {
  it("peels exactly one layer per press, in order, ending at none", () => {
    let ctx = { ...everything };
    for (const [flag, expected] of LAYERS) {
      expect(escapeAction(ctx)).toBe(expected);
      // Clear the layer the action would close and press Escape again.
      ctx = { ...ctx, [flag]: flag === "viewMode" ? "grid" : false };
    }
    expect(escapeAction(ctx)).toBe("none"); // layer 15 — NEVER quits
  });

  it("each layer wins over everything beneath it", () => {
    for (let i = 0; i < LAYERS.length; i++) {
      // Open layer i and all deeper layers; i must win.
      let ctx: EscapeContext = { ...everything };
      for (let j = 0; j < i; j++) {
        const [flag] = LAYERS[j];
        ctx = { ...ctx, [flag]: flag === "viewMode" ? "grid" : false };
      }
      expect(escapeAction(ctx)).toBe(LAYERS[i][1]);
    }
  });
});

describe("contract spot checks", () => {
  it("text-edit layers exit before chrome (§0: exits text inputs first)", () => {
    // An inline journal correction beats the composer beats the note input.
    expect(
      escapeAction({ ...none, journalEditOpen: true, noteInputOpen: true }),
    ).toBe("close-journal-edit");
    expect(
      escapeAction({ ...none, journalComposerFocused: true, noteInputOpen: true }),
    ).toBe("blur-journal-composer");
    expect(escapeAction({ ...none, noteInputOpen: true, inspectorOpen: true })).toBe(
      "close-note-input",
    );
  });

  it("the welcome card peels before everything — it was there first", () => {
    expect(escapeAction(everything)).toBe("close-welcome-card");
  });

  it("the redaction modal (the one destructive modal) cancels next", () => {
    expect(escapeAction({ ...everything, welcomeCardOpen: false })).toBe(
      "close-redaction-modal",
    );
  });

  it("the drop-confirm sheet closes before menus, after the one modal", () => {
    expect(
      escapeAction({ ...none, dropConfirmOpen: true, contextMenuOpen: true }),
    ).toBe("close-drop-confirm");
    expect(
      escapeAction({ ...none, dropConfirmOpen: true, redactionModalOpen: true }),
    ).toBe("close-redaction-modal");
  });

  it("the context menu (incl. sort ▾ and journal rows) closes before any panel", () => {
    expect(
      escapeAction({ ...none, contextMenuOpen: true, inspectorOpen: true }),
    ).toBe("close-context-menu");
  });

  it("the ⚙ ranking popover (Phase 3) peels before the query scope it tunes", () => {
    // The popover floats over the bar: Esc closes it before clearing the
    // query scope underneath, the way every transient popover peels first.
    expect(
      escapeAction({ ...none, rankingPopoverOpen: true, queryScopeActive: true }),
    ).toBe("close-ranking-popover");
    // It is still below the debug panel (its declared layer order).
    expect(
      escapeAction({ ...none, rankingPopoverOpen: true, debugPanelOpen: true }),
    ).toBe("close-debug-panel");
  });

  it("a query scope clears before Look; Look→Grid KEEPS the inspector (founder, June 2026)", () => {
    // M3: a committed query is the grid's scope, peeled before Look. With
    // the bar focused but no scope, the next Esc blurs it (also before Look).
    expect(escapeAction({ ...none, queryScopeActive: true, viewMode: "look" })).toBe(
      "clear-query-scope",
    );
    expect(escapeAction({ ...none, searchBarFocused: true, viewMode: "look" })).toBe(
      "blur-search-bar",
    );
    // The founder ask verbatim: Esc back to the grid must not close the
    // Journal/metadata sidebar — the still-active image's content stays.
    expect(
      escapeAction({ ...none, viewMode: "look", inspectorOpen: true }),
    ).toBe("leave-look");
    // In Grid the inspector then closes first, before the selection.
    expect(
      escapeAction({ ...none, inspectorOpen: true, hasSelection: true }),
    ).toBe("close-inspector");
  });

  it("leaves Look back to Grid, then clears the selection", () => {
    expect(escapeAction({ ...none, viewMode: "look", hasSelection: true })).toBe(
      "leave-look",
    );
    expect(escapeAction({ ...none, hasSelection: true })).toBe("clear-selection");
  });

  it("at the bottom of the stack does nothing — NEVER quits", () => {
    expect(escapeAction(none)).toBe("none");
  });
});
