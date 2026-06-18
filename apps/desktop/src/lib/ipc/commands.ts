/**
 * Typed command wrappers. The ONLY place `invoke` is called for app
 * commands — tests mock `@tauri-apps/api/core` and drive these.
 *
 * Image bytes never cross IPC (DECISIONS P16): previews load via the
 * photoproof:// protocol URLs from `urls.ts`.
 */
import { invoke } from "@tauri-apps/api/core";
import type {
  AddRootOutcome,
  AppSettings,
  CollectionCandidateDto,
  CollectionDto,
  CollectionNoteDto,
  DiversifyReport,
  DuplicateGroupDto,
  ExportReportDto,
  FolderNode,
  GridItem,
  ImageMetadataDto,
  ImageNeighbors,
  ImagePathsDto,
  IndicatorState,
  IngestStatus,
  JournalEntryDto,
  PreviewCacheStatsDto,
  RankedImageDto,
  RebuildReportDto,
  RedactReportDto,
  RootDto,
  RuntimeStatus,
  ScopeSubject,
  ScopeView,
  StrokeCommitDto,
  StrokePayloadWire,
  TopicDto,
  TopicNoteDto,
} from "../types/dto";
import type { Filter, FusionWeights, SearchResults } from "../types/search";

// -- scope & capture --------------------------------------------------------

/** Report the selection/view-derived target list and, DESIGN-VOICE-SUBJECTS.md,
 * an OPTIONAL non-image subject (a collection/topic whose note log dictation
 * lands in when no image is focused); the core echoes the scope. The two are
 * mutually exclusive (image targets always win), so `subject` is sent only
 * when `targets` is empty. */
export const setScope = (targets: string[], subject?: ScopeSubject | null) =>
  invoke<ScopeView>("set_scope", { targets, subject: subject ?? null });

export const indicatorState = () => invoke<IndicatorState>("indicator_state");

/** Mic toggle (CAPTURE §6.4) — the TAP/pointer form: flip whatever state
 * the mic is in; echoes the §11 indicator (a not-ready ASR lands
 * `disarmedError` quietly). */
export const toggleMic = () => invoke<IndicatorState>("toggle_mic");

/** Explicit mic intent (the M two-gesture ruling): push-to-talk drives
 * idempotent arm/disarm, never toggle — a PTT release racing a device-
 * failure disarm (§6.6) must end DISARMED, where a blind toggle could
 * re-arm. Already in the wanted state, the call just echoes the
 * indicator. */
export const setMic = (armed: boolean) =>
  invoke<IndicatorState>("set_mic", { armed });

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

/** Which search lane to run (M3 search-as-scope, Phase 1). `lexical` forces
 * the M1 keyword rig even on a warm machine — the <100 ms as-you-type lane;
 * `semantic` runs the full hybrid rig (commit on Enter). Omitting it keeps
 * today's auto behavior (the backend default), so non-scope callers are
 * unchanged. */
export type SearchMode = "lexical" | "semantic";

/** Optional semantic-lane tuning (search-as-scope Phase 3 — the ⚙ "Ranking
 * signals" popover). `weights` overrides the fusion's per-signal weights (an
 * unchecked signal arrives as 0.0, excluded); `includeDebug` lights up
 * `ImageResult.debug` so the popover can SHOW each result's per-signal
 * contribution. Both are SEMANTIC-LANE ONLY — the lexical keystroke path never
 * passes them, so the <100 ms budget is untouched. Omitting both preserves
 * today's behavior exactly (the backend takes its own default fusion). */
export interface SearchTuning {
  weights?: FusionWeights;
  includeDebug?: boolean;
}

export const search = (
  query: string,
  filters: Filter[],
  mode?: SearchMode,
  tuning?: SearchTuning,
  fuzzy?: boolean,
) => {
  // Build the invoke payload sparsely: an absent field is omitted entirely so
  // a plain (mode-less, untuned) call is byte-identical to the old wire — the
  // Rust args are all Option, and a missing key deserializes to None.
  const args: Record<string, unknown> = { query, filters };
  if (mode !== undefined) args.mode = mode;
  if (tuning?.weights !== undefined) args.weights = tuning.weights;
  if (tuning?.includeDebug === true) args.includeDebug = true;
  // The `~` fuzzy quiet-toggle (Phase 4): the lexical lane's additive
  // typo-tolerant camera/lens/filename widening. Sent only when armed, so an
  // unarmed call stays byte-identical to today; the backend honors it on the
  // lexical lane only, never the semantic/commit path.
  if (fuzzy === true) args.fuzzy = true;
  return invoke<SearchResults>("search", args);
};

