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
