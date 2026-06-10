/**
 * Search-overlay rows (moved from the P3.2 keymap, semantics unchanged):
 * result navigation, open-result, remove-last-chip. All work with the
 * search input focused — they never insert text (UI §11).
 */
import type { ActionDef } from "../types";

type Dir = "up" | "down" | "left" | "right";

export const SEARCH_DEFS: ActionDef[] = [
  {
    id: "search-open-result",
    verb: "Open result",
    keys: [{ key: "Enter" }],
    scope: "search",
    group: "search",
    available: (ctx) => ctx.searchOpen,
    worksInInput: true,
  },
  {
    id: "search-nav",
    verb: "Move between results",
    keys: [
      { key: "ArrowUp", arg: "up" },
      { key: "ArrowDown", arg: "down" },
      { key: "ArrowLeft", arg: "left" },
      { key: "ArrowRight", arg: "right" },
    ],
    scope: "search",
    group: "search",
    available: (ctx) => ctx.searchOpen,
    worksInInput: true,
    toAction: (ctx, arg) => {
      const dir = arg as Dir;
      // ←/→ keep moving the text cursor while the input is focused.
      if ((dir === "left" || dir === "right") && ctx.searchInputFocused) return null;
      return { kind: "search-nav", dir };
    },
  },
  {
    id: "remove-last-chip",
    verb: "Remove last filter",
    label: "Remove last filter chip (empty input)",
    keys: [{ key: "Backspace" }],
    scope: "search",
    group: "search",
    available: (ctx) => ctx.searchOpen,
    // Fires only from an EMPTY input (UI §11): with a non-empty query the
    // def doesn't match, no preventDefault fires, and Backspace edits text.
    // The empty-input predicate lives HERE, never in a caller.
    enabled: (ctx) => ctx.searchInputFocused && (ctx.queryEmpty ?? false),
    worksInInput: true,
  },
];
