//! IPC data-transfer types (camelCase on the wire). These are the shell's own
//! DTOs; the search result contract lives in `search_types` and mirrors
//! spec/RETRIEVAL.md §5.4 field-for-field instead.

use serde::{Deserialize, Serialize};

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
    /// Archived (lifecycle): hidden from the active rail list, kept whole.
    /// `list_roots` only ever returns active roots (false here); the
    /// `list_archived_roots` snapshot carries the archived ones (true).
    pub archived: bool,
}

/// Outcome of an `add_root` attempt (folder-tree improvements: refuse +
/// alias). A clean add returns `Added`; a folder overlapping an existing
/// active root returns `Overlap` carrying that root's id so the rail can
/// NAVIGATE to it instead of double-ingesting — the founder's rule that
/// folders are a thin physical substrate, never duplicated.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AddRootOutcome {
    Added { root: RootDto },
    Overlap { existing_root_id: String },
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
    /// Root the representative path sits under. The grid's stack pairing
    /// keys on it: a COLLECTION grid mixes roots (B71), and identical
    /// camera paths (DCIM/100CANON/IMG_0001.*) across two roots are
    /// unrelated photographs that must never collapse into one cell.
    pub root_id: Option<String>,
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
    /// A thumb artifact exists — the grid requests the protocol URL only
    /// when true (no doomed 404 round-trips mid-scan; `previews-changed`
    /// flips cells live as artifacts land).
    pub preview_ready: bool,
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

/// `journal-changed` payload (BACKLOG): the images whose journal truth a
/// committed mutation touched. Open surfaces refresh from this — required
/// before M2b voice events land without UI actions. No text content ever
/// rides this channel either.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalChanged {
    pub hashes: Vec<String>,
}

/// `previews-changed` payload: the images whose preview artifacts landed
/// in an ingest drain. Thumbs that exhausted their 404 retry budget heal
/// off this (the journal-changed seam, applied to previews) — without it,
/// any ingest outlasting the retry cap left permanent blanks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewsChanged {
    pub hashes: Vec<String>,
}

/// Settings → Previews cache-size readout (DESIGN-PREVIEW-POLICY.md). Bytes on
/// the wire (the UI formats GB/MB); `fullBytes`/`fullFiles` are the budgeted
/// 1:1 tier, `totalBytes` is the whole previews footprint (1:1 + display +
/// thumb). The configured budget rides along so the UI can show "X of Y GB"
/// without a second round-trip.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCacheStatsDto {
    pub full_bytes: u64,
    pub full_files: u64,
    pub total_bytes: u64,
    pub budget_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IngestStatus {
    pub running: bool,
    /// Completed work units (done + skipped) across all passes.
    pub done: u64,
    /// All known work units across all passes.
    pub total: u64,
    pub errors: u64,
    /// Pass kinds with work still queued (BACKLOG "Header shows
    /// background jobs"): the header pill's hover breakdown. Names as the
    /// queue spells them; versions of the same pass are summed — a
    /// version bump re-running a pass is the same KIND of work to the
    /// reviewer. Deterministic (BTreeMap) order so the pump's `!=`
    /// change detection never sees a phantom reorder.
    pub passes: Vec<PassRemaining>,
    /// A filesystem walk (add-root initial scan / rescan) is in flight.
    /// Pass rows only materialize at hash time, so the counters above read
    /// idle for the WHOLE walk of a slow volume — this flag is what keeps
    /// `running` honest through that window (founder, June 2026: "No
    /// photographs" shown over a folder busily being scanned).
    pub scanning: bool,
    /// Files discovered so far by in-flight walks (after exclusions) —
    /// the empty grid's quiet "N photographs found so far…" line.
    pub discovered: u64,
}

/// One still-digesting pass kind: `remaining` = pending + running rows.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PassRemaining {
    pub name: String,
    pub remaining: u64,
}

