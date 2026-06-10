/**
 * Typed command wrappers. The ONLY place `invoke` is called for app
 * commands — tests mock `@tauri-apps/api/core` and drive these.
 *
 * Image bytes never cross IPC (DECISIONS P16): previews load via the
 * photoproof:// protocol URLs from `urls.ts`.
 */
import { invoke } from "@tauri-apps/api/core";
import type {
  AppSettings,
  ExportReportDto,
  FolderNode,
  GridItem,
  ImageMetadataDto,
  ImagePathsDto,
  IndicatorState,
  IngestStatus,
  JournalEntryDto,
  RebuildReportDto,
  RedactReportDto,
  RootDto,
  RuntimeStatus,
  ScopeView,
} from "../types/dto";
import type { Filter, SearchResults } from "../types/search";

// -- scope & capture --------------------------------------------------------

/** Report the selection/view-derived target list; the core echoes the scope. */
export const setScope = (targets: string[]) =>
  invoke<ScopeView>("set_scope", { targets });

export const indicatorState = () => invoke<IndicatorState>("indicator_state");

/** Typed note bound to the current scope. Resolves true iff committed. */
export const addNote = (text: string) => invoke<boolean>("add_note", { text });

/** Rating key 0–5. Session scope = no-op (resolves false). */
export const setRating = (value: number) =>
  invoke<boolean>("set_rating", { value });

export const reportActivity = () => invoke<void>("report_activity");

// -- search (RETRIEVAL §4 / §5.4) -------------------------------------------

export const search = (query: string, filters: Filter[]) =>
  invoke<SearchResults>("search", { query, filters });

// -- roots & grid -----------------------------------------------------------

export const listRoots = () => invoke<RootDto[]>("list_roots");
export const addRoot = (path: string) => invoke<RootDto>("add_root", { path });
export const removeRoot = (rootId: string) =>
  invoke<void>("remove_root", { rootId });
export const folderTree = (rootId: string) =>
  invoke<FolderNode[]>("folder_tree", { rootId });
export const listFolder = (rootId: string, folder: string) =>
  invoke<GridItem[]>("list_folder", { rootId, folder });

// -- ingest / settings / export ---------------------------------------------

export const ingestStatus = () => invoke<IngestStatus>("ingest_status");
export const settingsGet = () => invoke<AppSettings>("settings_get");
export const runtimeStatus = () => invoke<RuntimeStatus>("runtime_status");
export const exportJournal = (dest: string) =>
  invoke<ExportReportDto>("export_journal", { dest });
export const rebuildIndex = () => invoke<RebuildReportDto>("rebuild_index");

// -- journal & metadata (Stage C bodies in commands/journal.rs) ---------------

/** Folded per-image journal, retracted rows included + flagged. */
export const imageJournal = (hash: string) =>
  invoke<JournalEntryDto[]>("image_journal", { hash });

/** Read-only EXIF subset for the Metadata tab (K16). */
export const imageMetadata = (hash: string) =>
  invoke<ImageMetadataDto>("image_metadata", { hash });

/** Inline correction → revision event (EVENTS fold: corrected text wins). */
export const reviseEvent = (eventId: string, text: string) =>
  invoke<boolean>("revise_event", { eventId, text });

/** Retract → tombstone; the sanctioned toast offers Undo. */
export const retractEvent = (eventId: string) =>
  invoke<boolean>("retract_event", { eventId });

/** The retract-toast "Undo": RE-STATE — appends a NEW remark carrying the
 * folded text (retraction-of-retraction is spec-forbidden, DECISIONS E4). */
export const unretractEvent = (eventId: string) =>
  invoke<boolean>("unretract_event", { eventId });

/** The one modal's commit: scrub everywhere (EVENTS; redaction wins). */
export const redactEvent = (eventId: string) =>
  invoke<RedactReportDto>("redact_event", { eventId });

// -- OS integration, D4 (Stage A bodies in commands/os.rs; no deletion — D3) --

export const imageAbsPath = (hash: string) =>
  invoke<ImagePathsDto>("image_abs_path", { hash });
export const revealInFileManager = (hash: string) =>
  invoke<void>("reveal_in_file_manager", { hash });
export const revealFolder = (rootId: string, folder: string) =>
  invoke<void>("reveal_folder", { rootId, folder });
export const openWithDefault = (hash: string) =>
  invoke<void>("open_with_default", { hash });

/** Rail-folder menu: rescan a watched root on demand. */
export const rescanRoot = (rootId: string) =>
  invoke<void>("rescan_root", { rootId });

// -- window plumbing ----------------------------------------------------------

export const openSettingsWindow = () => invoke<void>("open_settings_window");
export const quit = () => invoke<void>("quit");
