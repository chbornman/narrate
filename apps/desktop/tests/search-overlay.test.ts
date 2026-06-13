/**
 * Search-surface rendering against the mocked §5.4 contract: verbatim
 * quotes with ⟦⟧ → <mark>, provenance variants, the zero-result line, and
 * the no-raw-HTML guarantee, asserted on real DOM — plus the two-stage
 * presentation (backlog "Search entry as overlay, results as canvas"):
 * entry floats the input over a dim scrim with the invoking surface
 * visible behind (honest overlay, DECISIONS I1); results expand to the
 * full-canvas contact sheet the moment they exist.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { ui } from "../src/lib/state/app.svelte";
import { ZERO_RESULTS_LINE } from "../src/lib/search/render";
import SearchOverlay from "../src/lib/components/search/SearchOverlay.svelte";
import SearchResultRow from "../src/lib/components/search/SearchResultRow.svelte";
import type { ImageResult, SearchResults } from "../src/lib/types/search";

const result = (provenance: ImageResult["provenance"]): ImageResult => ({
  image_hash: "ab".repeat(32),
  preview: "ab".repeat(32),
  score: 0.0439,
  provenance,
  last_annotated_ts: "2026-01-14T21:08:11Z",
  debug: null,
});

const noop = () => {};

/** A §5.4 results payload with the given images (echo fields inert here). */
const searchResults = (images: ImageResult[]): SearchResults => ({
  query: { raw: "fog", filters: [], dropped: [], fallback: false },
  images,
  session_hits: [],
});

beforeEach(() => {
  document.body.innerHTML = "";
  // The ui singleton persists across cases — reset the search slice so
  // each stage assertion starts from a closed, queryless overlay.
  ui.searchOpen = false;
  ui.query = "";
  ui.chips = [];
  ui.results = null;
  ui.searchFocus = -1;
  ui.searchSel = { order: [], focus: -1, anchor: -1 };
});

describe("entry overlay vs results canvas (backlog: search entry as overlay)", () => {
  it("empty query floats the input over a dim scrim — context visible behind", () => {
    const { container } = render(SearchOverlay);
    const overlay = container.querySelector(".overlay") as HTMLElement;
    expect(overlay.dataset.stage).toBe("entry");
    // The honest overlay: a translucent dim layer, NOT an opaque canvas —
    // the invoking surface stays visible behind it (I1).
    expect(container.querySelector(".dim")).not.toBeNull();
    expect(container.querySelector("[role='listbox']")).toBeNull();
    // One input, present and ready (§5.1) — and nothing else: no trending,
    // no recents, no tips below it (§5.2).
    expect(container.querySelector("[data-search-input]")).not.toBeNull();
    expect(container.textContent).not.toContain(ZERO_RESULTS_LINE);
  });

  it("zero results stay in the entry overlay with the single dimmed line", () => {
    ui.results = searchResults([]);
    const { container } = render(SearchOverlay);
    // No results arrived → the canvas is never spent on an empty answer;
    // the §5.2 line renders inside the floating panel instead.
    expect((container.querySelector(".overlay") as HTMLElement).dataset.stage).toBe(
      "entry",
    );
    expect(container.querySelector(".dim")).not.toBeNull();
    expect(screen.getByText(ZERO_RESULTS_LINE)).toBeTruthy();
    expect(container.querySelector("[role='listbox']")).toBeNull();
  });

  it("results expand to the full canvas contact sheet as they arrive", async () => {
    const { container } = render(SearchOverlay);
    expect((container.querySelector(".overlay") as HTMLElement).dataset.stage).toBe(
      "entry",
    );
    // Results land (as-you-type flow writes ui.results) → canvas stage.
    ui.results = searchResults([result({ type: "visual_match" })]);
    await tick();
    const overlay = container.querySelector(".overlay") as HTMLElement;
    expect(overlay.dataset.stage).toBe("canvas");
    // The dim scrim is gone — the canvas is the surface now.
    expect(container.querySelector(".dim")).toBeNull();
    // The contact sheet is unchanged: provenance rows under the listbox.
    const listbox = container.querySelector("[role='listbox']");
    expect(listbox).not.toBeNull();
    expect(listbox?.querySelectorAll("[role='option']").length).toBe(1);
    // The input survives the stage flip (same element — no remount while
    // the user is mid-keystroke).
    expect(container.querySelector("[data-search-input]")).not.toBeNull();
  });

  it("emptying the answer honestly collapses the canvas back to the overlay", async () => {
    ui.results = searchResults([result({ type: "filter_only" })]);
    const { container } = render(SearchOverlay);
    expect((container.querySelector(".overlay") as HTMLElement).dataset.stage).toBe(
      "canvas",
    );
    // The query shrinks below the threshold → runSearch nulls results →
    // the stage is DERIVED, so the floating entry overlay returns.
    ui.results = null;
    await tick();
    expect((container.querySelector(".overlay") as HTMLElement).dataset.stage).toBe(
      "entry",
    );
    expect(container.querySelector(".dim")).not.toBeNull();
  });

  it("the dim is visual only — pointerdown does NOT dismiss (Esc is the exit, §5.1)", async () => {
    await ui.openSearch();
    expect(ui.searchOpen).toBe(true);
    const { container } = render(SearchOverlay);
    // UI §5.1 enumerates Escape as the return path; a dismissing scrim
    // would be a new affordance needing a spec ruling (Sheet's contract:
    // scrim instances are ENUMERATED). The dim only keeps the layer
    // beneath inert.
    await fireEvent.pointerDown(container.querySelector(".dim") as HTMLElement);
    expect(ui.searchOpen).toBe(true);
  });

  it("clicks inside the floating panel do NOT dismiss either", async () => {
    await ui.openSearch();
    const { container } = render(SearchOverlay);
    await fireEvent.pointerDown(container.querySelector(".bar") as HTMLElement);
    expect(ui.searchOpen).toBe(true);
  });

  it("reopening `/` after Enter→Look starts at the entry overlay, never a stale canvas", async () => {
    // First search: results arrived, the user pressed Enter on a result.
    // openLook(hash, true) flips searchOpen off DIRECTLY — it never goes
    // through closeSearch — so query/results survive the close.
    await ui.openSearch();
    ui.query = "fog";
    ui.results = searchResults([result({ type: "visual_match" })]);
    await ui.openLook("ab".repeat(32), true);
    expect(ui.searchOpen).toBe(false);

    // `/` again: a FRESH entry. Stale results would otherwise derive the
    // canvas stage instantly — no floating input, no dimmed surface —
    // making the entry overlay first-run-only.
    await ui.openSearch();
    expect(ui.query).toBe("");
    expect(ui.results).toBeNull();
    const { container } = render(SearchOverlay);
    expect((container.querySelector(".overlay") as HTMLElement).dataset.stage).toBe(
      "entry",
    );
    expect(container.querySelector(".dim")).not.toBeNull();
    expect(container.querySelector("[role='listbox']")).toBeNull();

    // Leave Look so later cases start from the grid surface again.
    await ui.closeSearch();
    await ui.leaveLook();
  });
});

