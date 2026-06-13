//! The search IPC contract — a field-for-field mirror of
//! spec/RETRIEVAL.md §5.4 (result contract) and §5.1 (the Filter JSON the
//! M1 chips construct). Field names stay snake_case as specced; enums are
//! `type`-discriminated exactly like RETRIEVAL §5.1's JSON encoding.
//!
//! TODO(coordinator): wire `photoproof_core::search` — when packet P3.1
//! lands, the `search` command body (commands.rs) calls the real engine and
//! maps its types onto these DTOs (or replaces them with core re-exports if
//! the engine derives Serialize). The TS twins live in
//! `src/lib/types/search.ts` and must stay in lockstep.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct SearchResults {
    pub query: QueryEcho,
    /// Fused order (M1: bm25 grouped per image — P3.1).
    pub images: Vec<ImageResult>,
    /// Session-level remark matches; never attributed to images.
    pub session_hits: Vec<SessionHit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryEcho {
    pub raw: String,
    /// Echo of the executed filters (M1: the chips, verbatim).
    pub filters: Vec<Filter>,
    /// §5.1: clauses the validation firewall dropped (debug panel only).
    pub dropped: Vec<DroppedClause>,
    /// §5.1: parse fell back to whole-query FTS.
    pub fallback: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DroppedClause {
    pub raw: serde_json::Value,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageResult {
    /// blake3 hex — also the preview-protocol key.
    pub image_hash: String,
    /// PreviewRef (LIBRARY cache key): the hash the `photoproof://` scheme
    /// serves; the UI builds the URL, bytes never cross IPC.
    pub preview: String,
    /// Fused RRF score (M1: normalized bm25 rank score).
    pub score: f32,
    /// REQUIRED, never absent (RETRIEVAL §6).
    pub provenance: Provenance,
    pub last_annotated_ts: Option<String>,
    /// Dev builds only; release stays None.
    pub debug: Option<DebugScores>,
}

/// §5.4 `Provenance` — every variant renders to human-readable text.
/// Constructed by the P3.1 engine once wired; the stub returns no results,
/// so the variants are "unused" until then.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Provenance {
    /// The BEST matching span of the user's own words, verbatim.
    Quote(Quote),
    /// Stroke-only evidence.
    Stroke {
        event_id: String,
        session_id: String,
        ts: String,
    },
    /// image_clip evidence only (M3) — labeled honestly, NO fake quote.
    VisualMatch,
    /// Pure structured-filter query.
    FilterOnly,
    /// Fuzzy (typo-tolerant) metadata match from the `~` quiet-toggle
    /// (search-as-scope Phase 4): a camera/lens/filename widening, appended
    /// BELOW the exact set. Labeled honestly as approximate so the UI never
    /// presents a widened gear/filename match as an exact one.
    FuzzyMeta {
        /// "camera" | "lens" | "filename" — which field fuzzily matched.
        field: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    pub event_id: String,
    pub session_id: String,
    pub ts: String,
    /// "voice" | "typed".
    pub source: String,
    /// Exact folded-text span. May carry FTS `⟦⟧` sentinels, which the UI
    /// maps to `<mark>`; never raw HTML through IPC (RETRIEVAL §4).
    pub text: String,
    pub char_start: u32,
    pub char_end: u32,
    /// Matched-term ranges within `text`.
    pub highlights: Vec<(u32, u32)>,
    pub linked_stroke: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionHit {
    pub session_id: String,
    pub quote: Quote,
}

/// Per-signal fusion weights from the ⚙ "Ranking signals" popover
/// (search-as-scope Phase 3 — making the B75 weights user-visible).
///
/// An optional payload on the `search` command: omitted entirely (the common
/// case) preserves today's behavior exactly. When present, each field is the
/// weight for one signal (S1/S2/S3-each/S4); the on/off checkboxes ship as
/// either the B75 default (checked) or `0.0` (unchecked, excluded from the
/// fusion). Continuous values are accepted on the wire so the eval-gated
/// sliders (Phase 4) need no contract change, but only the SEMANTIC lane reads
/// them — the lexical keystroke path never carries weights.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct FusionWeightsWire {
    pub s1: f64,
    pub s2: f64,
    pub s3_each: f64,
    pub s4: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugScores {
    pub per_signal: Vec<(String, Option<u32>, f32)>,
    pub fused: f32,
}

// ---------------------------------------------------------------------------
// Filters — RETRIEVAL §5.1's JSON encoding, constructed directly by M1 chips
// ("Chips construct Filter values directly — the same AST the M3 parser
// emits, so M1 filter execution IS the M3 code path").
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Filter {
    Date {
        /// "captured" | "annotated"; default captured.
        field: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        absolute: Option<AbsoluteRange>,
        #[serde(skip_serializing_if = "Option::is_none")]
        relative: Option<RelativeRange>,
    },
    Camera {
        value: String,
    },
    Lens {
        value: String,
    },
    Folder {
        value: String,
    },
    Root {
        value: String,
    },
    Rating {
        /// "eq" | "gte" | "lte".
        op: String,
        value: u8,
    },
    Collection {
        name: String,
    },
    Volume {
        /// "online" | "offline".
        value: String,
    },
    HasStrokes {
        value: bool,
    },
    Source {
        /// "voice" | "typed" | "pencil".
        values: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbsoluteRange {
    pub start: Option<String>,
    pub end: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelativeRange {
    /// "days" | "weeks" | "months" | "years" | "season" | "month" | "year".
    pub unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
}

/// An empty, well-formed, contract-shaped result (shape regression tests).
#[cfg(test)]
pub fn empty_results(raw: String, filters: Vec<Filter>) -> SearchResults {
    SearchResults {
        query: QueryEcho {
            raw,
            filters,
            dropped: Vec::new(),
            fallback: false,
        },
        images: Vec::new(),
        session_hits: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_serializes_type_discriminated_snake_case() {
        let q = Provenance::Quote(Quote {
            event_id: "01H".into(),
            session_id: "01S".into(),
            ts: "2026-01-14T21:08:11Z".into(),
            source: "voice".into(),
            text: "the ⟦fog⟧ swallowing the ⟦ba⟧rn".into(),
            char_start: 0,
            char_end: 31,
            highlights: vec![(4, 7)],
            linked_stroke: None,
        });
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(v["type"], "quote");
        assert_eq!(v["source"], "voice");
        assert_eq!(v["char_start"], 0);
        let s = serde_json::to_value(Provenance::VisualMatch).unwrap();
        assert_eq!(s["type"], "visual_match");
        let f = serde_json::to_value(Provenance::FilterOnly).unwrap();
        assert_eq!(f["type"], "filter_only");
    }

    #[test]
    fn filters_deserialize_the_retrieval_5_1_json_shapes() {
        let chips: Vec<Filter> = serde_json::from_str(
            r#"[
              {"type":"date","field":"annotated","relative":{"unit":"season","season":"winter","n":1}},
              {"type":"rating","op":"gte","value":3},
              {"type":"has_strokes","value":true},
              {"type":"camera","value":"X-T5"},
              {"type":"volume","value":"offline"},
              {"type":"collection","name":"Quiet Hours"},
              {"type":"source","values":["voice","typed"]}
            ]"#,
        )
        .unwrap();
        assert_eq!(chips.len(), 7);
        // B71 made "collection" the canonical wire tag; "project" is no
        // longer part of the section 5.1 grammar and must not deserialize.
        assert!(
            serde_json::from_str::<Filter>(r#"{"type":"project","name":"Quiet Hours"}"#).is_err()
        );
        assert_eq!(
            chips[5],
            Filter::Collection {
                name: "Quiet Hours".into()
            }
        );
        assert_eq!(
            chips[1],
            Filter::Rating {
                op: "gte".into(),
                value: 3
            }
        );
        assert_eq!(chips[2], Filter::HasStrokes { value: true });
    }

    #[test]
    fn empty_results_echo_the_query_and_chips() {
        let r = empty_results(
            "fog ba".into(),
            vec![Filter::Rating {
                op: "gte".into(),
                value: 3,
            }],
        );
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["query"]["raw"], "fog ba");
        assert_eq!(v["query"]["fallback"], false);
        assert_eq!(v["images"].as_array().unwrap().len(), 0);
        assert_eq!(v["session_hits"].as_array().unwrap().len(), 0);
        assert_eq!(v["query"]["filters"][0]["type"], "rating");
    }
}