/** Grid rows for an explicit hash list, IN THE ORDER GIVEN (M3): the query
 * grid runs `search` for fused-order result hashes, then this enriches them
 * into the same badge-bearing GridItems the folder/collection grids use.
 * Hashes the library never indexed are silently skipped (nothing to show). */
export const listImages = (hashes: string[]) =>
  invoke<GridItem[]>("list_images", { hashes });

/** "More like this" (B69 retrieval-stays-additive): the nearest OTHER images
 * to `hash` by visual (CLIP) similarity, in descending-similarity order with
 * the query image excluded. Returns just the ranked hashes — the `similar`
 * grid scope feeds them to `listImages` exactly like the query scope feeds
 * fused-order hashes. An un-embedded image or empty index resolves to `[]`
 * (never an error), so the mechanism is correct on a fresh/mock machine. */
export const findSimilar = (hash: string, limit?: number) =>
  invoke<string[]>(
    "find_similar",
    limit === undefined ? { hash } : { hash, limit },
  );

// -- semantic topic-graph (DESIGN-SEMANTIC-GRAPH.md) ------------------------

/** The grid scope the graph lens is pointed at — a folder, a collection, the
 * WHOLE library (the deliberate scale spike), or an EXPLICIT hash list (a
 * SEARCH RESULT: a committed query / "more like this" / a ranked topic, which
 * has no folder/collection/library noun to name). Field names are snake_case to
 * match the Rust `GraphScope` (serde tag = "kind", snake_case). The backend
 * filters a `hashes` scope down to images that still exist in the library, so
 * a stale grid hash never enters a scan. */
export type GraphScope =
  | { kind: "folder"; root_id: string; folder: string }
  | { kind: "collection"; id: string }
  | { kind: "library" }
  | { kind: "hashes"; hashes: string[] };

/** One image's blended affinity to one topic anchor (snake_case twin of the
 * Rust `TopicScore` — same convention as the search `ImageResult`). */
export interface TopicScore {
  topic: number;
  affinity: number;
}
export interface ImageAffinities {
  image_hash: string;
  scores: TopicScore[];
}
/** `topic_affinities` result + the honesty flags the UI surfaces (so a degraded
 * rig is VISIBLE, not a silent all-centered blob). */
export interface AffinityReport {
  images: ImageAffinities[];
  visual_ready: boolean;
  annotation_ready: boolean;
}
/** A suggested topic for the rail — a mined note n-gram or a collection name. */
export interface TopicSuggestion {
  phrase: string;
  source: "note" | "collection";
  count: number;
}
/** A note-grounded auto-topic (v2): one labeled k-means cluster of the scope's
 * image vectors. Twin of the Rust `ClusterTopic`. */
export interface ClusterTopic {
  label: string;
  size: number;
  centroid_affinity: number;
}
/** The v3 LLM-suggestion seam state (twin of the Rust `LlmSuggestions`). Until
 * the Gemma connector is wired this is always `unavailable` and the UI shows the
 * cluster + n-gram suggestions instead. */
export type LlmSuggestions =
  | { state: "unavailable"; reason: string }
  | { state: "ready"; topics: string[] };
/** The force-sim physics knobs + v2 LOD/cluster knobs (twin of the Rust
 * `GraphTuning`). */
export interface GraphTuning {
  alpha_default: number;
  attraction: number;
  repulsion: number;
  /** Semantic-neighbor spring (CLIP/note similarity): how hard alike photos draw
   * together, and the spring's rest length (px). The balance vs `attraction`
   * decides "congregate around topics" vs "clump by similarity". */
  neighbor_attraction: number;
  neighbor_rest_length: number;
  damping: number;
  centering: number;
  ring_radius: number;
  /** Force-placed anchors (founder dogfood): anchor centroid attraction +
   * mutual anchor repulsion + the anchors' own damping. */
  anchor_attraction: number;
  anchor_repulsion: number;
  anchor_damping: number;
  /** v2 cluster auto-labels: k-means k bounds. */
  cluster_k_min: number;
  cluster_k_max: number;
  /** v2 full-library LOD: aggregate into super-nodes past this node count. */
  lod_threshold: number;
}

