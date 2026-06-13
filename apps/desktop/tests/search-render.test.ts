/**
 * Search-result rendering (RETRIEVAL §4/§5.4/§6, UI §5): ⟦⟧ sentinel →
 * <mark> run mapping, highlight ranges, and every provenance variant
 * rendering to honest human-readable output. Runs are TEXT, never HTML.
 */
import { describe, expect, it } from "vitest";
import {
  FILTER_ONLY_LABEL,
  formatDate,
  fuzzyMetaLabel,
  provenanceDisplay,
  quoteRuns,
  rangeRuns,
  sentinelRuns,
  VISUAL_MATCH_LABEL,
  ZERO_RESULTS_LINE,
} from "../src/lib/search/render";
import type { Provenance, Quote } from "../src/lib/types/search";

const quote = (text: string, highlights: [number, number][] = []): Quote => ({
  event_id: "01HT8M",
  session_id: "01HT00",
  ts: "2026-01-12T09:31:00Z",
  source: "voice",
  text,
  char_start: 0,
  char_end: text.length,
  highlights,
  linked_stroke: null,
});

describe("⟦⟧ sentinel mapping (RETRIEVAL §7.2 worked example)", () => {
  it("maps sentinel spans to marked runs", () => {
    const runs = sentinelRuns("the ⟦fog⟧ swallowing the ⟦ba⟧rn, keep this one");
    expect(runs).toEqual([
      { text: "the ", marked: false },
      { text: "fog", marked: true },
      { text: " swallowing the ", marked: false },
      { text: "ba", marked: true },
      { text: "rn, keep this one", marked: false },
    ]);
  });

  it("degrades unbalanced sentinels to literal text (never guesses)", () => {
    const runs = sentinelRuns("lone ⟦sentinel here");
    expect(runs.map((r) => r.marked)).toEqual([false]);
    expect(runs[0].text).toBe("lone ⟦sentinel here");
  });

  it("never emits HTML — markup in the quote stays inert text", () => {
    const runs = sentinelRuns('⟦<img src=x onerror=alert(1)>⟧');
    expect(runs).toEqual([{ text: "<img src=x onerror=alert(1)>", marked: true }]);
  });
});

describe("highlight-range mapping", () => {
  it("wraps ranges as marked runs", () => {
    const runs = rangeRuns("moody light over the harbor", [[0, 5]]);
    expect(runs).toEqual([
      { text: "moody", marked: true },
      { text: " light over the harbor", marked: false },
    ]);
  });

  it("merges overlapping ranges and clamps out-of-bounds", () => {
    const runs = rangeRuns("abcdef", [
      [2, 4],
      [3, 99],
      [-5, 1],
    ]);
    expect(runs).toEqual([
      { text: "a", marked: true },
      { text: "b", marked: false },
      { text: "cdef", marked: true },
    ]);
  });

  it("quoteRuns prefers sentinels when present, else ranges", () => {
    expect(quoteRuns(quote("a ⟦b⟧ c"))).toEqual([
      { text: "a ", marked: false },
      { text: "b", marked: true },
      { text: " c", marked: false },
    ]);
    expect(quoteRuns(quote("plain words", [[0, 5]]))[0]).toEqual({
      text: "plain",
      marked: true,
    });
  });
});

describe("provenance variants (RETRIEVAL §6: no result without a readable why)", () => {
  it("quote → verbatim runs + date + source", () => {
    const d = provenanceDisplay({ type: "quote", ...quote("too literal for the series, shelve it") });
    expect(d.kind).toBe("quote");
    if (d.kind !== "quote") throw new Error("unreachable");
    expect(d.runs.map((r) => r.text).join("")).toBe(
      "too literal for the series, shelve it",
    );
    expect(d.dateLabel).toBe("12 Jan 2026");
    expect(d.source).toBe("voice");
  });

  it("stroke → stroke reference with date", () => {
    const p: Provenance = {
      type: "stroke",
      event_id: "01H",
      session_id: "01S",
      ts: "2025-11-03T10:00:00Z",
    };
    expect(provenanceDisplay(p)).toEqual({ kind: "stroke", dateLabel: "3 Nov 2025" });
  });

  it("visual match → labeled honestly, NO fake quote", () => {
    expect(provenanceDisplay({ type: "visual_match" })).toEqual({
      kind: "visual-match",
    });
    expect(VISUAL_MATCH_LABEL).toBe("visual match");
  });

  it("filter-only → 'matches your filters'", () => {
    expect(provenanceDisplay({ type: "filter_only" })).toEqual({
      kind: "filter-only",
    });
    expect(FILTER_ONLY_LABEL).toBe("matches your filters");
  });

  it("fuzzy-meta → an honest 'approximate' label naming the field", () => {
    // The `~` quiet-toggle's widened hits never pose as exact: each carries an
    // approximate-match label naming the metadata field that fuzzily matched.
    expect(provenanceDisplay({ type: "fuzzy_meta", field: "camera" })).toEqual({
      kind: "fuzzy-meta",
      field: "camera",
    });
    expect(fuzzyMetaLabel("camera")).toBe("approximate camera match");
    expect(fuzzyMetaLabel("lens")).toBe("approximate lens match");
    expect(fuzzyMetaLabel("filename")).toBe("approximate filename match");
  });
});

describe("copy", () => {
  it("zero-result line is the single dimmed sentence (UI §5.2)", () => {
    expect(ZERO_RESULTS_LINE).toBe("Nothing in your journal matches.");
  });

  it("dates render as '12 Jan 2026'", () => {
    expect(formatDate("2026-01-12T09:31:00Z")).toBe("12 Jan 2026");
    expect(formatDate("not-a-date")).toBe("");
  });
});