/// RUNTIME contract seam (P6.2 fills this in; M1 is the degraded mode that
/// is exactly the full journal product — DECISIONS K15).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    /// §8.3 readiness gates: false until the supervised child reports
    /// Ready (never true in P6.2 — real binaries are the P6.3 spike).
    pub asr_ready: bool,
    pub llm_ready: bool,
    /// Plan says Run but the binary could not be resolved (founder
    /// incident, June 2026: a dev target prune ate pp-asr-server and the
    /// mic key went dark with zero surfacing). Some(reason) means the
    /// process will NEVER become ready until the binary returns — distinct
    /// from `asr_ready == false`, which also covers the normal silent
    /// warm-up. None = not blocked. Settings shows the reason quietly.
    pub asr_blocked: Option<String>,
    pub llm_blocked: Option<String>,
    /// P7.4 §3.3: the in-process embedder readiness — true once the ort
    /// sessions are constructed (additive; settings rows show running/idle
    /// state text). False keeps search keyword-only and the backfill dark.
    pub clip_ready: bool,
    pub text_embedder_ready: bool,
    pub tier_detected: u8,
    /// After the `[runtime] tier` override — it always wins (§6.2).
    pub tier_effective: u8,
    /// Overriding ABOVE detected shows the one-time plain warning.
    pub tier_overridden_above: bool,
    /// "undecided" | "later" | "never" | "download" (§10.3: no download
    /// without explicit consent; Never is remembered; Later re-offers
    /// from settings only).
    pub consent: String,
    /// The live manifest byte sum at the effective tier (§5.4: the
    /// consent card always shows the live sum).
    pub consent_offer_bytes: u64,
    pub models: Vec<ModelRow>,
    pub instance_lock_held: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub id: String,
    pub role: String,
    /// "not-offered" | "not-downloaded" | "downloading" | "installed" |
    /// "failed" (§2.4 settings states).
    pub state: String,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    /// §5.3: license display + acceptance state; texts/links viewable in
    /// settings.
    pub license_name: String,
    pub license_url: String,
    pub acceptance_required: bool,
    pub accepted: bool,
    /// Download failure detail surfaced in settings/debug (§5.2).
    pub error: Option<String>,
    /// Present while the worker auto-retries an interrupted transfer:
    /// the row stays "downloading" (a cut is weather, not a verdict —
    /// `error` is written only when the retry schedule is exhausted) and
    /// this names the in-progress retry for settings to show.
    pub retry_hint: Option<String>,
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

// ---------------------------------------------------------------------------
// P4.2 additions — twins of types/dto.ts, declared contracts-first by
// FOUNDATIONS. Constructed by Stage C (journal/metadata) and Stage A
// (paths); the dead_code allowance retires with their command bodies.
// ---------------------------------------------------------------------------

/// One folded journal row (inspector Journal tab — featureset §3, D2).
/// Retracted rows ARE included, flagged, for the per-session "show
/// retracted" toggle; redacted events fold to inert stubs.
#[allow(dead_code)] // constructed by Stage C (commands/journal.rs)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntryDto {
    pub id: String,
    pub session_id: String,
    /// RFC 3339.
    pub ts: String,
    /// "remark" | "rating" | "stroke" | "redacted".
    pub kind: &'static str,
    /// "voice" | "typed" | "system".
    pub source: &'static str,
    /// Effective (folded) text for remarks; None for ratings/stubs.
    pub text: Option<String>,
    /// Pre-revision original when corrected ("edited" expand affordance).
    pub original_text: Option<String>,
    pub corrected: bool,
    pub retracted: bool,
    pub rating: Option<u8>,
    pub targets: Vec<String>,
    pub linked_event: Option<String>,
    /// Stroke rows only; None elsewhere (and on scrubbed strokes).
    pub stroke: Option<StrokeDto>,
}

/// Stroke geometry riding a journal row (P5.1: the Look overlay and the
/// journal micro-previews render from this). Wire form is EVENTS §3.3's
/// integer `[x, y, p, t]` tuples.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrokeDto {
    pub base_w: u32,
    pub orientation: u8,
    pub points: Vec<[i64; 4]>,
}

