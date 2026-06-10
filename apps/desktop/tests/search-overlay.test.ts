/**
 * Search-surface rendering against the mocked §5.4 contract: verbatim
 * quotes with ⟦⟧ → <mark>, provenance variants, the zero-result line, and
 * the no-raw-HTML guarantee, asserted on real DOM.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/svelte";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import SearchResultRow from "../src/lib/components/search/SearchResultRow.svelte";
import type { ImageResult } from "../src/lib/types/search";

const result = (provenance: ImageResult["provenance"]): ImageResult => ({
  image_hash: "ab".repeat(32),
  preview: "ab".repeat(32),
  score: 0.0439,
  provenance,
  last_annotated_ts: "2026-01-14T21:08:11Z",
  debug: null,
});

const noop = () => {};

beforeEach(() => {
  document.body.innerHTML = "";
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
    expect(container.textContent).toContain("— 12 Jan 2026");
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