/** Per-image blended affinity to each topic anchor (looks vs said, blend
 * `alpha`; omitted ⇒ the GraphTuning default). Never errors on a degraded rig —
 * a fresh/un-embedded machine resolves to a zeros report. */
export const topicAffinities = (
  scope: GraphScope,
  topics: string[],
  alpha?: number,
) => {
  const args: Record<string, unknown> = { scope, topics };
  if (alpha !== undefined) args.alpha = alpha;
  return invoke<AffinityReport>("topic_affinities", args);
};

/** Cheap candidate topics for the suggestion rail (note n-grams + collection
 * names). v1: no LLM, no clustering. */
export const suggestTopics = (scope: GraphScope) =>
  invoke<TopicSuggestion[]>("suggest_topics", { scope });

/** v2 note-grounded auto-topics: cluster the in-scope image vectors and label
 * each cluster by its most representative note phrase. `k` omitted picks from
 * the scope size. Never errors on a degraded rig (empty/un-embedded resolves to
 * an empty rail). */
export const clusterTopics = (scope: GraphScope, k?: number) => {
  const args: Record<string, unknown> = { scope };
  if (k !== undefined) args.k = k;
  return invoke<ClusterTopic[]>("cluster_topics", args);
};

/** v3 SEAM: LLM-suggested theme topics. The Gemma connector is not wired yet
 * (mocked in M1), so this resolves to `{ state: "unavailable" }` and the rail
 * degrades to the cluster + n-gram suggestions. Real themes appear only once the
 * connector lands. */
export const suggestTopicsLlm = (scope: GraphScope) =>
  invoke<LlmSuggestions>("suggest_topics_llm", { scope });

/** The sparse semantic k-NN graph the Visualizer's force layout reads to pull
 * CLIP/note-similar photos together (semantic-neighbor attraction). Each
 * in-scope image carries its top-`k` most-similar neighbors (default k = 6),
 * each with a cosine `weight` (roughly [0,1], higher = more alike). `scope` is
 * the same GraphScope shape `topicAffinities`/`clusterTopics` use; neighbors
 * depend ONLY on scope (not on the topic set or alpha). Never errors on a
 * degraded rig — an un-embedded scope resolves to `[]` (the layout then runs on
 * topic attraction alone). */
export const graphNeighbors = (scope: GraphScope, k?: number) => {
  const args: Record<string, unknown> = { scope };
  if (k !== undefined) args.k = k;
  return invoke<ImageNeighbors[]>("graph_neighbors", args);
};

/** The GraphTuning knobs the force sim + alpha slider read. */
export const graphTuning = () => invoke<GraphTuning>("graph_tuning");

// -- diversify / duplication-tolerance (DESIGN-DEDUP-AND-SIMILARITY.md) ------

/** The duplication-tolerance "Diversify" view filter: for a scope and a single
 * `tolerance ∈ [0,1]` (0 = show everything, higher = hide more), return WHICH
 * in-scope images are representatives (`shown`) and which are redundant
 * (`hidden`) — `shown ∪ hidden` is exactly the scope. Reuses the SAME GraphScope
 * shape `topicAffinities`/`graphNeighbors` use (the diversify set matches the
 * grid exactly), running greedy facility-location over the CLIP cosine k-NN
 * graph. NEVER errors on a degraded rig: an un-embedded scope / missing CLIP
 * model resolves to an "all shown" report flagged `degraded` (the UI then
 * disables the slider and shows a calm hint), the same graceful posture as the
 * other lens commands. */
export const diversifyScope = (scope: GraphScope, tolerance: number) =>
  invoke<DiversifyReport>("diversify_scope", { scope, tolerance });
// -- near-duplicate detection (DESIGN-DEDUP-AND-SIMILARITY.md "Tier 1") ------

