/** IPC DTOs (camelCase) — twins of `src-tauri/src/dto.rs`. */

export interface RootDto {
  rootId: string;
  displayName: string;
  relPath: string;
  volumeId: string;
  online: boolean;
  absPath: string | null;
}

export interface FolderNode {
  name: string;
  relPath: string;
  children: FolderNode[];
}

export interface GridItem {
  hash: string;
  fileName: string;
  relPath: string;
  /** Root the representative path sits under — stack pair identity
   * includes it (logic/stacks.ts): a COLLECTION grid mixes roots (B71),
   * and identical camera paths across two roots are unrelated images
   * that must never collapse into one cell. Optional so test fixtures
   * predate it; null = the path row has no root. */
  rootId?: string | null;
  captureTs: string | null;
  addedTs: string;
  hasJournal: boolean;
  /** Folded rating — DATA only; never rendered on thumbnails (UI §3.5). */
  rating: number | null;
  offline: boolean;
  /** A thumb artifact exists in the cache: the grid requests the protocol
   * URL only when true — mid-scan on a network volume, eager requests are
   * thousands of doomed 404 round-trips (founder, SMB, June 2026).
   * Optional so test fixtures predate it; absent = true (request as
   * before — the retry/heal machinery still backstops a 404). */
  previewReady?: boolean;
}

export type ScopeKind = "single" | "multi" | "session";

export interface ScopeView {
  kind: ScopeKind;
  count: number;
  previewHashes: string[];
}

export interface IndicatorState {
  currentScope: ScopeView;
  mic: "disarmed" | "arming" | "armedIdle" | "armedSpeaking" | "disarmedError";
  streamingUtterance: { boundScope: ScopeView; startedAt: string } | null;
  degraded: { asrUnavailable: boolean };
}

export interface IndicatorPulse {
  eventKind: string;
}

/** `journal-changed` payload: the images whose journal truth a committed
 * mutation touched. Open surfaces (journal panel, grid badges, the Look
 * overlay) refresh from this — the seam M2b voice events will ride. */
export interface JournalChanged {
  hashes: string[];
}

/** `previews-changed` payload: the images whose preview artifacts landed
 * in an ingest drain. Thumbs that exhausted their 404 retry budget heal
 * off this instead of staying blank until restart. */
export interface PreviewsChanged {
  hashes: string[];
}

export interface IngestStatus {
  running: boolean;
  done: number;
  total: number;
  errors: number;
  /** Pass kinds with work still queued (pending + running), queue-spelled
   * names, deterministic order — the header jobs pill's hover breakdown
   * (BACKLOG "Header shows background jobs"). */
  passes: { name: string; remaining: number }[];
  /** A filesystem walk (add-root initial scan / rescan) is in flight. Pass
   * counters lag the walk — rows only appear at hash time — so this is
   * what keeps `running` true through the walk's dark window (founder,
   * June 2026: "No photographs" over a folder busily being scanned). */
  scanning: boolean;
  /** Files discovered so far by in-flight walks (after exclusions) — the
   * empty grid's quiet "N photographs found so far…" line. */
  discovered: number;
}

/** RUNTIME (P6.2): tier + consent + per-model license/progress rows.
 * `asrReady`/`llmReady` are the §8.3 readiness gates — false until a
 * supervised child reports Ready (never true before P6.3 vendors real
 * binaries); features light up individually and silently as they flip. */
export interface RuntimeStatus {
  asrReady: boolean;
  llmReady: boolean;
  /** Plan says Run but the binary could not be resolved (e.g. a dev
   * target prune ate pp-asr-server — founder incident, June 2026): the
   * human reason. Distinct from `asrReady === false`, which also covers
   * the normal silent warm-up; blocked means it will NEVER flip ready
   * until the binary returns. null = not blocked. */
  asrBlocked: string | null;
  llmBlocked: string | null;
  /** P7.4 §3.3: in-process embedder readiness — true once the ort sessions
   * are constructed. Additive; like asr/llm they light up silently and gate
   * the semantic-search backfill, never blocking the journal. */
  clipReady: boolean;
  textEmbedderReady: boolean;
  tierDetected: number;
  /** After the always-winning user override (§6.2). */
  tierEffective: number;
  /** Overriding ABOVE detected hardware: the one-time plain warning. */
  tierOverriddenAbove: boolean;
  /** "undecided" | "later" | "never" | "download" (§10.3). */
  consent: string;
  /** Live manifest byte sum at the effective tier (§5.4). */
  consentOfferBytes: number;
  models: ModelRowDto[];
  instanceLockHeld: boolean;
}

export interface ModelRowDto {
  id: string;
  role: string;
  /** "not-offered" | "not-downloaded" | "downloading" | "installed" | "failed". */
  state: string;
  totalBytes: number;
  /** Model-cumulative: bytes of this model on disk, never per-file. */
  downloadedBytes: number;
  licenseName: string;
  licenseUrl: string;
  acceptanceRequired: boolean;
  accepted: boolean;
  error: string | null;
  /**
   * Set while an interrupted transfer is auto-retrying: the row stays
   * "downloading" (error is terminal, written only after the retry
   * schedule is exhausted) and this names the retry in flight.
   */
  retryHint: string | null;
}

