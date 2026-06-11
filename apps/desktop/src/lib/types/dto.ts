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
  captureTs: string | null;
  addedTs: string;
  hasJournal: boolean;
  /** Folded rating — DATA only; never rendered on thumbnails (UI §3.5). */
  rating: number | null;
  offline: boolean;
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

export interface IngestStatus {
  running: boolean;
  done: number;
  total: number;
  errors: number;
}

/** RUNTIME (P6.2): tier + consent + per-model license/progress rows.
 * `asrReady`/`llmReady` are the §8.3 readiness gates — false until a
 * supervised child reports Ready (never true before P6.3 vendors real
 * binaries); features light up individually and silently as they flip. */
export interface RuntimeStatus {
  asrReady: boolean;
  llmReady: boolean;
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
  downloadedBytes: number;
  licenseName: string;
  licenseUrl: string;
  acceptanceRequired: boolean;
  accepted: boolean;
  error: string | null;
}

export interface AppSettings {
  lastExportTs: string | null;
  /** Which member a collapsed RAW+JPEG stack displays (featureset §5
   * dogfood amendment; backend settings.rs StackDisplay twin). */
  stackDisplay: "jpeg" | "raw";
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

/** image_abs_path result (D4: reveal / copy path / open-default). */
export interface ImagePathsDto {
  /** Best online absolute path, null when every path is offline. */
  absPath: string | null;
  relPath: string;
  volumeLabel: string | null;
  online: boolean;
}
