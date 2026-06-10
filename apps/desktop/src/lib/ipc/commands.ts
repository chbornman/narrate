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
  IndicatorState,
  IngestStatus,
  RebuildReportDto,
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

// -- window plumbing ----------------------------------------------------------

export const openSettingsWindow = () => invoke<void>("open_settings_window");
export const quit = () => invoke<void>("quit");