/** Tier-1 perceptual-hash near-dup GROUPS in a scope. The backend reuses the
 * graph lens' `GraphScope` + `enumerate_scope` verbatim, so the "Duplicates"
 * lens resolves to the SAME image set the grid/graph already show for a folder /
 * collection / the whole library — `ui.graphScope()` feeds this exactly like the
 * graph commands. Returns the transitive near-dup clusters (each size >= 2)
 * whose 64-bit perceptual hashes are within the Hamming threshold.
 *
 * `hammingThreshold` is OPTIONAL: omitted, the backend uses the empirically
 * calibrated `[dedup].hamming_threshold` (default 8/64) the design doc insists
 * must be tuned, NOT a magic number on the wire. A caller passes an explicit
 * value to sweep tighter/looser live (the looseness slider). Sent sparsely so an
 * untuned call is byte-identical to the default. GRACEFUL: an un-hashed scope
 * (previews still pending) or an empty scope resolves to `[]`, never an error,
 * so the lens is correct on a fresh/mock machine. */
export const findNearDuplicates = (
  scope: GraphScope,
  hammingThreshold?: number,
) => {
  const args: Record<string, unknown> = { scope };
  if (hammingThreshold !== undefined) args.hammingThreshold = hammingThreshold;
  return invoke<DuplicateGroupDto[]>("find_near_duplicates", args);
};

// -- roots & grid -----------------------------------------------------------

export const listRoots = () => invoke<RootDto[]>("list_roots");
export const addRoot = (path: string) =>
  invoke<AddRootOutcome>("add_root", { path });
export const removeRoot = (rootId: string) =>
  invoke<void>("remove_root", { rootId });
// Archive lifecycle (folder-tree improvements): non-destructive hide/restore
// of a root, plus the archived snapshot for the rail's "Archived" affordance.
export const archiveRoot = (rootId: string) =>
  invoke<void>("archive_root", { rootId });
export const unarchiveRoot = (rootId: string) =>
  invoke<RootDto>("unarchive_root", { rootId });
export const listArchivedRoots = () =>
  invoke<RootDto[]>("list_archived_roots");
export const folderTree = (rootId: string) =>
  invoke<FolderNode[]>("folder_tree", { rootId });
export const listFolder = (rootId: string, folder: string) =>
  invoke<GridItem[]>("list_folder", { rootId, folder });

// -- collections (RETRIEVAL §10, B71 — rail Collections tab, P7.3 store) -----

export const listCollections = () => invoke<CollectionDto[]>("list_collections");
export const createCollection = (name: string) =>
  invoke<CollectionDto>("create_collection", { name });
/** Open membership intervals for every hash not already a member; resolves
 * to the count of NEWLY added members (mutations emit `collections-changed`). */
export const addToCollection = (id: string, hashes: string[]) =>
  invoke<number>("add_to_collection", { id, hashes });
/** Close open membership intervals — evented, never destructive (§10.1);
 * resolves to the count of members actually removed. */
export const removeFromCollection = (id: string, hashes: string[]) =>
  invoke<number>("remove_from_collection", { id, hashes });
/** Collections the image is CURRENTLY a member of — feeds the thumb
 * menu's membership checkmarks and the Remove-from-collection submenu. */
export const collectionsForImage = (hash: string) =>
  invoke<CollectionDto[]>("collections_for_image", { hash });
/** Current members as grid rows — clicking a collection in the rail shows
 * its members in the grid, exactly as folder selection drives it. */
export const listCollectionMembers = (id: string) =>
  invoke<GridItem[]>("list_collection_members", { id });
/** The collection's append-only notes, chronological (P7.3 store). A
 * collection note records the GROUPING's intent, never a single image —
 * the sibling of the per-image journal, read in the rail's collection
 * view. */
export const collectionNotes = (id: string) =>
  invoke<CollectionNoteDto[]>("collection_notes", { id });
/** Append a note to the collection (append-only — no edit/delete; the
 * journal's event model for the grouping). Resolves to the freshly
 * appended note; mutations emit `collections-changed`. */
export const addCollectionNote = (id: string, text: string) =>
  invoke<CollectionNoteDto>("add_collection_note", { id, text });