describe("quote provenance", () => {
  it("renders the verbatim quote with ⟦⟧ mapped to <mark>", () => {
    const { container } = render(SearchResultRow, {
      result: result({
        type: "quote",
        event_id: "01HT8M",
        session_id: "01HT00",
        ts: "2026-01-12T09:31:00Z",
        source: "voice",
        text: "the ⟦fog⟧ swallowing the ⟦ba⟧rn, keep this one",
        char_start: 0,
        char_end: 46,
        highlights: [],
        linked_stroke: null,
      }),
      focused: false,
      selected: false,
      onopen: noop,
      onselect: noop,
    });
    const marks = [...container.querySelectorAll("mark")].map((m) => m.textContent);
    expect(marks).toEqual(["fog", "ba"]);
    expect(container.textContent).toContain(
      "the fog swallowing the barn, keep this one",
    );
    // No sentinel characters leak into the DOM.
    expect(container.textContent).not.toContain("⟦");
    // Date renders per the layout sketch (§5.1).
    expect(container.textContent).toContain("- 12 Jan 2026");
  });

  it("never injects HTML from quote text (no raw HTML through IPC)", () => {
    const { container } = render(SearchResultRow, {
      result: result({
        type: "quote",
        event_id: "01X",
        session_id: "01Y",
        ts: "2026-01-12T09:31:00Z",
        source: "typed",
        text: '<img src=x onerror="boom"> ⟦<b>bold</b>⟧',
        char_start: 0,
        char_end: 10,
        highlights: [],
        linked_stroke: null,
      }),
      focused: false,
      selected: false,
      onopen: noop,
      onselect: noop,
    });
    // The markup arrives as TEXT — no injected <b>, no extra <img>.
    expect(container.querySelectorAll("b").length).toBe(0);
    expect(container.querySelectorAll("img").length).toBe(1); // the thumbnail only
    expect(container.textContent).toContain('<img src=x onerror="boom">');
  });
});

describe("non-quote provenance renders honestly (RETRIEVAL §6)", () => {
  it("visual match → label, no fake quote", () => {
    const { container } = render(SearchResultRow, {
      result: result({ type: "visual_match" }),
      focused: false,
      selected: false,
      onopen: noop,
      onselect: noop,
    });
    expect(container.textContent).toContain("visual match");
    expect(container.querySelector(".quote")).toBeNull();
  });

  it("filter-only → 'matches your filters'", () => {
    render(SearchResultRow, {
      result: result({ type: "filter_only" }),
      focused: false,
      selected: false,
      onopen: noop,
      onselect: noop,
    });
    expect(screen.getByText("matches your filters")).toBeTruthy();
  });

  it("stroke → stroke reference with its date", () => {
    const { container } = render(SearchResultRow, {
      result: result({
        type: "stroke",
        event_id: "01H",
        session_id: "01S",
        ts: "2025-11-03T10:00:00Z",
      }),
      focused: false,
      selected: false,
      onopen: noop,
      onselect: noop,
    });
    expect(container.textContent).toContain("marked with the pencil");
    expect(container.textContent).toContain("3 Nov 2025");
  });
});

describe("quiet rules", () => {
  it("no score, signal name, or ranking explanation appears (UI §5.3)", () => {
    const { container } = render(SearchResultRow, {
      result: result({ type: "filter_only" }),
      focused: false,
      selected: false,
      onopen: noop,
      onselect: noop,
    });
    expect(container.textContent).not.toContain("0.04");
    expect(container.textContent?.toLowerCase()).not.toContain("score");
  });

  it("thumbnails load via the photoproof protocol, never data/blob URLs", () => {
    const { container } = render(SearchResultRow, {
      result: result({ type: "filter_only" }),
      focused: false,
      selected: false,
      onopen: noop,
      onselect: noop,
    });
    const img = container.querySelector("img.thumb") as HTMLImageElement;
    expect(img.src).toBe(`photoproof://localhost/thumb/${"ab".repeat(32)}`);
  });
});