/// `add_stroke` input (CAPTURE §8.2, P5.1) — canonical integers only.
/// `stroke_payload` (commands/capture.rs) re-checks every range for honest
/// error messages; core's `append` re-validates the converted payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StrokePayloadDto {
    pub base_w: u32,
    pub orientation: u8,
    pub points: Vec<[i64; 4]>,
    pub tool: String,
}

/// `add_stroke` output (P5.1): the minted event id plus the session it
/// landed in. The pencil undo stack is session-scoped (CAPTURE §8.5,
/// DECISIONS C4 "this-session only") and session closure is LAZY — the
/// 30-minute boundary applies at the next activity touch (CAPTURE §2.2) —
/// so the echoed session id is how the frontend observes a rotation and
/// clears the stack.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrokeCommitDto {
    pub id: String,
    pub session_id: String,
}

/// Read-only EXIF subset + file identity (Metadata tab, K16: from the db's
/// EXIF subset; no new parsing).
#[allow(dead_code)] // constructed by Stage C (commands/journal.rs)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageMetadataDto {
    pub hash: String,
    pub file_name: String,
    pub rel_path: String,
    pub abs_path: Option<String>,
    pub byte_size: i64,
    pub format: String,
    pub pixel_width: Option<i64>,
    pub pixel_height: Option<i64>,
    pub orientation: u16,
    pub capture_ts: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub iso: Option<i64>,
    pub f_number: Option<f64>,
    pub exposure_time: Option<String>,
    /// Formatted GPS text (the UI renders text only).
    pub gps: Option<String>,
    pub preview_source: Option<String>,
    /// Preview backfill still pending (e.g. RAW full-decode).
    pub preview_pending: bool,
    pub first_ingested_at: String,
}

/// `redact_event` outcome — drives the sanctioned "Redacted" toast copy,
/// including "— N offline sidecar(s) pending" (UI §7.5/§8.4).
#[allow(dead_code)] // constructed by Stage C (commands/journal.rs)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactReportDto {
    /// Event ids scrubbed (target + revision chain).
    pub redacted: Vec<String>,
    pub sidecars_updated: usize,
    /// Labels of offline volumes whose sidecars are scrubbed on next mount.
    pub offline_pending: Vec<String>,
}

/// `image_abs_path` result (D4: reveal / copy path / open-default).
#[allow(dead_code)] // constructed by Stage A (commands/os.rs)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagePathsDto {
    /// Best online absolute path; None when every path is offline.
    pub abs_path: Option<String>,
    pub rel_path: String,
    pub volume_label: Option<String>,
    pub online: bool,
}

/// One collection (RETRIEVAL §10, B71) as the rail tab and the Look's
/// membership row render it. Counts cover CURRENT members (open intervals)
/// only; history stays a core-level read until a surface needs it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDto {
    pub id: String,
    pub name: String,
    pub description: String,
    /// "active" | "shelved" | "done".
    pub status: String,
    pub created_ts: String,
    pub updated_ts: String,
    pub member_count: usize,
    pub note_count: usize,
}

/// One collection note (RETRIEVAL §10.1: append-only, never edited or
/// deleted) as the rail tab's notes pane renders it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionNoteDto {
    pub id: String,
    pub ts: String,
    pub text: String,
}

/// One saved manual topic (DESIGN-TOPICS-COLLECTIONS.md) as the Topics rail
/// tab renders it. A topic is a saved phrase, like a saved search; its images
/// are always computed affinity (`topic_ranked_images`), never stored here.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicDto {
    pub id: String,
    pub phrase: String,
    /// "blend" (both spaces at the configured alpha — the default) | "annotation"
    /// (what you SAID) | "clip" (what it LOOKS like).
    pub space: String,
    pub created_ts: String,
}

/// One in-scope image and its blended affinity to a selected topic phrase, for
/// the Topics tab's ranked grid and the threshold slider. Descending `score`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RankedImageDto {
    pub hash: String,
    /// Blended affinity (a cosine, roughly [-1, 1]); the slider thresholds on it.
    pub score: f32,
}
