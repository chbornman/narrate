/**
 * The search IPC contract — TS twin of `src-tauri/src/search_types.rs`,
 * mirroring spec/RETRIEVAL.md §5.4 (result contract) and §5.1 (Filter JSON).
 * Field names stay snake_case as specced.
 *
 * TODO(coordinator): when photoproof_core::search (P3.1) is wired into the
 * `search` command, these types are the contract the UI already renders —
 * nothing here should need to change.
 */

export type ContentHash = string;

export interface SearchResults {
  query: QueryEcho;
  /** Fused order. */
  images: ImageResult[];
  /** Session-level remark matches; never attributed to images. */
  session_hits: SessionHit[];
}

export interface QueryEcho {
  raw: string;
  filters: Filter[];
  dropped: DroppedClause[];
  fallback: boolean;
}

export interface DroppedClause {
  raw: unknown;
  reason: string;
}

export interface ImageResult {
  image_hash: ContentHash;
  /** PreviewRef: the cache key the photoproof:// protocol serves. */
  preview: string;
  /** Fused RRF score. */
  score: number;
  /** REQUIRED, never absent (RETRIEVAL §6). */
  provenance: Provenance;
  last_annotated_ts: string | null;
  /** Dev builds only. */
  debug: DebugScores | null;
}

export interface Quote {
  event_id: string;
  session_id: string;
  ts: string;
  source: "voice" | "typed";
  /**
   * Exact folded-text span — the user's verbatim words. May carry FTS
   * `⟦⟧` sentinels, which the UI maps to <mark>; never raw HTML through
   * IPC (RETRIEVAL §4).
   */
  text: string;
  char_start: number;
  char_end: number;
  /** Matched-term ranges within `text`. */
  highlights: [number, number][];
  linked_stroke: string | null;
}

export type Provenance =
  | ({ type: "quote" } & Quote)
  | { type: "stroke"; event_id: string; session_id: string; ts: string }
  | { type: "visual_match" }
  | { type: "filter_only" };

export interface SessionHit {
  session_id: string;
  quote: Quote;
}

export interface DebugScores {
  per_signal: [string, number | null, number][];
  fused: number;
}

// ---------------------------------------------------------------------------
// Filters — RETRIEVAL §5.1 JSON shapes; M1 chips construct these directly.
// ---------------------------------------------------------------------------

export interface AbsoluteRange {
  start: string | null;
  end: string | null;
}

export interface RelativeRange {
  unit: "days" | "weeks" | "months" | "years" | "season" | "month" | "year";
  n?: number;
  season?: "spring" | "summer" | "autumn" | "winter";
  month?: number;
  year?: number;
}

export type Filter =
  | {
      type: "date";
      field: "captured" | "annotated";
      absolute?: AbsoluteRange;
      relative?: RelativeRange;
    }
  | { type: "camera"; value: string }
  | { type: "lens"; value: string }
  | { type: "folder"; value: string }
  | { type: "root"; value: string }
  | { type: "rating"; op: "eq" | "gte" | "lte"; value: number }
  | { type: "project"; name: string }
  | { type: "volume"; value: "online" | "offline" }
  | { type: "has_strokes"; value: boolean }
  | { type: "source"; values: ("voice" | "typed" | "pencil")[] };
