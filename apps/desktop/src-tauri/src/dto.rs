//! IPC data-transfer types (camelCase on the wire). These are the shell's own
//! DTOs; the search result contract lives in `search_types` and mirrors
//! spec/RETRIEVAL.md §5.4 field-for-field instead.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootDto {
    pub root_id: String,
    pub display_name: String,
    pub rel_path: String,
    pub volume_id: String,
    pub online: bool,
    /// Absolute directory when the volume is online.
    pub abs_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderNode {
    pub name: String,
    /// Root-relative folder path ("" = the root itself).
    pub rel_path: String,
    pub children: Vec<FolderNode>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GridItem {
    /// Content hash, lowercase hex — the image identity and the preview
    /// cache key (thumbnails load via the `photoproof://` protocol, never
    /// over IPC — DECISIONS P16).
    pub hash: String,
    pub file_name: String,
    /// Root-relative path of the file.
    pub rel_path: String,
    /// EXIF capture timestamp (RFC 3339) when known.
    pub capture_ts: Option<String>,
    /// First-ingested timestamp (RFC 3339) — the "date added" sort key.
    pub added_ts: String,
    /// Has-journal dot (spec/UI.md §3.5): derived `image_journal_stats`.
    pub has_journal: bool,
    /// Folded current rating (0..=5), absent = unrated.
    pub rating: Option<u8>,
    /// Every active path for this image sits on an offline volume (⏏ badge).
    pub offline: bool,
}

/// CAPTURE §11 `ScopeView`: what the indicator renders.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScopeView {
    /// "single" | "multi" | "session".
    pub kind: &'static str,
    pub count: usize,
    /// First ≤ 3 target hashes, for micro-thumbnails (CAPTURE §11).
    pub preview_hashes: Vec<String>,
}

/// CAPTURE §11 `IndicatorState`. M1: mic permanently disarmed, no streaming
/// utterance; the fields exist so the M2b wiring is additive.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndicatorState {
    pub current_scope: ScopeView,
    /// "disarmed" | "arming" | "armedIdle" | "armedSpeaking" | "disarmedError".
    pub mic: &'static str,
    pub streaming_utterance: Option<StreamingView>,
    pub degraded: DegradedFlags,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamingView {
    pub bound_scope: ScopeView,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DegradedFlags {
    pub asr_unavailable: bool,
}

/// Fire-and-forget pulse on every committed event (CAPTURE §11). No text
/// content ever rides this channel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndicatorPulse {
    pub event_kind: &'static str,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IngestStatus {
    pub running: bool,
    /// Completed work units (done + skipped) across all passes.
    pub done: u64,
    /// All known work units across all passes.
    pub total: u64,
    pub errors: u64,
}

/// RUNTIME contract seam (P6.2 fills this in; M1 is the degraded mode that
/// is exactly the full journal product — DECISIONS K15).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub asr_ready: bool,
    pub hardware_tier: Option<String>,
    pub models: Vec<ModelRow>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub name: String,
    pub size_bytes: u64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReportDto {
    pub dir: String,
    pub manifest_path: String,
    pub images: usize,
    pub events: usize,
    pub sessions: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuildReportDto {
    pub files_scanned: usize,
    pub files_parsed: usize,
    pub failures: usize,
}