// -- topics + the topic→collection bake (DESIGN-TOPICS-COLLECTIONS.md) -------
//
// A topic is a SAVED PHRASE (like a saved search): its images are ALWAYS
// computed affinity (topicRankedImages), never stored membership. The bake is a
// ONE-WAY commit of a topic threshold (or a client-computed selection) into an
// ordinary evented collection. The cluster-derived autosuggested topics come
// from `clusterTopics` above (the graph lens shares this topic set).

/** Save a phrase as a manual topic. `space`: "annotation" | "clip" pins one
 * embedding space; omitted blends both at the configured alpha (the default). */
export const addTopic = (phrase: string, space?: string) =>
  invoke<TopicDto>("add_topic", { phrase, space });
/** Every saved manual topic, newest first. */
export const listTopics = () => invoke<TopicDto[]>("list_topics");
/** Remove a saved topic by id (errors if already gone). */
export const removeTopic = (id: string) =>
  invoke<void>("remove_topic", { id });
/** A topic's append-only notes, chronological (the collection-notes mirror).
 * A topic note is the user's authored text ABOUT the topic (its definition,
 * what it is for, the refinement intent), keyed to the topic id. */
export const topicNotes = (id: string) =>
  invoke<TopicNoteDto[]>("topic_notes", { id });
/** Append a note to the topic (append-only — no edit/delete, the collection
 * note's event model mirrored for topics). Resolves to the freshly appended
 * note; the rail reloads its note log on demand (no snapshot event). */
export const addTopicNote = (id: string, text: string) =>
  invoke<TopicNoteDto>("add_topic_note", { id, text });
/** The in-scope images ranked by blended affinity to one topic phrase,
 * descending — the Topics-tab grid; the slider thresholds on `score`. `alpha`
 * omitted uses the graph default. Graceful: an un-embedded scope returns []. */
export const topicRankedImages = (
  phrase: string,
  scope: GraphScope,
  alpha?: number,
) => {
  const args: Record<string, unknown> = { phrase, scope };
  if (alpha !== undefined) args.alpha = alpha;
  return invoke<RankedImageDto[]>("topic_ranked_images", args);
};
/** Bake a topic threshold into a collection: members are the in-scope images
 * scoring >= `threshold`. Provenance (phrase + threshold + alpha) is recorded
 * in the collection description. ONE-WAY: the result is an ordinary independent
 * collection (no live link back). Emits `collections-changed`. */
export const createCollectionFromTopic = (
  phrase: string,
  scope: GraphScope,
  threshold: number,
  name: string,
  alpha?: number,
) => {
  const args: Record<string, unknown> = { phrase, scope, threshold, name };
  if (alpha !== undefined) args.alpha = alpha;
  return invoke<CollectionDto>("create_collection_from_topic", args);
};
/** The generic bake the graph's lasso/slider uses when the selection is already
 * computed client-side: commit the given hashes into an evented collection
 * (provenance "from topic graph selection"). Emits `collections-changed`. */
export const createCollectionFromSelection = (hashes: string[], name: string) =>
  invoke<CollectionDto>("create_collection_from_selection", { hashes, name });
/** Autosuggest Phase 3 (DESIGN-TOPICS-COLLECTIONS.md): propose candidate
 * GROUPINGS the human might bake into collections, computed on the fly from
 * existing signals (co-annotation in a session, repeated note phrases, time +
 * folder bursts). K14: it PROPOSES, never auto-creates. Graceful: an
 * empty/sparse scope returns []. */
export const suggestCollections = (scope: GraphScope) =>
  invoke<CollectionCandidateDto[]>("suggest_collections", { scope });

// -- ingest / settings / export ---------------------------------------------

export const ingestStatus = () => invoke<IngestStatus>("ingest_status");
export const settingsGet = () => invoke<AppSettings>("settings_get");
/** Settings → "Stacked pairs show": persists and emits `settings-changed`
 * to every window (the main grid re-pairs live). */
export const setStackDisplay = (display: "jpeg" | "raw") =>
  invoke<AppSettings>("set_stack_display", { display });
/** Settings → "Open in external editor" target (BACKLOG "Configurable
 * external editor, D4 revisit"): persists and emits `settings-changed`.
 * Empty/whitespace clears the pref back to the OS default handler. */
