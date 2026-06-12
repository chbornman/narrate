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
  StrokeCommitDto,
  StrokePayloadWire,
} from "../types/dto";
import type { Filter, SearchResults } from "../types/search";

// -- scope & capture --------------------------------------------------------

/** Report the selection/view-derived target list; the core echoes the scope. */
export const setScope = (targets: string[]) =>
  invoke<ScopeView>("set_scope", { targets });

export const indicatorState = () => invoke<IndicatorState>("indicator_state");

/** M-key toggle (CAPTURE §6.4): arm/disarm the mic; echoes the §11
 * indicator (a not-ready ASR lands `disarmedError` quietly). */
export const toggleMic = () => invoke<IndicatorState>("toggle_mic");

/** Typed note bound to the current scope — or, `target` given, to that
 * single image (the journal-panel composer's explicit binding: the panel's
 * image, never the grid write-scope). Resolves true iff committed. */
export const addNote = (text: string, target?: string) =>
  invoke<boolean>("add_note", target === undefined ? { text } : { text, target });

/** Rating key 0–5. Session scope = no-op (resolves false). `target` is the
 * journal-panel composer's explicit single-image binding (always rates). */
export const setRating = (value: number, target?: string) =>
  invoke<boolean>("set_rating", target === undefined ? { value } : { value, target });

/** Activity touch (CAPTURE §2.1). Resolves to the CURRENT (post-touch)
 * session id: session closure is lazy (§2.2), so this echo is how the
 * frontend observes a rotation — the session-scoped pencil undo stack
 * clears against it (§8.5). */
export const reportActivity = () => invoke<string>("report_activity");

/** Grease-pencil stroke (CAPTURE §8): minted at pen-up, bound to the single
 * VIEWED image — never the scope ring buffer — and committed UNLINKED
 * (DECISIONS C5; P6.1 resolves links). Resolves to the event id plus the
 * session it landed in — both feed the session-scoped undo stack (§8.5). */
export const addStroke = (hash: string, payload: StrokePayloadWire) =>
  invoke<StrokeCommitDto>("add_stroke", { hash, payload });

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
/** Settings → "Stacked pairs show": persists and emits `settings-changed`
 * to every window (the main grid re-pairs live). */
export const setStackDisplay = (display: "jpeg" | "raw") =>
  invoke<AppSettings>("set_stack_display", { display });
export const runtimeStatus = () => invoke<RuntimeStatus>("runtime_status");
/** §10.2–10.3: the ONE consent decision — Download now / Later / Never.
 * No download starts without it; Never is remembered; skipping changes
 * nothing about journaling. */
export const runtimeConsent = (decision: "download" | "later" | "never") =>
  invoke<RuntimeStatus>("runtime_consent", { decision });
/** §5.3: record a per-model license acceptance (id + url + timestamp). */
export const runtimeAcceptLicense = (modelId: string) =>
  invoke<RuntimeStatus>("runtime_accept_license", { modelId });
export const runtimeDownloadModel = (modelId: string) =>
  invoke<RuntimeStatus>("runtime_download_model", { modelId });
export const runtimeRemoveModel = (modelId: string) =>
  invoke<RuntimeStatus>("runtime_remove_model", { modelId });
/** Settings → "restart runtime" (§8.1: fresh attempt budget). */
export const runtimeRestart = () => invoke<RuntimeStatus>("runtime_restart");
/** Settings → re-detect hardware (§6.1.4). */
export const runtimeRedetect = () => invoke<RuntimeStatus>("runtime_redetect");
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

/** Rail-folder menu: "Rebuild previews…" — re-pend the preview pass for
 * every image under the root (the recovery verb SEPARATE from Rescan;
 * BACKLOG, founder dogfood round 3). Resolves to the re-pend count; the
 * pump regenerates in the background and thumbs heal off
 * `previews-changed`. */
export const rebuildPreviews = (rootId: string) =>
  invoke<number>("rebuild_previews", { rootId });

// -- window plumbing ----------------------------------------------------------

export const openSettingsWindow = () => invoke<void>("open_settings_window");
/** macOS Tab lights-out (featureset §0 "hides ALL chrome"): the traffic
 * lights are NATIVE NSButtons (Overlay titlebar), outside the DOM region
 * gates — left visible they float over (and click-block) the chrome-less
 * grid. Hidden/shown in lockstep with chromeHidden; never persisted.
 * No-op off macOS. */
export const setTrafficLightsHidden = (hidden: boolean) =>
  invoke<void>("set_traffic_lights_hidden", { hidden });
export const quit = () => invoke<void>("quit");