export interface AppSettings {
  lastExportTs: string | null;
  /** Which member a collapsed RAW+JPEG stack displays (featureset §5
   * dogfood amendment; backend settings.rs StackDisplay twin). */
  stackDisplay: "jpeg" | "raw";
  /** Configurable external editor (BACKLOG "Configurable external editor,
   * D4 revisit"): the app name (macOS) / executable (Win/Linux) the
   * "Open in external editor" verb hands the original off to. null = the
   * OS default handler (settings.rs external_editor twin). */
  externalEditor: string | null;
}

export interface ExportReportDto {
  dir: string;
  manifestPath: string;
  images: number;
  events: number;
  sessions: number;
}

export interface RebuildReportDto {
  filesScanned: number;
  filesParsed: number;
  failures: number;
}

// ---------------------------------------------------------------------------
// P4.2 additions (contracts frozen by FOUNDATIONS; bodies land with their
// stages: journal/metadata — Stage C, paths/OS — Stage A).
// ---------------------------------------------------------------------------

/** Integer [x, y, p, t] stroke sample (EVENTS §3.3 wire form: x/y in
 * ten-thousandths of the display-oriented extent −2500..12500, p per-mille
 * with 1000 = device reports none, t = ms offset from pen-down). */
export type StrokeWirePoint = [number, number, number, number];

/** CAPTURE §8.2 stroke payload — `add_stroke`'s input (P5.1). Canonical
 * integers only; the Rust side validates, core re-validates on append. */
export interface StrokePayloadWire {
  baseW: number;
  orientation: number;
  points: StrokeWirePoint[];
  tool: "pencil";
}

/** `add_stroke`'s output: the minted event id plus the session it landed
 * in. The pencil undo stack is session-scoped (CAPTURE §8.5, DECISIONS C4
 * "this-session only"); session closure is lazy, so the echoed session id
 * is how the frontend observes a rotation and clears the stack. */
export interface StrokeCommitDto {
  id: string;
  sessionId: string;
}

/** Stroke geometry riding a journal row (the Look overlay and the journal
 * micro-previews render from this; `pencil` is the only tool in v1). */
export interface StrokeDto {
  baseW: number;
  orientation: number;
  points: StrokeWirePoint[];
}

/** One folded journal row (inspector Journal tab — featureset §3, D2).
 * Revisions/retractions never appear standalone (EVENTS folds); retracted
 * rows ARE included, flagged, for the per-session "show retracted" toggle. */
export interface JournalEntryDto {
  id: string;
  sessionId: string;
  /** RFC 3339. */
  ts: string;
  kind: "remark" | "rating" | "stroke" | "redacted";
  source: "voice" | "typed" | "system";
  /** Effective (folded) text for remarks; null for ratings/stubs. */
  text: string | null;
  /** Pre-revision original when corrected ("edited" expand affordance). */
  originalText: string | null;
  corrected: boolean;
  retracted: boolean;
  rating: number | null;
  targets: string[];
  linkedEvent: string | null;
  /** Stroke rows only; null/absent elsewhere (and on scrubbed strokes). */
  stroke?: StrokeDto | null;
}

/** Read-only EXIF subset + file identity (Metadata tab, K16 stands —
 * from the db's EXIF subset; no new parsing). */
export interface ImageMetadataDto {
  hash: string;
  fileName: string;
  relPath: string;
  absPath: string | null;
  byteSize: number;
  format: string;
  pixelWidth: number | null;
  pixelHeight: number | null;
  orientation: number;
  captureTs: string | null;
  cameraMake: string | null;
  cameraModel: string | null;
  lensModel: string | null;
  focalLengthMm: number | null;
  iso: number | null;
  fNumber: number | null;
  exposureTime: string | null;
  /** Formatted GPS text (UI renders text only). */
  gps: string | null;
  previewSource: string | null;
  /** Preview backfill still pending (e.g. RAW full-decode). */
  previewPending: boolean;
  firstIngestedAt: string;
}

/** redact_event outcome — drives the sanctioned "Redacted" toast copy,
 * including "— N offline sidecar(s) pending" (UI §7.5/§8.4). */
export interface RedactReportDto {
  /** Event ids scrubbed (target + revision chain). */
  redacted: string[];
  sidecarsUpdated: number;
  /** Labels of offline volumes whose sidecars are scrubbed on next mount. */
  offlinePending: string[];
}

/** One collection (RETRIEVAL §10, B71) as the rail's Collections tab and
 * the thumb menu's Add-to-collection submenu render it. Counts cover
 * CURRENT members (open intervals) only. Snapshots arrive whole on the
 * `collections-changed` event — the frontend never reconciles deltas. */
export interface CollectionDto {
  id: string;
  name: string;
  description: string;
  /** "active" | "shelved" | "done" — shelved/done rows render dimmed. */
  status: string;
  createdTs: string;
  updatedTs: string;
  memberCount: number;
  noteCount: number;
}

/** One append-only collection note (RETRIEVAL §10, B71; P7.3 store). A
 * collection note is about the GROUPING's intent (why these images are
 * together), a DELIBERATELY separate kind from per-image journal events —
 * it never targets a single image. Append-only, like the journal: no edit,
 * no delete (K14: the record preserves the user's own words). */
export interface CollectionNoteDto {
  id: string;
  /** RFC 3339. */
  ts: string;
  text: string;
}

/** image_abs_path result (D4: reveal / copy path / open-default). */
export interface ImagePathsDto {
  /** Best online absolute path, null when every path is offline. */
  absPath: string | null;
  relPath: string;
  volumeLabel: string | null;
  online: boolean;
}