export const setExternalEditor = (editor: string) =>
  invoke<AppSettings>("set_external_editor", { editor });
/** Settings → Previews: set the 1:1 preview cache budget in BYTES
 * (DESIGN-PREVIEW-POLICY.md). Persists and immediately re-evicts so a lowered
 * budget reclaims disk now. Returns the updated settings. */
export const setPreviewCacheBudget = (bytes: number) =>
  invoke<AppSettings>("set_preview_cache_budget", { bytes });
/** Settings → Previews cache-size readout: current 1:1 cache size + count,
 * total previews footprint, and the configured budget. */
export const previewCacheStats = () =>
  invoke<PreviewCacheStatsDto>("preview_cache_stats");
/** Settings → Previews "Clear 1:1 cache" / "Clear all previews". `kind` =
 * "full" (just the 1:1 tier) | "all" (1:1 + display + thumb). SAFE — every
 * removed artifact re-derives on next view. Returns files removed. */
export const clearPreviewCache = (kind: "full" | "all") =>
  invoke<number>("clear_preview_cache", { kind });
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
/** "Open in external editor" (BACKLOG "Configurable external editor, D4
 * revisit"): hand the original off to the configured editor, or the OS
 * default handler when the pref is empty. */
export const openInExternalEditor = (hash: string) =>
  invoke<void>("open_in_external_editor", { hash });

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

/** On-demand full RAW develop trigger (OD-1): Look calls this when its
 * resolution ladder reaches a RAW with no cached full-decode artifact. Resolves
 * to `true` if a develop is now pending (show "developing..." and retry the
 * /full-decode URL), `false` if the cache already had it (serve immediately) or
 * the hash is not a RAW. The pump develops in the background; the developed
 * artifact lands and `previews-changed` fires. */
export const requestFullDecode = (hash: string) =>
  invoke<boolean>("request_full_decode", { hash });

/** Viewport-first preview generation (OD-2): the grid sends the hashes the
 * user is currently scrolled to (visible + a small look-ahead) whose thumbnail
 * is not yet generated, so the pump generates those previews ahead of the
 * offscreen backfill it would otherwise grind through in scan order. Bumps only
 * PENDING preview rows to the top interactive priority; resolves to how many
 * rows were promoted. Image bytes never cross IPC: only hashes go out, a count
 * comes back. Complements the client-side `thumbqueue` LOAD ordering (this
 * reorders SERVER generation; that reorders the draw of already-made thumbs). */
export const prioritizePreviews = (hashes: string[]) =>
  invoke<number>("prioritize_previews", { hashes });

// -- attention/engagement heatmap (DESIGN-ATTENTION-HEATMAP.md) ---------------

/** Which focus tier a dwell episode was captured at. "look" = single-image
 * view (full weight); "grid" = grid select / multi-select (a small fraction).
 * The tier RATE and the 60 s per-episode cap live in the backend (tuning is
 * authoritative); the frontend reports only the raw episode. */
export type DwellSource = "look" | "grid";

/** Report one finished focus episode (DESIGN §"dwell capture"). Fire-and-
 * forget: capture is light and a dropped report just loses a little dwell. The
 * backend applies the tier rate + 60 s cap and accumulates into image_dwell. */
export const recordDwell = (hash: string, source: DwellSource, elapsedMs: number) =>
  invoke<void>("record_dwell", { hash, source, elapsedMs });

/** One image's normalized (0..1) engagement intensity in a scope. */
export interface ImageIntensity {
  hash: string;
  intensity: number;
}

/** Per-scope, normalized engagement intensity (DESIGN §"Rendering"). `allTime`
 * false (default) is recency-weighted ("what am I working on now"); true is flat
 * all-time ("what mattered most ever"). One entry per input hash, in order. */
export const imageIntensity = (hashes: string[], allTime: boolean) =>
  invoke<ImageIntensity[]>("image_intensity", { hashes, allTime });

/** Settings → "Clear attention data": wipe the local dwell telemetry (the
 * privacy-hygiene reset). Annotation counts are the user's journal, untouched.
 * Resolves to the number of per-image rows removed. */
export const clearDwell = () => invoke<number>("clear_dwell");

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
