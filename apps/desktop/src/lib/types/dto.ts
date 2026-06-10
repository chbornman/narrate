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

export interface IngestStatus {
  running: boolean;
  done: number;
  total: number;
  errors: number;
}

export interface RuntimeStatus {
  asrReady: boolean;
  hardwareTier: string | null;
  models: { name: string; sizeBytes: number; state: string }[];
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
