/**
 * Search-result rendering (spec/UI.md §5, RETRIEVAL §4/§5.4/§6) as pure
 * functions producing text RUNS — never HTML strings. The component renders
 * runs as text nodes inside <mark>/<span>, so nothing the backend sends can
 * inject markup ("never raw HTML through IPC").
 *
 * Quotes are the user's own words, verbatim, match-highlighted; they are
 * never paraphrased (R3). Every provenance variant renders to something a
 * human can read (RETRIEVAL §6).
 */
import type { Provenance, Quote } from "../types/search";

export interface Run {
  text: string;
  marked: boolean;
}

const OPEN = "⟦";
const CLOSE = "⟧";

/**
 * Sentinel mapping: FTS5 snippets arrive with `⟦match⟧` delimiters
 * (RETRIEVAL §4); they become marked runs. Unbalanced sentinels degrade to
 * literal text rather than guessing.
 */
export function sentinelRuns(text: string): Run[] {
  const runs: Run[] = [];
  let rest = text;
  for (;;) {
    const open = rest.indexOf(OPEN);
    if (open < 0) break;
    const close = rest.indexOf(CLOSE, open + OPEN.length);
    if (close < 0) break; // unbalanced: stop parsing, emit remainder verbatim
    if (open > 0) runs.push({ text: rest.slice(0, open), marked: false });
    runs.push({
      text: rest.slice(open + OPEN.length, close),
      marked: true,
    });
    rest = rest.slice(close + CLOSE.length);
  }
  if (rest.length > 0) runs.push({ text: rest, marked: false });
  return runs.filter((r) => r.text.length > 0);
}

/** Range mapping: explicit highlight ranges within the quote text. */
export function rangeRuns(text: string, highlights: [number, number][]): Run[] {
  const runs: Run[] = [];
  // Clamp, drop invalid, sort, merge overlaps.
  const sorted = highlights
    .map(([s, e]) => [Math.max(0, s), Math.min(text.length, e)] as [number, number])
    .filter(([s, e]) => s < e)
    .sort((a, b) => a[0] - b[0]);
  const merged: [number, number][] = [];
  for (const r of sorted) {
    const last = merged[merged.length - 1];
    if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
    else merged.push([...r] as [number, number]);
  }
  let pos = 0;
  for (const [s, e] of merged) {
    if (s > pos) runs.push({ text: text.slice(pos, s), marked: false });
    runs.push({ text: text.slice(s, e), marked: true });
    pos = e;
  }
  if (pos < text.length) runs.push({ text: text.slice(pos), marked: false });
  return runs.filter((r) => r.text.length > 0);
}

/** A quote's display runs: sentinels when present, else highlight ranges. */
export function quoteRuns(q: Quote): Run[] {
  if (q.text.includes(OPEN) && q.text.includes(CLOSE)) {
    return sentinelRuns(q.text);
  }
  return rangeRuns(q.text, q.highlights);
}

/** "12 Jan 2026" (UI §5.1 layout sketch). */
export function formatDate(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return "";
  const months = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];
  return `${d.getUTCDate()} ${months[d.getUTCMonth()]} ${d.getUTCFullYear()}`;
}

export type ProvenanceDisplay =
  | { kind: "quote"; runs: Run[]; dateLabel: string; source: "voice" | "typed" }
  | { kind: "stroke"; dateLabel: string }
  | { kind: "visual-match" }
  | { kind: "filter-only" }
  | { kind: "fuzzy-meta"; field: "camera" | "lens" | "filename" };

/**
 * RETRIEVAL §6: FTS/vector hit → verbatim quote + date; stroke-only →
 * stroke reference; clip-only → labeled "visual match", honestly, never a
 * generated caption; filter-only → "matches your filters"; fuzzy-meta → an
 * honest "approximate" label naming the metadata field that fuzzily matched
 * (the `~` quiet-toggle, Phase 4) so a widened hit never poses as an exact one.
 */
export function provenanceDisplay(p: Provenance): ProvenanceDisplay {
  switch (p.type) {
    case "quote":
      return {
        kind: "quote",
        runs: quoteRuns(p),
        dateLabel: formatDate(p.ts),
        source: p.source,
      };
    case "stroke":
      return { kind: "stroke", dateLabel: formatDate(p.ts) };
    case "visual_match":
      return { kind: "visual-match" };
    case "filter_only":
      return { kind: "filter-only" };
    case "fuzzy_meta":
      return { kind: "fuzzy-meta", field: p.field };
  }
}

/** The `~` fuzzy quiet-toggle's approximate-match label (Phase 4): names the
 * metadata field that fuzzily matched, honestly flagged as approximate. */
export function fuzzyMetaLabel(field: "camera" | "lens" | "filename"): string {
  return `approximate ${field} match`;
}

/** UI §5.2: the single dimmed zero-result line. Nothing else. */
export const ZERO_RESULTS_LINE = "Nothing in your journal matches.";

/** Provenance labels (UI renders quotes verbatim; these are the non-quote labels). */
export const VISUAL_MATCH_LABEL = "visual match";
export const FILTER_ONLY_LABEL = "matches your filters";
