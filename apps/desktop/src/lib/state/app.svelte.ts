/**
 * Composition root (Svelte 5 runes): exports `ui` — the shell/grid/look/
 * inspector slices plus the search-bar state — the perform(Action) router,
 * and the actionContext() snapshot the keymap/menus/cheatsheet read. Slices
 * never import each other; CROSS-SLICE FLOWS LIVE ONLY HERE: openLook (entry
 * selection → LookEntry order via looknav.navigationSet), leaveLook (same
 * image active, flip-aware; the grid restores its own scroll anchor on
 * mount), goHome (G), auto-advance wiring (logic/advance.ts), the
 * inspector following the active image, the drag-folder drop-confirm
 * (featureset §6), and scope reporting (report, then render the echo —
 * UI §3.4; the backend owns scope semantics).
 *
 * SEARCH-AS-SCOPE (M3, Phase 1): the grid's image set is arbitrated by one
 * `gridScope` discriminated union — folder | collection | query — and four
 * `setItems` feeders (openFolder / openCollection / runQueryScope /
 * refreshItems), all guarded by the monotone `gridLoad` token. A committed
 * query is just a third scope; results render in place as ordinary grid
 * cells, so there is ONE selection system (grid.sel). The old overlay,
 * `searchSel`/`searchFocus`/`resultHashes`, and openLook's fromSearch branch
 * are retired.
 */
import * as ipc from "../ipc/commands";
import * as sel from "../logic/selection";
import * as note from "../logic/note";
import * as dedup from "../logic/dedup";
import * as topicbake from "../logic/topicbake";
import { isMac } from "../logic/platform";
import { escapeAction, type EscapeContext } from "../logic/escape";
import { navigationSet } from "../logic/looknav";
import {
  MIC_HOLD_IDLE,
  micBlur,
  micDown,
  micUp,
  type MicHoldState,
} from "../logic/michold";
import { scopeLabel, scopeSubject, scopeTargets } from "../logic/scope";
import { nextLane, type SearchLane } from "../logic/searchmode";
import {
  defaultToggles,
  isDefault,
  togglesToWeights,
  type SignalKey,
  type SignalToggles,
} from "../logic/ranking";
import { afterCommit } from "../logic/advance";
import {
  DEFAULT_TOLERANCE_PERCENT,
  hiddenCount,
  percentToTolerance,
} from "../logic/diversify";
import { invalidateScopedGraphs } from "../logic/graphstore";
import {
  collectionRows,
  filterTree,
  firstMatchKey,
  flatRows,
  moveFocus as railMoveFocus,
  sections,
  toggleExpand,
} from "../logic/sources";
import {
  IDLE_FLUSH_MS,
  beginEpisode,
  endEpisode,
  sameFocus,
  type DwellEpisode,
} from "../logic/dwell";
import type { DwellSource } from "../ipc/commands";
import type { Action } from "../logic/keymap";
import type { ActionContext, ViewMode } from "../actions/types";
import type {
  AddRootOutcome,
  AppSettings,
  CollectionDto,
  DuplicateGroupDto,
  FolderNode,
  IngestStatus,
  RootDto,
  StrokePayloadWire,
  TopicDto,
} from "../types/dto";
import type { Filter } from "../types/search";
import {
  DEDUP_THRESHOLD_DEFAULT,
  DIVERSIFY_DEBOUNCE_MS,
  MIN_QUERY_CHARS,
} from "../tuning";
import { copyKey, copyToClipboard } from "../primitives/copyflash.svelte";
import * as prefs from "./prefs";
import { ShellSlice } from "./shell.svelte";
import { GridSlice } from "./grid.svelte";
import { LookSlice } from "./look.svelte";
import { InspectorSlice } from "./inspector.svelte";

/** Mid-scan grid refresh cadence — ONE policy shared by both refresh
 * paths: the event-driven re-list throttle in onIngestProgress and
 * App.svelte's poll interval. 2 s keeps a slow network-volume scan
 * visibly streaming into the grid without hammering list_folder; a
 * shared export keeps the two paths from drifting apart silently. */
export const INGEST_RELIST_MS = 2_000;

/** Watchdog deadline for the optimistic `ingestExpecting` bridge
 * (shell.expectIngest). WHY this value: the bridge covers the dark window
 * between an add/rescan click and the pump's FIRST `ingest-progress` emit. A
 * silent no-op (deleted path, zero-change rescan) emits NO progress, so without
 * a deadline the flag — and the "Indexing…" empty state — strands forever
 * (AUDIT-FRONTEND-COUPLING A2 / STATE-MACHINE §6e). 8 s sits comfortably past a
 * slow network volume's walk-start dark window (the SMB scan that motivated the
 * bridge begins streaming well inside this) while still standing the lie down
 * fast enough that a stranded grid self-heals on its own — no restart needed.
 * If a real status DOES land, it cancels this timer (shell.clearIngestExpecting),
 * so a healthy scan never trips it. */
export const INGEST_EXPECT_TIMEOUT_MS = 8_000;

// MIN_QUERY_CHARS (the minimum free-text query length before a search runs)
// now lives in the centralized UI tuning module (lib/tuning.ts) — imported
// above. The gating policy it encodes is unchanged.

/**
 * What the grid is currently showing (M3 search-as-scope, Phase 1). The old
 * two-mode arbitration (a folder, OR a collection when `collectionId` is
 * non-null) generalizes into ONE discriminated union with a third `query`
 * variant — a committed search is now just another grid scope, rendered in
 * place as ordinary cells. `within` records the folder/collection the query
 * is scoped over so the bar can show the `within:` residue and one-key
 * clear returns there. (Phase 1 always scopes a query over the WHOLE
 * library; `within` carries the source the user returns to, not a backend
 * constraint — that lands with a later phase.)
 */
export type GridScope =
  | { kind: "folder"; rootId: string; folder: string }
  | { kind: "collection"; id: string }
  | { kind: "query"; query: string; chips: Filter[]; within: GridScope }
  // "More like this" (B69 retrieval-stays-additive): the grid shows the
  // visual neighbors of one image. A fourth scope variant rendered EXACTLY
  // like a query — ordinary cells in similarity (relevance) order — so it
  // reuses the query scope's whole feeder/residue/clear machinery. `hash` is
  // the query image; `filename` is captured at dispatch so the residue can
  // say "similar to <name>" without a later lookup; `within` records the
  // source one-key clear returns to (the query scope's `within` precedent).
  | { kind: "similar"; hash: string; filename: string; within: GridScope }
  // Topic scope (DESIGN-TOPICS-COLLECTIONS.md): selecting a topic in the
  // Topics rail tab scopes the grid to that topic's RANKED images (highest
  // blended affinity first). A FIFTH scope variant rendered exactly like
  // query/similar — ordinary cells in ranked (relevance) order — so it reuses
  // the same feeder/residue/clear machinery (the residue reads "topic:
  // <phrase>", one-key clear / Escape returns to `within`). A topic is fuzzy
  // (computed affinity, never stored membership); the bake gesture commits a
  // threshold of it into a durable collection (createCollectionFromTopic).
  //
  // `topicId` is the SAVED topic record this scope was opened from, when it
  // came from the Topics rail tab (so its append-only note log can surface,
  // mirroring how a `collection` scope carries the id its notes hang off).
  // Optional: a topic scope reached by phrase alone (a live re-rank with no
  // saved record, a restore from a phrase) carries no id and shows no notes.
  | { kind: "topic"; phrase: string; topicId?: string; within: GridScope };

export class Ui {
  // -- slices (contracts frozen by FOUNDATIONS) -------------------------------
  shell = new ShellSlice();
  grid = new GridSlice();
  look = new LookSlice();
  inspector = new InspectorSlice();

  // -- view mode: the center "lens" axis (DESIGN-VIEW-MODES.md) ----------------
  // ONE orthogonal axis — grid / visualizer / look — replacing the old
  // `surface: "grid" | "look"` enum PLUS the bolted-on `graphOpen` overlay
  // boolean. The visualizer is a PEER view now (it renders instead of the
  // grid, not over it), so adding a future `compare` view is additive (one
  // ViewMode token, one activeHash arm, one App.svelte arm) rather than
  // another boolean threaded through ~20 call sites. Orthogonal to
  // `gridScope` (the noun the grid shows), which this axis never touches.
  // Search is no longer a surface (M3 search-as-scope): a query scopes the
  // GRID in place. OPEN enum so the litmus in the doc holds.
  viewMode = $state<ViewMode>("grid");

  /** The image SELECTED in the visualizer (its hash), or null when nothing is
   * selected. A single click on an image node SELECTS it (glow + scope);
   * double-click / Enter then OPENS it in Look. The selection drives the write
   * scope while the visualizer is the active view (see reportScope + the
   * activeHash getter): a selected node is the dictation/rating target, null is
   * the NEUTRAL session scope. This lives in the composition root (not the
   * TopicGraph component) because it must outlive the lens' mount and gate
   * reportScope. Renamed from `graphSelection` — no longer graph-specific
   * (a future compare view could reuse it). */
  viewSelection = $state<string | null>(null);

  /** The three-state Attention OVERLAY on the graph (heatmap x graph synthesis):
   * "off" (the plain graph) / "engaged" (where attention lives) / "overlooked"
   * (coherent but cold). Persisted like the other graph + heatmap toggles. The
   * TopicGraph component owns the synthesis math + the intensity fetch (reusing
   * the heatmap `image_intensity` command); this flag just survives sessions and
   * is the one piece of overlay state the composition root holds. */
  graphAttention = $state<prefs.AttentionMode>("off");

  // -- roots & folder tree (shared by rail + grid) ----------------------------
  roots = $state<RootDto[]>([]);
  tree = $state<FolderNode[]>([]);
  /** Archived roots (folder-tree improvements): the rail's collapsed
   * "Archived" affordance lists these, restorable. Loaded lazily and
   * refreshed alongside the active snapshot. */
  archivedRoots = $state<RootDto[]>([]);
  /** Lazy deep-tree cap: folders deeper than this auto-collapse so a deep
   * root never renders its whole tree eagerly. A filter raises it past any
   * depth (every surviving match shows). The constant is a deliberate small
   * default; the user expands the branch they want. */
  static readonly AUTO_EXPAND_DEPTH = 2;

  // -- collections (B71 — rail Collections tab, P7.3 store) -------------------
  /** Full snapshot, backend list order; replaced whole on every
   * `collections-changed` event (never reconciled as deltas). */
  collections = $state<CollectionDto[]>([]);
  /** What the grid is showing (M3 search-as-scope): folder, collection, or a
   * committed query. The single arbiter the feeders (openFolder /
   * openCollection / runQueryScope / refreshItems) set and read. Boots into
   * an empty-folder scope; init() opens the real one. */
  gridScope = $state<GridScope>({ kind: "folder", rootId: "", folder: "" });

  /** Back-compat read of the old collection-mode flag: the rail's
   * current-selection highlight and the empty-collection copy still ask "is
   * a collection open?". A query or similar scope OVER a collection counts as
   * that collection being open (the residue still points there). A derived
   * scope's `within` is always a folder/collection (never another derived
   * scope), so one level of unwrap suffices. null otherwise. */
  collectionId = $derived<string | null>(
    this.gridScope.kind === "collection"
      ? this.gridScope.id
      : (this.gridScope.kind === "query" ||
            this.gridScope.kind === "similar" ||
            this.gridScope.kind === "topic") &&
          this.gridScope.within.kind === "collection"
        ? this.gridScope.within.id
        : null,
  );
  /** The SAVED topic whose note log is open in the rail, or null. Derived
   * from the topic gridScope's `topicId` exactly as `collectionId` is derived
   * from the collection scope, so leaving the topic scope (opening a folder,
   * a query, a collection) auto-hides the note pane with no extra cleanup.
   * Only the rail Topics tab opens a topic WITH its saved id; a phrase-only
   * topic scope (a graph lens, a live re-rank) reads null and shows no notes,
   * exactly as a query/similar scope shows no collection notes. */
  topicDetailId = $derived<string | null>(
    this.gridScope.kind === "topic" ? (this.gridScope.topicId ?? null) : null,
  );
  /** Collection ids the ACTIVE image is CURRENTLY in (open intervals) —
   * the thumb menu's Add-to-collection checkmarks and the
   * Remove-from-collection submenu. Follows the active image through
   * reportScope (the inspector-follow pattern) and re-loads on
   * collections-changed, because membership may be what changed. */
  activeMemberships = $state<string[]>([]);
  /** The hash activeMemberships describes — dedupes the follow fetch and
   * guards a stale response against a focus that moved on mid-await. */
  private membershipsHash: string | null = null;
  /** Monotone token for async grid loads (openFolder / openCollection /
   * refreshItems): only the LATEST load may setItems. Without it a
   * stale collection-members response can overwrite a just-opened
   * folder's items — or vice versa — because awaits reorder arrivals
   * while the mode fields already describe the newer view. */
  private gridLoad = 0;

  // -- topics (DESIGN-TOPICS-COLLECTIONS.md — rail Topics tab) -----------------
  /** The saved MANUAL topics (`list_topics`), newest first. A topic is a saved
   * phrase, like a saved search; its images are ALWAYS computed affinity
   * (`topic_ranked_images`), never stored membership. Editable/removable in the
   * Topics rail tab. The autosuggested (cluster) topics are computed on demand
   * by the Topics tab / graph (clusterTopics), not held here — only the durable
   * manual set persists in this store. Replaced whole on every CRUD. */
  topics = $state<TopicDto[]>([]);

  // -- attention/engagement heatmap (DESIGN-ATTENTION-HEATMAP.md) --------------
  // A focused, clearly-named region (kept localized for the parallel
  // semantic-graph merge). Two parts: the grid HEAT-TINT state (a toggle + the
  // fetched per-image intensity, the All-time recency switch) and the DWELL
  // tracker (a tiny focus-episode state machine over logic/dwell.ts).

  /** Grid heat-tint toggle, default OFF, persisted like the histogram/cell-info
   * toggles (DESIGN §"Rendering"). When on, the grid fetches intensity and each
   * cell gets a warm glow scaled by it. */
  heatOn = $state(false);
  /** "All-time" recency switch (founder decision), default OFF = recency-
   * weighted ("what am I working on now"); ON = flat all-time ("what mattered
   * most ever"). Persisted; re-fetches intensity on change. */
  heatAllTime = $state(false);
  /** Per-hash normalized intensity (0..1) for the current scope, fetched when
   * the heat tint is on. Empty when off or not yet loaded; Thumb reads a cell's
   * value (default 0 = no glow). A plain Map kept in a $state so reads in the
   * grid re-render when it is replaced wholesale. */
  intensity = $state<Map<string, number>>(new Map());

  /** The in-flight dwell focus episode (logic/dwell.ts), or null when nothing
   * is focused. Plain field (not $state): it is capture bookkeeping, never
   * rendered. */
  private dwellEpisode: DwellEpisode | null = null;
  /** Idle-flush timer handle: a focus episode with no input for IDLE_FLUSH_MS
   * is flushed (walk-away). Re-armed on each refocus/activity touch. */
  private dwellIdleTimer: ReturnType<typeof setTimeout> | undefined;
  /** Monotone token so a slow intensity fetch cannot overwrite a newer scope's
   * (the gridLoad precedent, for the heat path). */
  private heatLoad = 0;
  /** The grid item-set the cached intensity was fetched for (length + first /
   * last hash) — a cheap signature so reportScope (which fires on every focus
   * move) only re-fetches when the SCOPE's items actually changed, never on a
   * mere selection change. */
  private heatItemsKey = "";

  // -- diversify / duplication-tolerance (DESIGN-DEDUP-AND-SIMILARITY.md) -------
  // The opt-in "hide redundancy -> surface variety" view filter. It is a DISPLAY
  // LAYER over whatever scope the grid is showing (any kind), NOT a scope kind:
  // it composes with folder/collection/query/topic/similar, so making it a scope
  // would forbid diversifying a search result. The root owns the (debounced)
  // diversify_scope IPC and mirrors the resulting `shown` set into the grid slice
  // (grid.diversifyShown), exactly as the heat tint mirrors `intensity`.

  /** Diversify on/off. Default OFF — it is opt-in (the design's "Opt-in: it's
   * destructive-adjacent") and a pure view filter, so a fresh session shows the
   * whole scope. Persisted like the other reviewing-aid toggles. */
  diversifyOn = $state(false);
  /** The 0..100% duplication-tolerance slider value (the dupeGuru/digiKam idiom).
   * 0 shows everything; higher hides more (collapses each similar cluster to one
   * representative). Mapped to the backend's 0..1 tolerance by percentToTolerance
   * at call time. Persisted so the slider reopens where the reviewer left it. */
  diversifyTolerancePercent = $state(DEFAULT_TOLERANCE_PERCENT);
  /** True when the last diversify pass came back `degraded` (no CLIP model /
   * un-embedded scope): the slider disables and the chrome shows a calm "embed
   * photos to diversify" hint instead of implying the slider had no effect. */
  diversifyDegraded = $state(false);
  /** How many in-scope images the filter is currently hiding — the header's
   * unobtrusive "N hidden" affordance. Derived from the loaded scope size minus
   * the shown set (hiddenCount), so it stays honest if an item left the scope
   * between the diversify call and a re-list. 0 when off / degraded / nothing
   * collapsed. */
  diversifyHidden = $state(0);
  /** Monotone token so a slow diversify_scope pass cannot overwrite a newer
   * scope's / tolerance's result (the heatLoad/gridLoad precedent). */
  private diversifyLoad = 0;
  /** Debounce handle for the slider drag: a continuous drag fires ONE pass on
   * settle (DIVERSIFY_DEBOUNCE_MS), never one per intermediate value. */
  private diversifyTimer: ReturnType<typeof setTimeout> | undefined;
  /** The scope item-set the cached `shown` set was computed for (the heatItemsKey
   * signature, for the diversify path): reportScope fires on every focus move, so
   * the active filter only re-runs when the SCOPE's items actually changed. */
  private diversifyScopeKey = "";
  // -- near-duplicate lens (DESIGN-DEDUP-AND-SIMILARITY.md "Tier 1") -----------
  // An OPT-IN display lens over the CURRENT grid scope (like the heat tint, not
  // a new ViewMode peer): when on, the grid surface renders the near-dup GROUPS
  // (each cluster a row, a representative highlighted) instead of the ordinary
  // grid. DETECT + DISPLAY only — no delete/archive; keep/cull is sidecar truth,
  // deliberately deferred. The scope is `graphScope()` (the same image set the
  // grid/graph show); the lens re-fetches when that scope changes.

  /** Duplicates lens toggle, default OFF (opt-in, destructive-adjacent).
   * Persisted like the heat toggle. */
  dupesOn = $state(false);
  /** The raw near-dup groups from the last `find_near_duplicates` scan, or null
   * before the first scan / while one is in flight (the view shows a quiet
   * "scanning" line on null, the none-state on []). Replaced wholesale; the
   * pure logic/dedup.ts turns it into ordered clusters for the view. */
  dupeGroups = $state<DuplicateGroupDto[] | null>(null);
  /** The looseness-slider value (Hamming threshold / 64). Drives an explicit
   * `hammingThreshold` so the founder can sweep tighter/looser live; the slider
   * debounces the re-scan. Defaults to the backend's calibrated 8/64 (mirrored
   * from the tuning registry), persisted across sessions. */
  dupeThreshold = $state(DEDUP_THRESHOLD_DEFAULT);
  /** Monotone token so a slow near-dup scan cannot overwrite a newer one's
   * result (the heatLoad/gridLoad precedent, for the dedup path). */
  private dupeLoad = 0;
  /** The scope signature the cached groups were scanned for (a cheap kind +
   * length + endpoints key) so reportScope only re-scans when the SCOPE's image
   * set actually changed, never on a mere focus move. */
  private dupeScopeKey = "";

  // -- search bar (M3 search-as-scope) ----------------------------------------
  // The bar's live input state. `query`/`chips` drive the always-visible bar
  // in the grid header; committing them re-scopes the grid (runQueryScope).
  // The parallel search selection system (searchSel/searchFocus/resultHashes)
  // is RETIRED — results are ordinary grid cells now, so grid.sel is the one
  // selection. `barFocused` lets Escape's first press clear the query scope
  // while focused (the residue tells you where you return to).
  query = $state("");
  chips = $state<Filter[]>([]);
  /** True while the header search input holds focus (set by the bar's
   * focus/blur). Escape layer ordering reads it; the grid keymap does not. */
  barFocused = $state(false);
  /** The lexical/semantic lane the grid's query is CURRENTLY scoped by (M3
   * Phase 2 — the ONE explicit, centralized home for the live-lexical /
   * commit-semantic split). Phase 1 spread the lane decision across the
   * input/keydown/commit handlers and showed a static detail-row hint; this
   * flag is the single truth the bar's status indicator derives from. The
   * pure `nextLane` reducer (logic/searchmode.ts) moves it: every keystroke
   * -> "lexical", Enter -> "semantic", a dropped scope -> "none". `runQueryScope`
   * and the clear paths are the only writers, so the indicator can never
   * disagree with the lane that actually fed the grid. */
  searchLane = $state<SearchLane>("none");

  /** The `~` fuzzy quiet-toggle (search-as-scope Phase 4). OFF by default
   * (never default-on — the whole point of the feature). When armed, the
   * as-you-type LEXICAL search passes `fuzzy: true`, which appends a
   * typo-tolerant camera/lens/filename metadata pass AFTER the exact FTS hits
   * (additive widening, never reordering). LEXICAL-LANE ONLY by construction:
   * runQueryScope sends it only for the lexical mode, so a committed semantic
   * search never widens and the <100 ms keystroke budget stays protected.
   * Persisted across the session like every other UI pref (prefs.fuzzy). */
  fuzzyMode = $state(false);

  // -- ranking signals (search-as-scope Phase 3: B75 weights made visible) ----
  // The ⚙ "Ranking signals" popover's on/off state — one boolean per fusion
  // signal (S1/S2/S3/S4). Checked = the signal's B75 default weight; unchecked
  // = excluded from the fusion (weight 0). SEMANTIC-LANE ONLY by construction:
  // these toggles only ride a committed semantic search, never the lexical
  // keystroke path, so they can never tax the <100 ms budget. Persisted across
  // the session like every other UI pref (prefs.signalToggles). Default all-on
  // (the quiet B75 default) means the semantic search OMITS the weights payload
  // entirely, so the backend takes its own default fusion (today's behavior).
  signalToggles = $state<SignalToggles>(defaultToggles());
  /** Whether the ⚙ popover is open. Default-closed (the quiet discipline).
   * While OPEN the semantic search sets `include_debug` so each result's
   * per-signal contribution can be SHOWN, making the weights visible while
   * tuning; closing it drops debug again (it is only paid while tuning). */
  rankingPopoverOpen = $state(false);
  /** Per-result signal provenance from the LAST semantic search, keyed by
   * image hash (only populated while the popover was open and asked for debug).
   * The grid cells read it for the quiet per-cell contribution hint. Cleared
   * whenever the popover is closed or a non-debug search re-scopes the grid. */
  resultDebug = $state<Map<string, import("../types/search").DebugScores>>(new Map());

  // -- drag-folder drop (featureset §6: register-root confirmation) -----------
  /** Paths dropped onto the window awaiting confirmation; null = closed. */
  dropPaths = $state<string[] | null>(null);
  /** One inline line when a confirmed registration fails (honest, quiet). */
  dropError = $state<string | null>(null);

  // -- modes --------------------------------------------------------------------
  /** Auto-advance (featureset §4, D7 default OFF) — root-owned: it wires
   * grid AND look advancement. Visible via modes.ts → the indicator. */
  autoAdvance = $state(false);

  /** Set by App.svelte at mount (compile-time debug builds only). */
  debugEnabled = false;

  // ---------------------------------------------------------------------------
  // boot
  // ---------------------------------------------------------------------------

  async init() {
    this.shell.loadPrefs();
    this.grid.loadPrefs();
    this.look.loadPrefs();
    // Restore the persisted UI scale before anything renders at size —
    // fire-and-forget like the other chrome restores (a failed call just
    // leaves the design size; the next Cmd+=/− re-applies).
    if (this.shell.uiZoom !== 1) void this.applyUiZoom();
    this.autoAdvance = prefs.loadAutoAdvance();
    this.signalToggles = prefs.loadSignalToggles();
    this.fuzzyMode = prefs.loadFuzzy();
    // Heatmap toggles (DESIGN-ATTENTION-HEATMAP.md), persisted like the other
    // UI toggles. The first openFolder below drives the intensity fetch via
    // reportScope when the heat tint comes back on.
    this.heatOn = prefs.loadHeatOn();
    this.heatAllTime = prefs.loadHeatAllTime();
    // Duplicates lens (DESIGN-DEDUP-AND-SIMILARITY.md): the toggle + looseness
    // slider, persisted like the heat toggles. The first openFolder drives the
    // scan via reportScope when the lens comes back on. The threshold default
    // lives in the tuning registry (passed as the fallback), not duplicated here.
    this.dupesOn = prefs.loadDupesOn();
    this.dupeThreshold = prefs.loadDupeThreshold(DEDUP_THRESHOLD_DEFAULT);
    // Attention overlay on the graph (heatmap x graph synthesis), persisted like
    // the heat tint so the lens reopens in the view the reviewer left it.
    this.graphAttention = prefs.loadAttentionMode();
    // Diversify (duplication-tolerance) filter, persisted like the other
    // reviewing-aid toggles. The first openFolder below drives the diversify
    // pass via reportScope when the filter restores ON.
    this.diversifyOn = prefs.loadDiversifyOn();
    this.diversifyTolerancePercent = prefs.loadDiversifyTolerance();
    try {
      this.applySettings(await ipc.settingsGet());
    } catch {
      /* backend unavailable (tests/dev): defaults stand */
    }
    this.roots = await ipc.listRoots();
    // Archived snapshot for the rail's "Archived" affordance (folder-tree
    // improvements). Tolerant of an older backend without the command.
    try {
      this.archivedRoots = (await ipc.listArchivedRoots()) ?? [];
    } catch {
      /* backend unavailable / pre-archive build: no archived roots shown */
    }
    const last = prefs.loadLastFolder();
    if (last && this.roots.some((r) => r.rootId === last.rootId)) {
      await this.openFolder(last.rootId, last.folder);
    } else if (this.roots.length > 0) {
      await this.openFolder(this.roots[0].rootId, "");
    }
    this.shell.ingest = await ipc.ingestStatus();
    try {
      this.shell.onRuntimeStatus(await ipc.runtimeStatus());
    } catch {
      /* backend unavailable (tests/dev): runtime stays dark */
    }
    // After the folder is on screen — collections are rail furniture, not
    // boot-critical. `?? []`: test mocks resolve unknown commands to null.
    try {
      this.collections = (await ipc.listCollections()) ?? [];
    } catch {
      /* backend unavailable (tests/dev): no collections yet */
    }
    // Saved manual topics (the Topics rail tab) — rail furniture like
    // collections, fetched after the folder is on screen. `?? []`: test mocks
    // resolve unknown commands to null.
    try {
      this.topics = (await ipc.listTopics()) ?? [];
    } catch {
      /* backend unavailable (tests/dev): no saved topics yet */
    }
    await this.reportScope();
  }

  /** UI scale, the webview half (desktop conventions): the shell slice
   * owns the ladder/persistence; THIS applies it. Webview zoom (Tauri
   * set_zoom) rather than a CSS transform so layout, text rasterization,
   * and hit-testing all scale coherently — the same mechanism browsers
   * use for Cmd+=. Dynamic import + try/catch is the toggle-fullscreen
   * precedent: tests and non-Tauri dev have no webview to scale. */
  private async applyUiZoom() {
    try {
      const { getCurrentWebview } = await import("@tauri-apps/api/webview");
      await getCurrentWebview().setZoom(this.shell.uiZoom);
    } catch {
      /* tests / non-tauri dev */
    }
  }

  /** Backend settings echo (boot + the Settings window's live edits via
   * the `settings-changed` event): the stacked-pair display preference
   * flows into the grid slice (stacks.ts). Look starts on the same member
   * by construction — LookEntry.display derives from DisplayUnit.primary. */
  applySettings(s: AppSettings | null) {
    const member = s?.stackDisplay === "raw" ? "raw" : "jpeg";
    if (member === this.grid.stackDisplay) return;
    this.grid.setStackDisplay(member);
    // A selected collapsed pair re-reports: display member leads (U4).
    void this.reportScope();
  }

  // ---------------------------------------------------------------------------
  // Station 2.0 missing-model fix (DESIGN-STATION.md) — the hover prompt's
  // inline Download / Accept-license actions. IPC lives ONLY here (the shell
  // slice is pure state); the existing runtime_* commands are reused verbatim,
  // so no fake download — the same path Settings → Models drives. The fresh
  // RuntimeStatus the command echoes flows straight back through onRuntimeStatus
  // so the Station re-renders (downloading transient, border) immediately.
  // ---------------------------------------------------------------------------

  /** Accept a gated model's license from the Station prompt (the Settings
   * Models flow, inline): records the acceptance, then the prompt offers
   * Download. Echoes fresh status so `needsLicense` flips off live. */
  async acceptModelLicense(modelId: string) {
    try {
      this.shell.onRuntimeStatus(await ipc.runtimeAcceptLicense(modelId));
    } catch {
      /* backend unavailable (tests/dev): the prompt keeps its prior state */
    }
  }

  /** Kick a needed-but-missing model's download from the Station prompt.
   * Reuses runtime_download_model (the same command Settings → Models calls)
   * — genuine wiring, no faked progress. The echoed status flips the row to
   * "downloading", so the missing transient retires and the download
   * transient + working border take over on the next render. */
  async downloadMissingModel(modelId: string) {
    try {
      this.shell.onRuntimeStatus(await ipc.runtimeDownloadModel(modelId));
    } catch {
      /* backend unavailable (tests/dev): the prompt stays, no progress faked */
    }
  }

  get folderName(): string {
    if (this.collectionId !== null) {
      const c = this.collections.find((c) => c.id === this.collectionId);
      if (c !== undefined) return c.name;
    }
    if (this.grid.folder !== "")
      return this.grid.folder.split("/").pop() ?? this.grid.folder;
    const root = this.roots.find((r) => r.rootId === this.grid.rootId);
    return root?.displayName ?? "Photoproof";
  }

  /** Display filename for a hash, read from the currently-loaded grid items
   * (the image is on screen when "find similar" fires, in the grid OR as a
   * collapsed pair member). Used to label the similarity residue at dispatch
   * time so the residue needs no async lookup. Falls back to a generic
   * "image" when the hash is not among the loaded items (defensive — the
   * residue stays readable). */
  filenameFor(hash: string): string {
    const item = this.grid.rawItems.find((i) => i.hash === hash);
    return item?.fileName ?? "image";
  }

  // ---------------------------------------------------------------------------
  // scope reporting (CAPTURE §3 — report, then render the echo)
  // ---------------------------------------------------------------------------

  /** The ACTIVE image hash — the single funnel the inspector, membership marks,
   * dwell, and scope all read (DESIGN-VIEW-MODES.md). One arm per view: Look's
   * current image, the visualizer's selected node, or the grid focus. Per-view
   * cursors stay the source of truth for navigation; this getter just answers
   * "what photo is active now", and a VIEW SWITCH seeds the target cursor from
   * it so the photo carries across (see openVisualizer / leaveVisualizer). */
  private get activeHash(): string | null {
    switch (this.viewMode) {
      case "look":
        return this.look.currentHash;
      case "visualizer":
        return this.viewSelection;
      case "grid":
        return this.grid.activeHash;
    }
  }

  /** DESIGN-VOICE-SUBJECTS.md: the open collection's {id, name} for subject
   * routing, or null. `collectionId` already unwraps a query/similar/topic
   * scope sitting OVER a collection (the residue still points there), so this
   * follows it; the name comes from the loaded collections list. */
  private scopeCollection(): { id: string; name: string } | null {
    const id = this.collectionId;
    if (id === null) return null;
    const c = this.collections.find((c) => c.id === id);
    return c === undefined ? null : { id, name: c.name };
  }

  /** The open SAVED topic's {id, name} for subject routing, or null. The name
   * is the saved phrase; only a topic opened WITH its saved id (the rail
   * Topics tab) carries `topicDetailId`, so a phrase-only lens reads null. */
  private scopeTopic(): { id: string; name: string } | null {
    const id = this.topicDetailId;
    if (id === null) return null;
    const t = this.topics.find((t) => t.id === id);
    return t === undefined ? null : { id, name: t.phrase };
  }

  async reportScope() {
    // DESIGN-VOICE-SUBJECTS.md: build the scope source ONCE and derive both
    // the image targets and the optional non-image subject from it, so the
    // focused-image > collection > topic > neutral precedence is single-sourced
    // in scope.ts (an image always wins; a subject only rides an empty target
    // list).
    const src = {
      viewMode: this.viewMode,
      // Search is no longer a separate selection surface (M3): query results
      // ARE grid cells, so the write scope is the grid selection in every
      // non-Look case. searchOpen/searchSelection are held false/empty to
      // keep scope.ts's pure contract satisfied without a search surface.
      searchOpen: false,
      gridSelection: this.grid.selectionTargets, // stack-expanded upstream
      searchSelection: [],
      lookTargets: this.look.currentTargets,
      // The visualizer, when active, OWNS the scope: the selected node (or
      // session-neutral when none) takes precedence over grid/Look so its
      // dictation/rating never targets a stale image (scope.ts comment).
      viewSelection: this.viewSelection,
      collection: this.scopeCollection(),
      topic: this.scopeTopic(),
    };
    const targets = scopeTargets(src);
    const subject = scopeSubject(src);
    try {
      const echoed = await ipc.setScope(targets, subject);
      this.shell.onScopeEcho(echoed);
    } catch {
      /* backend unavailable (tests/dev): scope keeps last echo */
    }
    // The inspector shows the ACTIVE image's truth (featureset §3); every
    // active-hash change flows through here (focus moves, ←/→ in Look,
    // stack flips, graph node select), so an open inspector follows the eye.
    const active = this.activeHash;
    if (this.inspector.open !== false && this.inspector.hash !== active)
      await this.inspector.load(active);
    // Membership marks follow the active image the same way: the thumb
    // menu's checkmarks must be honest at open time, not after a click.
    await this.refreshActiveMemberships();
    // Dwell capture (heatmap): reportScope is the ONE funnel every focus
    // change flows through (selection, deselect, view switch, Look enter /
    // leave / nav), so refocusing the dwell tracker here covers them all with
    // one localized hook (DESIGN-ATTENTION-HEATMAP.md).
    this.dwellRefocus();
    // Heat-tint: refetch intensity only when the SCOPE's item set changed (not
    // on a mere focus move) — reportScope fires far more often than the items
    // change, and intensity is per-scope.
    this.refreshHeatIfItemsChanged();
    // Diversify: re-run the filter on the SAME scope-changed trigger so the
    // shown set follows folder/collection/query/topic switches while the filter
    // is on (the design's "Re-run when the scope changes while active"). A no-op
    // when off or the scope is unchanged.
    this.refreshDiversifyIfScopeChanged();
    // Duplicates lens: rescan only when the SCOPE's item set changed, same as
    // the heat tint (the scan is per-scope and O(n^2), so never on a focus move).
    this.refreshDupesIfScopeChanged();
  }

  /** Rescan near-duplicates when the loaded grid item-set changed since the last
   * scan (the heat-tint signature, reused). Cheap no-op when the lens is off or
   * the scope is unchanged. */
  private refreshDupesIfScopeChanged() {
    if (!this.dupesOn) return;
    const h = this.grid.scopeHashes;
    const key = `${this.gridScope.kind}:${h.length}:${h[0] ?? ""}:${h[h.length - 1] ?? ""}`;
    if (key === this.dupeScopeKey) return;
    this.dupeScopeKey = key;
    void this.fetchDuplicates();
  }

  /** Refetch heat-tint intensity when the loaded grid item-set changed since
   * the last fetch (a cheap length + endpoints signature). Cheap no-op when the
   * heat tint is off or the scope is unchanged. */
  private refreshHeatIfItemsChanged() {
    if (!this.heatOn) return;
    const h = this.grid.scopeHashes;
    const key = `${h.length}:${h[0] ?? ""}:${h[h.length - 1] ?? ""}`;
    if (key === this.heatItemsKey) return;
    this.heatItemsKey = key;
    void this.fetchIntensity();
  }

  /** Load the ACTIVE image's current memberships (collections_for_image).
   * Skipped entirely while no collections exist — the common no-collection
   * session pays zero extra IPC per focus move. `force` re-fetches for an
   * unchanged hash (a collections-changed snapshot may mean THIS image's
   * membership moved in another window). */
  private async refreshActiveMemberships(force = false) {
    const active = this.activeHash;
    if (!force && active === this.membershipsHash) return;
    this.membershipsHash = active;
    if (active === null || this.collections.length === 0) {
      this.activeMemberships = [];
      return;
    }
    try {
      const list = (await ipc.collectionsForImage(active)) ?? [];
      // The focus may have moved on during the await: a stale response
      // must not label a different image's menu.
      if (this.membershipsHash === active)
        this.activeMemberships = list.map((c) => c.id);
    } catch {
      /* backend unavailable (tests/dev): no membership marks */
    }
  }

  // ---------------------------------------------------------------------------
  // folders & grid
  // ---------------------------------------------------------------------------

  async refreshRoots() {
    this.applyRootsSnapshot(await ipc.listRoots());
    // Keep the archived snapshot fresh too (folder-tree improvements): the
    // rail's "Archived" affordance reads it. Cheap; only changes on lifecycle.
    this.archivedRoots = (await ipc.listArchivedRoots()) ?? [];
  }

  /** Apply a roots snapshot: a vanished current root resets the grid. */
  private applyRootsSnapshot(roots: RootDto[]) {
    this.roots = roots;
    if (
      this.grid.rootId !== null &&
      !roots.some((r) => r.rootId === this.grid.rootId)
    ) {
      this.grid.rootId = null;
      this.grid.rawItems = [];
    }
  }

  /** Roots edited in ANY window land here LIVE (add_root/remove_root emit
   * `roots-changed` with the fresh snapshot — the P4.2b settings-changed
   * pattern; founder dogfood, round 2): the rail updates instantly. With
   * nothing open afterwards — the open root was removed, or a first root
   * just arrived — the first remaining root opens (the init() rule), so
   * the grid never sits on a dead folder. */
  async onRootsChanged(roots: RootDto[]) {
    // Drop any cached Visualizer graph for a root that just disappeared from the
    // snapshot. The module-level graphState snapshot survives the lens
    // close→reopen by design, so a removed root would otherwise restore a stale
    // layout pointing at vanished images (the "view-swap workaround"). Diff the
    // prior roots against the incoming set BEFORE applyRootsSnapshot overwrites
    // this.roots, and invalidate each removed root's scoped graphs.
    const incoming = new Set(roots.map((r) => r.rootId));
    for (const prior of this.roots)
      if (!incoming.has(prior.rootId)) invalidateScopedGraphs(prior.rootId);
    this.applyRootsSnapshot(roots);
    // A collection or query view has rootId null BY DESIGN — never yank it
    // to a folder just because a roots snapshot landed. Only an empty
    // FOLDER scope with no root falls back to the first root.
    if (
      this.gridScope.kind === "folder" &&
      this.grid.rootId === null &&
      roots.length > 0
    )
      await this.openFolder(roots[0].rootId, "");
  }

  /** "Add folder…" (rail footer button + rail-folder seat): the OS
   * directory picker straight from the rail — the FirstRun/Settings
   * add-root flow, one click (founder, dogfood rounds 1+2). The new root
   * opens; every other window's rail follows via `roots-changed`. */
  async addRootFromPicker() {
    let dir: unknown;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      dir = await open({ directory: true, multiple: false });
    } catch {
      return; // non-tauri dev/tests: no picker to show
    }
    if (typeof dir !== "string") return;
    // The lying window (founder, June 2026): between this call and the
    // pump's first scanning=true emit, ingest.running is still false —
    // the empty state must read "indexing", never "no photographs".
    // Optimistic; the first real status event clears it (onIngestProgress),
    // and the watchdog stands it down if a silent no-op emits nothing.
    this.shell.expectIngest(INGEST_EXPECT_TIMEOUT_MS);
    let outcome: AddRootOutcome;
    try {
      outcome = await ipc.addRoot(dir);
    } catch (e) {
      this.shell.clearIngestExpecting(); // terminal: no scan will run
      throw e;
    }
    // Refuse + alias (folder-tree improvements): a folder that overlaps an
    // existing active root is NOT re-ingested. Navigate to the root the user
    // already has instead, and stand the optimistic bridge down (no scan).
    if (outcome.kind === "overlap") {
      this.shell.clearIngestExpecting();
      await this.openFolder(outcome.existingRootId, "");
      return;
    }
    await this.refreshRoots();
    await this.openFolder(outcome.root.rootId, "");
  }

  async openFolder(rootId: string, folder: string) {
    // Opening a folder from the rail drops Look back to the grid (founder
    // dogfood, round 1); the visualizer PERSISTS and re-points at the new
    // scope via graphScope() (DESIGN-VIEW-MODES.md transition table). Only the
    // look arm needs the leaveLook teardown — the visualizer is untouched.
    if (this.viewMode === "look") await this.leaveLook();
    // The grid is now in folder scope (M3); switching source clears any live
    // query (D5: query scope is ephemeral per-source).
    this.gridScope = { kind: "folder", rootId, folder };
    this.clearQueryInput();
    this.grid.rootId = rootId;
    this.grid.folder = folder;
    this.grid.sort = prefs.loadSort(rootId, folder);
    this.grid.sel = sel.EMPTY;
    const load = ++this.gridLoad;
    const items = await ipc.listFolder(rootId, folder);
    if (load !== this.gridLoad) return; // a newer load owns the grid now
    this.grid.setItems(items);
    const tree = await ipc.folderTree(rootId);
    if (load !== this.gridLoad) return;
    this.tree = tree;
    prefs.saveLastFolder(rootId, folder);
    await this.reportScope();
  }

  /** Open a collection's current members in the grid — the folder-open
   * sibling (B71: collections drive the grid exactly as folders do). The
   * grid leaves folder mode entirely: rootId goes null so folder-keyed
   * machinery (sort persistence, last-folder, ingest re-list) stands down. */
  async openCollection(id: string) {
    if (this.viewMode === "look") await this.leaveLook();
    this.gridScope = { kind: "collection", id };
    this.clearQueryInput();
    this.grid.rootId = null;
    this.grid.folder = "";
    this.grid.sel = sel.EMPTY;
    const load = ++this.gridLoad;
    const items = (await ipc.listCollectionMembers(id)) ?? [];
    if (load !== this.gridLoad) return; // superseded mid-await (mode may differ)
    this.grid.setItems(items);
    await this.reportScope();
  }

  /** `collections-changed` snapshot (any window's mutation — the
   * roots-changed pattern): replace the list whole; a viewed collection
   * re-lists its members, and the active image's membership marks
   * re-fetch, because membership may be what changed. */
  async onCollectionsChanged(collections: CollectionDto[]) {
    this.collections = collections;
    if (this.collectionId !== null) await this.refreshItems();
    await this.refreshActiveMemberships(true);
  }

  /** The rail's inline create (its footer affordance, the add-root
   * sibling). The fresh snapshot is fetched directly — awaited and
   * deterministic for tests; the `collections-changed` event is the
   * cross-window catch-all, and replacing the same snapshot twice is
   * harmless. NO catch here: creating a collection is a user-truth write
   * (RETRIEVAL §10.2) — a real persistence failure must not vanish as if
   * the backend were merely a test mock (the rate() precedent; the
   * fire-and-forget swallow is reserved for OS verbs like reveal). */
  async createCollection(name: string): Promise<string | null> {
    const trimmed = name.trim();
    if (trimmed === "") return null;
    // create_collection resolves to the new DTO — return its id so callers
    // that need to chain a write (createCollectionAndAdd) can target the
    // collection deterministically instead of guessing from the snapshot.
    const created = await ipc.createCollection(trimmed);
    this.collections = (await ipc.listCollections()) ?? [];
    return created?.id ?? null;
  }

  /** Mint a collection and drop `targets` into it in one evented step
   * (founder, dogfood June 12 2026 — the thumb menu's "New collection…").
   * The targets are passed IN (captured synchronously at menu-pick time, so
   * a selection change while the user types the name cannot poison them).
   * Order matters: the add must wait for the create's id, and a blank name
   * (createCollection returns null) adds nothing — the new-empty-collection
   * failure mode the founder flagged. NO catch: like createCollection and
   * the add-to-collection sink, gathering is user truth (RETRIEVAL §10.2)
   * and a real persistence failure must surface, not vanish as a test mock. */
  async createCollectionAndAdd(targets: string[], name: string): Promise<void> {
    const id = await this.createCollection(name);
    if (id === null || targets.length === 0) return;
    await ipc.addToCollection(id, targets);
    // Same awaited direct refresh the add-to-collection sink uses — the
    // collections-changed event is the cross-window catch-all.
    await this.onCollectionsChanged((await ipc.listCollections()) ?? []);
  }

  /** Incremental refresh during ingest: keeps selection/focus (UI §3.3).
   * The token is taken only when something will actually load — a no-op
   * call must not cancel an in-flight open. */
  async refreshItems() {
    const scope = this.gridScope;
    if (scope.kind === "collection") {
      const load = ++this.gridLoad;
      const items = (await ipc.listCollectionMembers(scope.id)) ?? [];
      if (load === this.gridLoad) this.grid.setItems(items);
      return;
    }
    if (scope.kind === "query") {
      // A live re-list under a query re-runs the SAME lane the scope was
      // committed with (the bar's commit state owns the lane choice). Phase
      // 1: re-run lexical — a background ingest refresh must never silently
      // upgrade a lexical scope to semantic (or pay vector latency for it).
      // transition=false: this is not a user keystroke, so the displayed lane
      // (a committed "semantic" included) must not flip to lexical underneath.
      await this.runQueryScope("lexical", false);
      return;
    }
    if (scope.kind === "similar") {
      // A live re-list under a similarity view re-runs the SAME neighbor
      // search so newly-embedded images can enter the set. The source folder
      // and filename are unchanged — re-feed against the same query image.
      await this.runSimilarScope(scope.hash, scope.filename);
      return;
    }
    if (scope.kind === "topic") {
      // A live re-list under a topic view re-ranks the SAME phrase so
      // newly-embedded images can enter (and re-rank) the set. The underlying
      // source is unchanged — re-feed against the same topic phrase, preserving
      // the open topic's id so its note log stays surfaced across the re-rank.
      await this.runTopicScope(scope.phrase, scope.topicId);
      return;
    }
    if (this.grid.rootId === null) return;
    const load = ++this.gridLoad;
    const items = await ipc.listFolder(this.grid.rootId, this.grid.folder);
    if (load === this.gridLoad) this.grid.setItems(items);
  }

  /** Coalesced ingest progress (pump.rs, ≤1 per 400 ms): the indicator
   * pill always updates; while ingest RUNS the open folder also re-lists
   * on a 2 s throttle — new images and previewReady flips only enter the
   * grid through list_folder, and without this the grid sat EMPTY for the
   * whole first scan of a slow network volume (founder, SMB, June 2026).
   * The running→idle edge re-lists once more, unthrottled, so the settled
   * state is exact. */
  private lastIngestRefresh = 0;
  async onIngestProgress(status: IngestStatus) {
    // Real status arrived: the optimistic add-root/rescan bridge stands
    // down — `running` (walk-aware via `scanning`) owns the empty-state
    // copy from here. Cleared on EVERY event, idle ones included: an
    // instantly-finished scan must not leave "Indexing" stranded. This also
    // CANCELS the pending watchdog so a late timer can't fire a spurious
    // clear after a healthy scan already took over.
    this.shell.clearIngestExpecting();
    const wasRunning = this.shell.ingest.running;
    this.shell.ingest = status;
    if (this.grid.rootId === null) return;
    if (status.running) {
      const now = Date.now();
      if (now - this.lastIngestRefresh < INGEST_RELIST_MS) return;
      this.lastIngestRefresh = now;
      await this.refreshItems();
    } else if (wasRunning) {
      await this.refreshItems();
    }
  }

  async applySelection(next: sel.SelState) {
    this.grid.setSelection(next);
    await this.reportScope();
  }

  // ---------------------------------------------------------------------------
  // Look (cross-slice flow; INTEGRATION finishes nav-set + anchor restore)
  // ---------------------------------------------------------------------------

  async openLook(hash: string) {
    // Navigation set = entry selection (featureset §2): a ≥2 selection
    // including the entry cycles within it (GRID order — looknav.ts);
    // otherwise the whole grid. Query results are ordinary grid cells now
    // (M3 search-as-scope), so there is ONE path — the old fromSearch branch
    // over a parallel result/selection list is retired with the overlay.
    const nav = navigationSet(this.grid.units, this.grid.sel.order, hash);
    if (nav === null) return;
    const { order, index: idx } = nav;
    this.look.open(order, idx);
    this.viewMode = "look";
    await this.reportScope();
  }

  async lookNav(delta: 1 | -1) {
    if (!this.look.next(delta)) return;
    await this.reportScope();
  }

  async leaveLook() {
    // Back to Grid with the same image ACTIVE (UI §2.2). Flip-aware: after
    // R the viewed hash may be a collapsed pair's hidden member, so the
    // match runs over primary AND alt. The grid restores its own scroll
    // anchor on mount and then scrolls the active cell into view.
    const hash = this.look.currentHash;
    this.viewMode = "grid";
    this.look.close();
    if (hash !== null) {
      const idx = this.grid.units.findIndex(
        (u) => u.primary.hash === hash || u.alt?.hash === hash,
      );
      if (idx >= 0) this.grid.sel = { ...this.grid.sel, focus: idx };
    }
    await this.reportScope();
  }

  /** G — universal "go home" (featureset §0). Clears any DERIVED scope too
   * (query OR similar): home is the underlying folder/collection, not a
   * search result set or a similarity view. */
  async goHome() {
    // "go grid" (G / goHome): *->grid (DESIGN-VIEW-MODES.md). Leave the
    // visualizer first (it seeds grid focus from the departing selection), then
    // FALL THROUGH so a derived scope underneath the lens still clears to its
    // source — G is "land me on the plain grid", not "just close the lens".
    // The look arm keeps the image active and has nothing further to do.
    if (this.viewMode === "visualizer") await this.leaveVisualizer();
    if (this.viewMode === "look") {
      await this.leaveLook();
      return;
    }
    if (
      this.gridScope.kind === "query" ||
      this.gridScope.kind === "similar" ||
      this.gridScope.kind === "topic"
    ) {
      await this.clearQueryScope();
      return;
    }
    await this.reportScope();
  }

  // ---------------------------------------------------------------------------
  // Search-as-scope (M3): the always-visible bar re-scopes the grid in place
  // ---------------------------------------------------------------------------

  /** `/` and Cmd+F: focus the header bar (no overlay). The bar component
   * owns the actual DOM focus via this token; bumping it re-fires its focus
   * effect even when the value is unchanged. */
  focusBarRequest = $state(0);
  focusBar() {
    this.focusBarRequest += 1;
  }

  /** Clear the bar's live input without touching the grid scope (used when
   * switching source: the scope is already being replaced). The lane drops to
   * "none" too — wiping the text means the bar is no longer a scope, so the
   * detail row must not keep naming a stale lexical/semantic lane. This is the
   * single funnel for every explicit clear (Esc / residue / source switch). */
  private clearQueryInput() {
    this.query = "";
    this.chips = [];
    this.searchLane = nextLane(this.searchLane, "clear");
  }

  /** The underlying folder/collection source the current scope rests on —
   * the thing one-key clear returns to. A DERIVED scope (query / similar /
   * topic) is never the source for a new derived scope (you return PAST it,
   * not to it), so each unwraps to its own `within`; a plain folder/collection
   * IS the source. Shared by `runQueryScope` / `runSimilarScope` /
   * `runTopicScope`. */
  private scopeSource(): GridScope {
    return this.gridScope.kind === "query" ||
      this.gridScope.kind === "similar" ||
      this.gridScope.kind === "topic"
      ? this.gridScope.within
      : this.gridScope;
  }

  /** Back-compat alias kept for the query feeder's call site (same rule as
   * `scopeSource`, named for the query path it has always served). */
  private queryWithin(): GridScope {
    return this.scopeSource();
  }

  /**
   * Run the bar's query and re-scope the grid to the results, in place.
   * The fourth `setItems` feeder (next to openFolder/openCollection/
   * refreshItems): `search` returns result hashes in fused order, then
   * `list_images` enriches them into GridItems; we feed them in that order
   * and the grid renders them as ordinary cells. Guarded by the SAME
   * monotone `gridLoad` token as the other feeders so a slow query can't
   * overwrite a newer scope.
   *
   * `mode` picks the lane (the <100 ms guardrail): "lexical" for as-you-type
   * (forced keyword, the budget floor), "semantic" on Enter (full hybrid).
   * An empty bar (no query, no chips) is NOT a scope — it returns the grid
   * to its underlying source (the quiet zero-config default).
   *
   * `transition` (default true) gates the user-facing lane indicator: a USER
   * action (typing, Enter) moves the explicit lane via nextLane; a BACKGROUND
   * re-list (refreshItems during ingest) re-runs the keyword query for fresh
   * items but passes `false` so the displayed mode does not flip — a scope the
   * user committed as "semantic" must keep reading "semantic" while ingest
   * churns underneath it.
   */
  async runQueryScope(mode: ipc.SearchMode, transition = true) {
    const trimmed = this.query.trim();
    if (this.chips.length === 0 && trimmed.length < MIN_QUERY_CHARS) {
      // Below the threshold with no chips: not a query (yet). Return the grid
      // to its underlying source WITHOUT touching the bar input — the user is
      // mid-type (e.g. the first character of a fresh query), and clearing
      // this.query here would erase it under them (the input is bind:value'd).
      // Only an EXPLICIT clear (Esc / G / the residue button) wipes the text.
      // The bar is no longer a scope, so the lane drops to "none" — the detail
      // row stops naming a lane (returnToSource also clears it; this covers the
      // never-formed-a-scope keystroke too).
      if (transition) this.searchLane = nextLane(this.searchLane, "clear");
      if (this.gridScope.kind === "query") await this.returnToSource();
      return;
    }
    // A real query is forming: move the explicit lane in lockstep with the
    // lane that is about to feed the grid (type -> lexical, commit ->
    // semantic). This is the single write the status indicator reads. A
    // background refresh (transition=false) skips it so the label holds.
    if (transition)
      this.searchLane = nextLane(this.searchLane, mode === "semantic" ? "commit" : "type");
    const within = this.queryWithin();
    const wasQuery = this.gridScope.kind === "query";
    // Set the scope discriminator BEFORE the await: folderName, the bar's
    // residue, and the sort menu all key off it, and a stale async result is
    // already fenced by gridLoad below.
    this.gridScope = { kind: "query", query: this.query, chips: [...this.chips], within };
    // Entering query mode defaults the sort to relevance — the backend's
    // fused order, which sortItems preserves as a pass-through (this is the
    // spec's "committing a search auto-selects relevance"). A semantic commit
    // re-asserts it. But a user who picked date/filename WHILE already in a
    // query keeps it across further keystrokes — re-scoping the same hashes
    // must not yank a chosen ordering out from under them.
    if (!wasQuery || mode === "semantic") this.grid.sort = "relevance";
    // Phase 3 tuning rides the SEMANTIC lane only. Non-default toggles send an
    // explicit `weights` payload (an unchecked signal -> 0.0, excluded from the
    // fusion); all-on omits it so the backend takes its own default fusion
    // (today's exact behavior). `includeDebug` is set only while the ⚙ popover
    // is open, so each result's per-signal contribution can be SHOWN. The
    // lexical lane carries neither — the toggles can never tax the keystroke
    // budget.
    const tuning =
      mode === "semantic"
        ? {
            weights: isDefault(this.signalToggles)
              ? undefined
              : togglesToWeights(this.signalToggles),
            includeDebug: this.rankingPopoverOpen,
          }
        : undefined;
    // The `~` fuzzy quiet-toggle rides the LEXICAL lane only: when armed, the
    // as-you-type search widens with typo-tolerant metadata matches appended
    // below the exact set. The semantic lane never widens (its vectors already
    // generalize past typos), so this can never tax the commit path.
    const fuzzy = mode === "lexical" && this.fuzzyMode;
    const load = ++this.gridLoad;
    const results = await ipc.search(this.query, this.chips, mode, tuning, fuzzy);
    if (load !== this.gridLoad) return; // a newer scope owns the grid now
    // Retain per-result signal provenance for the cells' contribution hint —
    // only while the popover asked for it; otherwise keep the map empty so the
    // hints stay quiet (and a lexical re-list never leaves stale debug behind).
    this.resultDebug =
      mode === "semantic" && this.rankingPopoverOpen
        ? new Map(
            results.images.flatMap((i) => (i.debug !== null ? [[i.image_hash, i.debug]] : [])),
          )
        : new Map();
    const hashes = results.images.map((i) => i.image_hash);
    // Enrich result hashes → GridItems (in fused order; list_images
    // preserves the order given). The grid's relevance sort keeps it.
    const items = hashes.length === 0 ? [] : ((await ipc.listImages(hashes)) ?? []);
    if (load !== this.gridLoad) return;
    this.grid.setItems(items);
    await this.reportScope();
  }

  /**
   * "More like this": re-scope the grid to the visual neighbors of `hash`.
   * The fifth `setItems` feeder, deliberately the same two-step shape as
   * `runQueryScope` — `find_similar` returns neighbor hashes in similarity
   * order, then `list_images` enriches them into the same GridItems folders
   * and queries render. Guarded by the SAME monotone `gridLoad` token so a
   * slow neighbor search can't overwrite a newer scope.
   *
   * `within` is the source one-key clear returns to: when invoked from a
   * query or another similar scope, we keep THAT scope's underlying source
   * (a similarity view is never the thing you "return to" — you return past
   * it), mirroring `queryWithin`. Relevance is the backend's similarity
   * order, pass-through (the grid's relevance sort preserves it). An
   * un-embedded image or empty index yields an empty grid, never an error —
   * the command resolves to [] in that case.
   *
   * Surface-safe like returnToSource: a neighbor search can be triggered
   * from Look (the look-backdrop seat), so it swaps the grid scope
   * UNDERNEATH Look; the caller leaves Look first when that is the intent.
   */
  async runSimilarScope(hash: string, filename: string) {
    const within = this.scopeSource();
    // Set the discriminator BEFORE the await: the residue and the sort menu
    // key off it immediately; a stale async result is fenced by gridLoad.
    this.gridScope = { kind: "similar", hash, filename, within };
    // Similarity order IS the relevance order — the same pass-through the
    // query scope uses (sortItems preserves the fed order under "relevance").
    this.grid.sort = "relevance";
    // A fresh similarity view clears the bar's live input: the bar is not the
    // scope here, and a stale lexical/semantic lane label would mislead.
    this.clearQueryInput();
    const load = ++this.gridLoad;
    const hashes = (await ipc.findSimilar(hash)) ?? [];
    if (load !== this.gridLoad) return; // a newer scope owns the grid now
    const items = hashes.length === 0 ? [] : ((await ipc.listImages(hashes)) ?? []);
    if (load !== this.gridLoad) return;
    this.grid.setItems(items);
    await this.reportScope();
  }

  /**
   * Topic scope (DESIGN-TOPICS-COLLECTIONS.md): re-scope the grid to a topic
   * phrase's RANKED images (highest blended affinity first). The SIXTH
   * `setItems` feeder, deliberately the same two-step shape as
   * `runSimilarScope` — `topic_ranked_images` returns hash+score in descending
   * affinity, then `list_images` enriches the hashes into the same GridItems
   * folders and queries render. Guarded by the SAME monotone `gridLoad` token so
   * a slow rank can't overwrite a newer scope.
   *
   * `within` is the source one-key clear / Escape returns to: when invoked from
   * a derived scope we keep THAT scope's underlying source (a topic view is
   * never the thing you "return to"), mirroring `runSimilarScope`. Ranked order
   * IS the relevance order — the same pass-through the query scope uses
   * (sortItems preserves the fed order under "relevance"). The ranked replies
   * are also cached on `topicScored` so the Topics-tab bake bar can threshold
   * over them without a second fetch. An un-embedded scope yields an empty grid,
   * never an error.
   *
   * Surface-safe like runSimilarScope: triggered from the rail, it swaps the
   * grid scope; the caller leaves Look first when that is the intent.
   */
  async runTopicScope(phrase: string, topicId?: string) {
    const within = this.scopeSource();
    // Set the discriminator BEFORE the await: the residue and the sort menu key
    // off it immediately; a stale async result is fenced by gridLoad. `topicId`
    // (present when opened from a saved topic in the rail) rides the scope so
    // the topic's note log can surface, mirroring the `collection` scope's id.
    this.gridScope = { kind: "topic", phrase, topicId, within };
    // Ranked (descending-affinity) order IS the relevance order — the same
    // pass-through the query/similar scopes use.
    this.grid.sort = "relevance";
    // A fresh topic view clears the bar's live input: the bar is not the scope
    // here, and a stale lexical/semantic lane label would mislead.
    this.clearQueryInput();
    const scope = this.graphScope();
    const load = ++this.gridLoad;
    let ranked: import("../types/dto").RankedImageDto[] = [];
    try {
      ranked = (await ipc.topicRankedImages(phrase, scope)) ?? [];
    } catch {
      ranked = []; // unreachable backend / empty index: an honest empty grid
    }
    if (load !== this.gridLoad) return; // a newer scope owns the grid now
    // Cache the ranked scores for the Topics-tab bake bar (threshold -> count ->
    // members) so the bake thresholds over the SAME numbers the grid was fed.
    this.topicScored = topicbake.rankedToScored(ranked);
    const hashes = topicbake.rankedHashes(ranked);
    const items = hashes.length === 0 ? [] : ((await ipc.listImages(hashes)) ?? []);
    if (load !== this.gridLoad) return;
    this.grid.setItems(items);
    await this.reportScope();
  }

  // ---------------------------------------------------------------------------
  // topics CRUD + the topic -> collection bake (DESIGN-TOPICS-COLLECTIONS.md).
  // Manual topics persist (a saved phrase); the bake is a ONE-WAY commit of a
  // threshold into an ordinary evented collection (provenance recorded by the
  // backend). The Topics rail tab + the graph share these methods.
  // ---------------------------------------------------------------------------

  /** The ranked affinity scores the CURRENT topic scope was fed (hash + score,
   * descending). The Topics-tab bake bar thresholds over these so its live count
   * + bake membership agree with what the grid shows. Empty outside a topic
   * scope (or before the rank resolves). Plain field reflected into $state via
   * the setter below so the tab's count re-renders. */
  topicScored = $state<topicbake.ScoredImage[]>([]);

  /** Save a phrase as a manual topic (the Topics rail tab's add-topic input,
   * and the "promote a suggestion" click). NO catch: saving a topic is user
   * truth like createCollection — a real persistence failure must surface, not
   * vanish as a test mock. A blank phrase or an already-saved one is a no-op
   * (the rail also guards, belt-and-braces). Refreshes the snapshot directly
   * (awaited + deterministic for tests). */
  async addTopic(phrase: string): Promise<void> {
    const trimmed = phrase.trim();
    if (trimmed === "") return;
    if (this.topics.some((t) => t.phrase === trimmed)) return;
    await ipc.addTopic(trimmed);
    this.topics = (await ipc.listTopics()) ?? [];
  }

  /** Remove a saved manual topic (the Topics rail tab's per-topic remove). If
   * the removed topic is the one currently scoping the grid, fall back to its
   * source so the grid never sits on a deleted topic. */
  async removeTopic(id: string): Promise<void> {
    const removed = this.topics.find((t) => t.id === id);
    await ipc.removeTopic(id);
    this.topics = (await ipc.listTopics()) ?? [];
    if (
      removed !== undefined &&
      this.gridScope.kind === "topic" &&
      this.gridScope.phrase === removed.phrase
    )
      await this.clearQueryScope();
  }

  /** Select a topic in the rail -> scope the grid to its ranked images AND, when
   * opened from a saved topic record (`topicId` given), surface that topic's
   * append-only note log in the rail. Mirrors how `openCollection` both scopes
   * the grid and shows the collection's notes. Leaves Look first (the rail
   * drives the grid surface), like openFolder. A phrase-only call (the graph
   * lens) scopes the grid without a note log. */
  async openTopic(phrase: string, topicId?: string): Promise<void> {
    if (this.viewMode === "look") await this.leaveLook();
    await this.runTopicScope(phrase, topicId);
  }

  /**
   * The bake (DESIGN-TOPICS-COLLECTIONS.md): commit a topic threshold into an
   * evented collection. Members are the in-scope images scoring >= `threshold`;
   * the backend records the provenance (phrase + threshold + alpha) and returns
   * an ordinary INDEPENDENT collection (ONE-WAY, no live link back). The graph
   * slider and the Topics tab both call this. `name` defaults to the phrase at
   * the call site. Refreshes the collections snapshot so the new collection
   * appears in the rail immediately (the collections-changed event is the
   * cross-window catch-all). NO catch: a bake is a user-truth write.
   *
   * Resolves to the new collection (so the caller can surface a toast / select
   * it), or null on a blank name / empty selection (nothing to bake).
   */
  async bakeTopicCollection(
    phrase: string,
    threshold: number,
    name: string,
    alpha?: number,
  ): Promise<CollectionDto | null> {
    const trimmed = name.trim();
    if (trimmed === "") return null;
    const created = await ipc.createCollectionFromTopic(
      phrase,
      this.graphScope(),
      threshold,
      trimmed,
      alpha,
    );
    await this.onCollectionsChanged((await ipc.listCollections()) ?? []);
    return created ?? null;
  }

  // ---------------------------------------------------------------------------
  // The visualizer view (DESIGN-VIEW-MODES.md) — the force-directed topic-graph
  // lens, now a PEER view on the viewMode axis (not an overlay boolean). A
  // focused, clearly named region so the parallel heatmap merge stays
  // mechanical.
  // ---------------------------------------------------------------------------

  /** Open the visualizer over the current grid scope, like find-similar leaves
   * Look first. R6 seed-from-active (DESIGN-VIEW-MODES.md): the visualizer
   * SEEDS viewSelection from the photo you were just on (grid focus / Look
   * current) so it carries across the switch and dictation/rating continue on
   * it — NOT the old unconditional neutralize. When NOTHING was active (a fresh
   * scope, nothing focused), it opens NEUTRAL (viewSelection null, scope []) so
   * a dictation becomes a session note instead of mis-targeting. The seed is
   * read BEFORE flipping viewMode, because activeHash's arm changes with it. */
  async openVisualizer() {
    if (this.viewMode === "look") await this.leaveLook();
    const seed = this.activeHash; // read before the view switch changes the arm
    this.viewMode = "visualizer";
    this.viewSelection = seed;
    await this.reportScope();
  }

  /** Single-click on a visualizer image node SELECTS it (glow + scope); pass
   * null to DESELECT (Esc on a selection, or a click on empty canvas).
   * Reporting scope here is the whole point: a selected node becomes the
   * dictation/rating target, a deselect returns to the neutral session scope.
   * (Method name unchanged — TopicGraph calls it; it writes viewSelection.) */
  async selectGraphNode(hash: string | null) {
    this.viewSelection = hash;
    await this.reportScope();
  }

  /** Leave the visualizer back to the grid. Seeds grid focus from the departing
   * viewSelection (the photo carries across the switch, DESIGN-VIEW-MODES.md)
   * so the grid lands on the same image; then clears the selection and reports
   * so the write scope returns to the grid selection underneath. */
  async leaveVisualizer() {
    const hash = this.viewSelection;
    if (hash !== null) {
      const idx = this.grid.units.findIndex(
        (u) => u.primary.hash === hash || u.alt?.hash === hash,
      );
      if (idx >= 0) this.grid.sel = { ...this.grid.sel, focus: idx };
    }
    this.viewMode = "grid";
    this.viewSelection = null;
    await this.reportScope();
  }

  /** Set the Attention overlay mode on the graph (Off / Engaged / Overlooked)
   * and persist it. The TopicGraph component reacts to the change (fetching
   * intensity + recomputing the synthesis); this just holds + persists the
   * flag, like toggleHeat for the grid tint. */
  setAttention(mode: prefs.AttentionMode) {
    this.graphAttention = mode;
    prefs.saveAttentionMode(mode);
  }

  /** The backend GraphScope for the lens, derived from the CURRENT grid scope
   * (the lens shows whatever the grid shows). A derived scope (query/similar)
   * unwraps to its underlying folder/collection source; the founder can also
   * point the lens at the WHOLE library (the deliberate scale spike) via the
   * lens' own control, which passes `{ kind: "library" }` directly. */
  graphScope(): ipc.GraphScope {
    const src = this.scopeSource();
    if (src.kind === "collection") return { kind: "collection", id: src.id };
    if (src.kind === "folder")
      return { kind: "folder", root_id: src.rootId, folder: src.folder };
    // A bare query/similar with no resolvable folder/collection source falls
    // back to the whole library (nothing narrower to scope to).
    return { kind: "library" };
  }

  /** Click a topic anchor → scope the grid to that topic (visualizer->grid +
   * semantic query scope, DESIGN-VIEW-MODES.md). v1 reuses the query scope
   * machinery: the topic phrase becomes a committed semantic query, so the grid
   * shows the topic's strongest matches in fused order, with the residue +
   * Escape-to-clear the query scope already provides. Dropping to the grid view
   * (replacing the old closeGraph()) lands the user on the scoped grid. */
  async scopeToTopic(phrase: string) {
    this.viewMode = "grid";
    this.viewSelection = null;
    this.query = phrase;
    this.chips = [];
    await this.runQueryScope("semantic");
  }

  /** Double-click / Enter on a selected image node → open it in Look. openLook
   * sets viewMode="look" DIRECTLY, so there is no closeGraph-then-open flash
   * through the grid (DESIGN-VIEW-MODES.md (iii)); the trailing viewSelection
   * reset keeps the departed visualizer clean for its next open. openLook
   * builds the nav set over the grid units exactly as a grid click would.
   * (Single click now SELECTS rather than opens — selectGraphNode.) */
  async openFromGraph(hash: string) {
    await this.openLook(hash);
    this.viewSelection = null;
  }

  /** Re-point the grid from a DERIVED scope (query OR similar) back to its
   * underlying source and re-list, WITHOUT clearing the bar input or leaving
   * Look.
   *
   * Surface-safe: a query can be committed (or a neighbor search triggered)
   * and then a result opened in Look, so this swaps the grid scope UNDERNEATH
   * Look (Look has its own Esc layer below). It re-points gridScope at the
   * source, restores its sort, and re-lists via refreshItems (the scope-aware
   * feeder) — never through openFolder/openCollection, which would leaveLook
   * and peel two Esc layers at once. No-op when the scope is not derived (a
   * plain folder/collection has nowhere to return to). */
  private async returnToSource() {
    if (
      this.gridScope.kind !== "query" &&
      this.gridScope.kind !== "similar" &&
      this.gridScope.kind !== "topic"
    )
      return;
    const within = this.gridScope.within;
    this.gridScope = within;
    if (within.kind === "folder") {
      this.grid.rootId = within.rootId;
      this.grid.folder = within.folder;
      this.grid.sort = prefs.loadSort(within.rootId, within.folder);
    } else {
      this.grid.rootId = null;
      this.grid.folder = "";
    }
    await this.refreshItems();
    await this.reportScope();
  }

  /** First Escape / one-key residue clear / G: drop a DERIVED scope (query
   * OR similar) and return the grid to its underlying source. The bar input
   * clears too — an EXPLICIT clear is the only thing that wipes the text, and
   * the residue's whole point is that you SEE where you land. */
  async clearQueryScope() {
    const wasDerived =
      this.gridScope.kind === "query" ||
      this.gridScope.kind === "similar" ||
      this.gridScope.kind === "topic";
    this.clearQueryInput();
    // returnToSource re-lists AND reports when it was a derived scope; when it
    // wasn't (defensive — callers gate) the input still cleared, so report
    // directly.
    if (wasDerived) await this.returnToSource();
    else await this.reportScope();
  }

  /** Bar edit removed the last chip (Backspace on an empty input). Re-runs
   * the live lexical lane so the grid re-scopes immediately. */
  async removeChip(index: number) {
    this.chips = this.chips.filter((_, i) => i !== index);
    await this.runQueryScope("lexical");
  }

  // ---------------------------------------------------------------------------
  // Ranking signals (search-as-scope Phase 3): the ⚙ popover's on/off toggles
  // ---------------------------------------------------------------------------

  /** Open/close the ⚙ "Ranking signals" popover. Opening it asks the NEXT
   * semantic search for per-signal debug (so the weights become VISIBLE while
   * tuning); closing it drops that debug — it is only paid while tuning. When
   * a semantic scope is already active, a re-commit applies the change so the
   * provenance hints appear (or vanish) immediately. */
  async setRankingPopover(open: boolean) {
    if (this.rankingPopoverOpen === open) return;
    this.rankingPopoverOpen = open;
    if (!open) this.resultDebug = new Map(); // stop showing debug once closed
    // Re-run only when a committed semantic scope is live: opening/closing the
    // popover must never touch the lexical keystroke path or kick off a query
    // where there is no scope (the quiet, queryless default stays quiet).
    if (this.gridScope.kind === "query" && this.searchLane === "semantic")
      await this.runQueryScope("semantic", false);
  }

  /** Arm/disarm the `~` fuzzy quiet-toggle (Phase 4) and persist. When a
   * LEXICAL query scope is live, re-run it so the widening appears (or vanishes)
   * immediately. NEVER re-runs the semantic lane: fuzzy is lexical-only, so a
   * committed semantic scope is left untouched (the next edit, which drops back
   * to lexical, will pick up the new state). */
  async setFuzzyMode(on: boolean) {
    if (this.fuzzyMode === on) return;
    this.fuzzyMode = on;
    prefs.saveFuzzy(on);
    if (this.gridScope.kind === "query" && this.searchLane === "lexical")
      await this.runQueryScope("lexical", false);
  }

  /** Flip one signal's checkbox and persist. A change re-runs the live
   * semantic scope so the new weights (and the excluded-signal effect) land
   * immediately — but ONLY for a committed semantic scope: toggles are
   * semantic-lane only and must never re-run the lexical as-you-type path. */
  async setSignal(key: SignalKey, on: boolean) {
    if (this.signalToggles[key] === on) return;
    this.signalToggles = { ...this.signalToggles, [key]: on };
    prefs.saveSignalToggles(this.signalToggles);
    if (this.gridScope.kind === "query" && this.searchLane === "semantic")
      await this.runQueryScope("semantic", false);
  }

  /** "Reset to defaults": every signal back on (the B75 defaults). Re-runs a
   * live semantic scope so the grid returns to the default fusion order. */
  async resetSignals() {
    const def = defaultToggles();
    if (isDefault(this.signalToggles)) return;
    this.signalToggles = def;
    prefs.saveSignalToggles(def);
    if (this.gridScope.kind === "query" && this.searchLane === "semantic")
      await this.runQueryScope("semantic", false);
  }

  // ---------------------------------------------------------------------------
  // drag-folder → register-root confirmation (featureset §6)
  // ---------------------------------------------------------------------------

  /** OS drop on the window: open the confirm sheet (App.svelte wires the
   * webview drag-drop event here). */
  offerDrop(paths: string[]) {
    if (paths.length === 0) return;
    this.dropPaths = paths;
    this.dropError = null;
  }

  cancelDrop() {
    this.dropPaths = null;
    this.dropError = null;
  }

  /** Confirm: register each dropped folder; the first one opens. A path
   * the backend refuses (not a directory, offline …) reports one inline
   * line and keeps the sheet open — honest, never a toast (§7.5). */
  async confirmDrop() {
    const paths = this.dropPaths;
    if (paths === null) return;
    // Same optimistic bridge as the picker flow: registration kicks an
    // initial scan, and the empty state must say so immediately.
    this.shell.expectIngest(INGEST_EXPECT_TIMEOUT_MS);
    let first: string | null = null;
    for (const path of paths) {
      try {
        const outcome = await ipc.addRoot(path);
        // Refuse + alias (folder-tree improvements): a dropped folder that
        // overlaps an existing active root is NOT re-ingested. Treat it as a
        // navigation target (the first one opens below) rather than an error.
        first ??=
          outcome.kind === "added"
            ? outcome.root.rootId
            : outcome.existingRootId;
      } catch (e) {
        this.dropError = e instanceof Error ? e.message : String(e);
        // Only a FULLY refused drop stands the bridge down — once one
        // root registered, its scan is real and the events will clear it.
        if (first === null) this.shell.clearIngestExpecting();
        return;
      }
    }
    this.dropPaths = null;
    await this.refreshRoots();
    if (first !== null) await this.openFolder(first, "");
  }

  // ---------------------------------------------------------------------------
  // capture: notes, ratings, auto-advance
  // ---------------------------------------------------------------------------

  summonNote() {
    this.shell.summonNote();
  }

  cancelNote() {
    this.shell.cancelNote();
  }

  async submitNote(text: string) {
    const { state, scope } = note.submit(this.shell.note);
    this.shell.note = state; // vanishes immediately (UI §6)
    const committed = await ipc.addNote(text);
    if (committed) {
      // The pop move (founder: "which is cool"): the shipped note flashes
      // a rising chip from the station, named with its summon-time scope.
      this.shell.stationPop(
        `Noted - ● ${scopeLabel(scope?.kind ?? "session", scope?.count ?? 0)}`,
      );
      await this.refreshInspectorIfTargeted();
      // A remark lights the has-journal dot (B37): refresh badges live too.
      await this.refreshItems();
      await this.advanceAfter("note");
    }
  }

  async rate(value: number) {
    // Session scope: rating keys do nothing (CAPTURE §10).
    if (this.shell.scope.kind === "session") return;
    const committed = await ipc.setRating(value);
    if (committed) {
      await this.refreshInspectorIfTargeted();
      await this.advanceAfter("rating");
    }
  }

  /** Journal-panel composer (BACKLOG "compose entries from the journal
   * panel"): a typed remark bound to the PANEL's image as its single
   * explicit target — a deliberate panel-context variant of CAPTURE §4's
   * submit-time scope (the N transient keeps the scope rule; the composer
   * never consults the grid write-scope). Resolves true iff committed
   * (the component clears its draft on commit only). Never advances —
   * composing in the panel is reading-side work, not the §4 heartbeat. */
  async composeRemark(text: string): Promise<boolean> {
    const hash = this.inspector.hash;
    if (hash === null || text.trim() === "") return false;
    try {
      const committed = await ipc.addNote(text, hash);
      if (committed) await this.refreshJournalFor(hash);
      return committed;
    } catch {
      return false; // backend unavailable: the draft survives
    }
  }

  /** Composer rating (0–5, 0 clears, same fold): bound to the panel's
   * image like composeRemark — always rates (never session scope). */
  async composeRating(value: number) {
    const hash = this.inspector.hash;
    if (hash === null) return;
    try {
      if (await ipc.setRating(value, hash)) await this.refreshJournalFor(hash);
    } catch {
      /* backend unavailable */
    }
  }

  /** The inspector is LIVE: any commit that targets the inspected image
   * re-folds its journal/metadata immediately (founder dogfood, round 1) —
   * never wait for an active-image change. */
  private async refreshInspectorIfTargeted() {
    const hash = this.inspector.hash;
    if (this.inspector.open === false || hash === null) return;
    const targets = scopeTargets({
      viewMode: this.viewMode,
      searchOpen: false,
      gridSelection: this.grid.selectionTargets,
      searchSelection: [],
      lookTargets: this.look.currentTargets,
      viewSelection: this.viewSelection,
    });
    if (targets.includes(hash)) await this.inspector.load(hash);
  }

  /** Auto-advance wiring (logic/advance.ts): multi-select rating never
   * advances or destroys the selection. */
  private async advanceAfter(commit: "rating" | "note") {
    const outcome = afterCommit({
      autoAdvance: this.autoAdvance,
      // advance.ts keeps its "grid"|"look" field (minimal blast radius); the
      // visualizer advances like the grid (DESIGN-VIEW-MODES.md).
      surface: this.viewMode === "look" ? "look" : "grid",
      commit,
      selectionCount: this.viewMode === "look" ? 1 : this.grid.sel.order.length,
    });
    if (outcome === "look-next") await this.lookNav(1);
    else if (outcome === "grid-next" && this.grid.advanceActive())
      await this.reportScope();
  }

  toggleAutoAdvance() {
    this.autoAdvance = !this.autoAdvance;
    prefs.saveAutoAdvance(this.autoAdvance);
  }

  // ---------------------------------------------------------------------------
  // Tab lights-out (featureset §0) — cross-slice, so the flow lives here
  // ---------------------------------------------------------------------------

  /** SNAPSHOT-RESTORE (founder, June 12 2026): hiding records WHICH
   * panels were open and closes them; the next Tab restores exactly that
   * set — never a fixed default. The closes go AROUND the toggle methods
   * on purpose: lights-out must not rewrite panel prefs (a quit while
   * hidden keeps the user's standing intent) and must not steal rail
   * focus. EXEMPT by ruling (DECISIONS): the capture indicator and an
   * open note input stay visible — neither is a panel, so neither enters
   * the snapshot. */
  async toggleLightsOut() {
    if (!this.shell.chromeHidden) {
      this.shell.panelSnapshot = {
        rail: this.shell.railOpen,
        inspector: this.inspector.open,
        filmstrip: this.look.filmstrip,
      };
      this.shell.railOpen = false;
      this.shell.railFocused = false;
      // close() (not a bare flag flip): unmounting the composer fires no
      // blur, and the inline-edit/redaction substates must not go stale.
      this.inspector.close();
      this.look.filmstrip = false;
      this.shell.chromeHidden = true;
    } else {
      const snap = this.shell.panelSnapshot;
      this.shell.panelSnapshot = null;
      this.shell.chromeHidden = false;
      if (snap !== null) {
        this.shell.railOpen = snap.rail;
        this.look.filmstrip = snap.filmstrip;
        if (snap.inspector !== false) {
          this.inspector.openTab(snap.inspector);
          await this.inspector.load(this.actionContext().activeHash);
        }
      }
    }
    // macOS: the traffic lights are NATIVE NSButtons (Overlay titlebar) —
    // DOM gates can't reach them, and left visible they'd float over
    // (and click-block) the chrome-less grid. Hide/show them in lockstep.
    // Nothing persists: lib.rs strips DECORATIONS from the window-state
    // flags, so a quit during lights-out still relaunches with chrome.
    if (isMac()) {
      try {
        await ipc.setTrafficLightsHidden(this.shell.chromeHidden);
      } catch {
        /* tests / non-tauri dev */
      }
    }
  }

  // ---------------------------------------------------------------------------
  // grease pencil flows (P5.1 — CAPTURE §8.4–8.6; PencilOverlay is the glue)
  // ---------------------------------------------------------------------------

  /** Pen-up commit: mint the stroke bound to the VIEWED image (never the
   * scope ring buffer), push the undo stack, refresh journal evidence.
   * The backend echoes the session the stroke landed in — the stack is
   * session-scoped (§8.5), so a rotation starts a fresh stack. The
   * indicator pulse rides the backend's existing channel. */
  async commitStroke(payload: StrokePayloadWire) {
    const hash = this.look.currentHash;
    if (hash === null) return;
    let committed: { id: string; sessionId: string };
    try {
      committed = await ipc.addStroke(hash, payload);
    } catch {
      return; // backend unavailable: the mark was never an event
    }
    this.look.onStrokeCommitted(committed.id, hash, committed.sessionId);
    await this.refreshJournalFor(hash);
  }

  /** Eraser tap: whole-stroke retraction through the EXISTING tombstone
   * path (§8.6) — no toast; the pulse is the feedback (the journal panel's
   * Retract row keeps its toast+Undo). */
  async eraseStroke(eventId: string) {
    const hash = this.look.currentHash;
    try {
      if (!(await ipc.retractEvent(eventId))) return;
    } catch {
      return;
    }
    this.look.onStrokeRetracted(eventId);
    if (hash !== null) await this.refreshJournalFor(hash);
  }

  /** Ctrl+Z (§8.5): during pen-down, cancel the in-flight stroke (local,
   * unlogged — UI §4.4); otherwise mint a retraction for the most recent
   * stacked stroke. Empty stack = no-op (the def's enabled gate already
   * keeps the chord unswallowed). NO redo in v1.
   *
   * The stack is session-scoped and session closure is LAZY (the
   * 30-minute boundary applies at the next activity — CAPTURE §2.2), so
   * this very keypress may BE the close: report it as activity first and
   * let the echoed session id arbitrate. A rotated session empties the
   * stack and the press mints nothing — prior-session strokes are
   * retracted via the journal panel or eraser, never Ctrl+Z. */
  async pencilUndo() {
    if (this.look.penDown) {
      this.look.requestPenCancel();
      return;
    }
    if (this.look.peekUndo() === null) return;
    try {
      this.look.syncUndoSession(await ipc.reportActivity());
    } catch {
      return; // backend unavailable: the session is unprovable, mint nothing
    }
    const top = this.look.peekUndo();
    if (top === null) return; // the stack belonged to a closed session
    try {
      if (!(await ipc.retractEvent(top.id))) return;
    } catch {
      return;
    }
    this.look.onStrokeRetracted(top.id);
    await this.refreshJournalFor(top.hash);
  }

  /** Stroke mutations change journal truth: re-fold an inspector showing
   * that image and refresh grid badges (the has-journal dot). */
  private async refreshJournalFor(hash: string) {
    if (this.inspector.open !== false && this.inspector.hash === hash)
      await this.inspector.load(hash);
    await this.refreshItems();
  }

  /** Backend `journal-changed` (BACKLOG): journal mutations announce their
   * affected hashes from the Rust side, so open surfaces refresh without a
   * frontend-triggered reload — the catch-all for writers WITHOUT a UI
   * action (M2b voice) and for cross-window mutations. The M1 writers'
   * direct refresh hooks above remain: they are awaited and deterministic
   * (tests run without a Tauri event loop); the duplicate fetch this
   * implies for same-window writes is absorbed by the inspector slice's
   * stale-response guard. The Look overlay's strokesVersion bump MIGRATED
   * here from the indicator-pulse heuristic — hash-aware now. */
  async onJournalChanged(hashes: string[]) {
    const affected = new Set(hashes);
    if (this.look.currentHash !== null && affected.has(this.look.currentHash))
      this.look.strokesVersion += 1; // overlay re-folds its strokes
    const hash = this.inspector.hash;
    if (this.inspector.open !== false && hash !== null && affected.has(hash))
      await this.inspector.load(hash);
    // Grid badges (has-journal dot, folded rating data) re-list only when
    // an affected image is in the current folder.
    if (this.grid.items.some((i) => affected.has(i.hash)))
      await this.refreshItems();
  }

  /** Select-from-note (BACKLOG): jump home and select the entry's FULL
   * target set in the grid — selection order = event_targets.position
   * (CAPTURE §3). Stack-aware: a target that is a collapsed pair's hidden
   * member selects the pair's cell (once, both members re-report through
   * selectionTargets). Targets outside the current folder (multi-target
   * notes minted over search selections can span folders) are skipped. */
  async selectJournalTargets(targets: string[]) {
    // Select-from-note jumps HOME and selects in the grid (DESIGN-VIEW-MODES.md:
    // viewMode="grid"). From Look we also tear down the single-image view; the
    // visualizer just drops its view (its selection is replaced by the grid one).
    if (this.viewMode === "look") this.look.close();
    this.viewMode = "grid";
    this.viewSelection = null;
    const order: string[] = [];
    for (const t of targets) {
      const unit = this.grid.units.find(
        (u) => u.primary.hash === t || u.alt?.hash === t,
      );
      if (unit !== undefined && !order.includes(unit.primary.hash))
        order.push(unit.primary.hash);
    }
    if (order.length === 0) {
      await this.reportScope(); // home, selection untouched
      return;
    }
    const focus = this.grid.unitHashes.indexOf(order[0]);
    await this.applySelection({ order, focus, anchor: focus });
  }

  // ---------------------------------------------------------------------------
  // Escape — the 15-layer order (logic/escape.ts)
  // ---------------------------------------------------------------------------

  escapeContext(): EscapeContext {
    return {
      welcomeCardOpen: this.shell.welcomeOpen,
      redactionModalOpen: this.inspector.redactTargetId !== null,
      dropConfirmOpen: this.dropPaths !== null,
      contextMenuOpen: this.shell.contextMenu !== null,
      journalEditOpen: this.inspector.editingEventId !== null,
      journalComposerFocused: this.inspector.composerFocused,
      noteInputOpen: this.shell.note.open,
      cheatsheetOpen: this.shell.cheatsheetOpen,
      // The station's pinned detail rides the popover's escape layer: one
      // expansion, one peel — hover-open and pin-open close on the same Esc.
      indicatorPopoverOpen: this.shell.popoverOpen || this.shell.stationPinned,
      debugPanelOpen: this.shell.debugOpen,
      rankingPopoverOpen: this.rankingPopoverOpen,
      inspectorOpen: this.inspector.open !== false,
      // A derived scope (query OR similar OR topic) is what first Esc / the
      // residue peels back to source — similar and topic scopes share the query
      // residue's one-key clear layer.
      queryScopeActive:
        this.gridScope.kind === "query" ||
        this.gridScope.kind === "similar" ||
        this.gridScope.kind === "topic",
      searchBarFocused: this.barFocused,
      viewMode: this.viewMode,
      hasSelection: this.grid.sel.order.length > 0,
    };
  }

  async escape() {
    switch (escapeAction(this.escapeContext())) {
      case "close-welcome-card":
        // Honors the card's "don't show again" toggle — Esc is a real
        // dismissal, never a trap that resurrects the card every launch.
        this.shell.dismissWelcome();
        break;
      case "close-redaction-modal":
        this.inspector.redactTargetId = null;
        break;
      case "close-drop-confirm":
        this.cancelDrop();
        break;
      case "close-context-menu":
        this.shell.closeContextMenu();
        break;
      case "close-journal-edit":
        this.inspector.editingEventId = null;
        break;
      case "blur-journal-composer":
        // Exit text-input focus first (§0); the input's blur handler keeps
        // the flag honest, but a headless context (tests) has no element.
        (document.activeElement as HTMLElement | null)?.blur?.();
        this.inspector.composerFocused = false;
        break;
      case "close-note-input":
        this.cancelNote();
        break;
      case "close-cheatsheet":
        this.shell.cheatsheetOpen = false;
        break;
      case "close-indicator-popover":
        this.shell.closeStation(); // hover AND pin — one dismissal
        break;
      case "close-debug-panel":
        this.shell.debugOpen = false;
        break;
      case "close-ranking-popover":
        // Esc peels the ⚙ popover like every other transient — through the
        // same funnel as its dismiss, so debug + state clean up together.
        await this.setRankingPopover(false);
        break;
      case "close-inspector":
        this.inspector.close();
        break;
      case "clear-query-scope":
        // First Esc with a query active: drop the scope, return to source.
        // The bar keeps focus — a second Esc then blurs (the design's
        // two-press sequence). The grid is the same surface throughout.
        await this.clearQueryScope();
        break;
      case "blur-search-bar":
        // Second Esc (or first when there was only an uncommitted query):
        // exit the input's focus (§0). The blur handler flips barFocused.
        (document.activeElement as HTMLElement | null)?.blur?.();
        this.barFocused = false;
        break;
      case "leave-look":
        await this.leaveLook();
        break;
      case "clear-selection":
        await this.applySelection(sel.clear(this.grid.sel));
        break;
      case "none":
        break; // never quits
    }
  }

  // ---------------------------------------------------------------------------
  // ActionContext snapshot (keymap dispatch + menus + cheatsheet)
  // ---------------------------------------------------------------------------

  actionContext(input?: {
    inputFocused: boolean;
    searchInputFocused: boolean;
  }): ActionContext {
    return {
      viewMode: this.viewMode,
      // Search is no longer a surface/scope (M3): there is no "search open"
      // keymap mode. Held false so the frozen ActionContext contract still
      // type-checks for any residual reader. The header bar handles its own
      // input keys (Enter/Backspace) locally, the way a focused text input
      // should — not through a search-scope keymap layer.
      searchOpen: false,
      inputFocused: input?.inputFocused ?? false,
      searchInputFocused: input?.searchInputFocused ?? false,
      queryEmpty: this.query === "",
      hasSelection: this.grid.sel.order.length > 0,
      selectionCount: this.grid.sel.order.length,
      // The shared active image (DESIGN-VIEW-MODES.md): the one getter funnels
      // all three view arms (Look current / visualizer selection / grid focus).
      activeHash: this.activeHash,
      activeIsPair: this.grid.activeIsPair,
      activePairCollapsed: this.grid.activePairCollapsed,
      railOpen: this.shell.railOpen,
      railFocused: this.shell.railFocused,
      inspectorOpen: this.inspector.open,
      cheatsheetOpen: this.shell.cheatsheetOpen,
      contextMenuOpen: this.shell.contextMenu !== null,
      chromeHidden: this.shell.chromeHidden,
      autoAdvance: this.autoAdvance,
      debugEnabled: this.debugEnabled,
      asrReady: this.shell.asrReady, // live from runtime-status (P6.2, §8.3)
      sort: this.grid.sort,
      // The `relevance` sort row is offered for any RELEVANCE-ORDERED scope:
      // a query (fused order), a similar scope (similarity order), OR a topic
      // scope (ranked affinity order). All feed the grid in a backend-chosen
      // order that "relevance" preserves.
      queryActive:
        this.gridScope.kind === "query" ||
        this.gridScope.kind === "similar" ||
        this.gridScope.kind === "topic",
      heatOn: this.heatOn,
      heatAllTime: this.heatAllTime,
      thumbStep: this.grid.thumbStep,
      surround: this.shell.surround,
      filmstrip: this.look.filmstrip,
      histogram: this.look.histogram,
      pencilMode: this.look.pencilMode,
      overlayVisible: this.look.overlayVisible,
      pencilUndoable: this.look.penDown || this.look.undoStack.length > 0,
      micArmed: this.shell.mic === "armedIdle" || this.shell.mic === "armedSpeaking",
      micState: this.shell.mic,
      asrUnavailable: this.shell.asrUnavailable,
      collections: this.collections.map((c) => ({ id: c.id, name: c.name })),
      activeMemberships: this.activeMemberships,
    };
  }

  // ---------------------------------------------------------------------------
  // THE perform sink — every Action from keys, menus, and pointer wiring
  // lands here (dispatch tested in logic/keymap + actions/match)
  // ---------------------------------------------------------------------------

  async perform(action: Action) {
    switch (action.kind) {
      // ---- contract ---------------------------------------------------------
      case "escape":
        await this.escape();
        break;
      case "go-grid":
        await this.goHome();
        break;
      case "toggle-graph":
        // `l` toggles the visualizer (DESIGN-VIEW-MODES.md transition table):
        // grid->visualizer, visualizer->grid, look->visualizer. From look,
        // openVisualizer leaves Look first and seeds from look.currentHash, so
        // the photo carries across.
        if (this.viewMode === "visualizer") await this.leaveVisualizer();
        else await this.openVisualizer();
        break;
      case "toggle-lights-out":
        await this.toggleLightsOut();
        break;
      case "toggle-rail":
        this.shell.toggleRail();
        break;
      case "toggle-cheatsheet":
        this.shell.toggleCheatsheet();
        break;
      case "open-search":
        // `/` and Cmd+F FOCUS the always-visible header bar now (M3) — no
        // overlay to open. The bar's source is whatever the grid currently
        // shows; a query scopes within it.
        this.focusBar();
        break;
      case "summon-note":
        this.summonNote();
        break;
      case "toggle-auto-advance":
        this.toggleAutoAdvance();
        break;
      case "toggle-fullscreen": {
        this.shell.fullscreen = !this.shell.fullscreen;
        try {
          const { getCurrentWindow } = await import("@tauri-apps/api/window");
          await getCurrentWindow().setFullscreen(this.shell.fullscreen);
        } catch {
          /* tests / non-tauri dev */
        }
        break;
      }
      case "ui-zoom":
        this.shell.stepUiZoom(action.delta);
        await this.applyUiZoom();
        break;
      case "ui-zoom-reset":
        this.shell.resetUiZoom();
        await this.applyUiZoom();
        break;
      case "open-settings":
        await ipc.openSettingsWindow();
        break;
      case "quit":
        await ipc.quit();
        break;
      case "toggle-debug-panel":
        this.shell.debugOpen = !this.shell.debugOpen;
        break;
      case "rate":
        await this.rate(action.value);
        break;
      case "set-surround":
        this.shell.setSurround(action.level);
        break;
      // ---- grid -------------------------------------------------------------
      case "open-look": {
        // ONE path now (M3): query results are grid cells, so Enter on the
        // focused cell opens Look over the grid units — whatever scope the
        // grid is in.
        const hash = this.grid.unitHashes[this.grid.sel.focus];
        if (hash !== undefined) await this.openLook(hash);
        break;
      }
      case "focus-move":
        await this.applySelection(
          sel.moveFocus(
            this.grid.sel,
            this.grid.unitHashes,
            this.grid.gridCols,
            action.dir,
            action.extend,
          ),
        );
        break;
      case "focus-edge":
        await this.applySelection(
          sel.moveEdge(this.grid.sel, this.grid.unitHashes, action.edge, action.extend),
        );
        break;
      case "focus-page":
        await this.applySelection(
          sel.movePage(
            this.grid.sel,
            this.grid.unitHashes,
            this.grid.gridCols,
            this.grid.gridRowsPerPage,
            action.dir,
            action.extend,
          ),
        );
        break;
      case "toggle-select-focused":
        await this.applySelection(
          sel.toggle(this.grid.sel, this.grid.unitHashes, this.grid.sel.focus),
        );
        break;
      case "select-all":
        await this.applySelection(sel.selectAll(this.grid.sel, this.grid.unitHashes));
        break;
      case "select-none":
        await this.applySelection(sel.selectNone(this.grid.sel));
        break;
      case "open-sort-menu":
        // Keyboard-summoned: no pointer anchor; the host picks a default.
        if (this.shell.contextMenu?.seat === "sort") this.shell.closeContextMenu();
        else this.shell.openContextMenu("sort", null);
        break;
      case "set-sort":
        this.grid.setSort(action.mode);
        break;
      case "thumb-size":
        this.grid.setThumbStep(this.grid.thumbStep + action.delta);
        break;
      case "set-thumb-step":
        this.grid.setThumbStep(action.step);
        break;
      case "cycle-cell-info":
        this.grid.cycleCellInfo();
        break;
      case "stack-toggle-active":
        this.grid.toggleActiveStack();
        await this.reportScope();
        break;
      case "stack-collapse-all":
        this.grid.setAllStacks(true);
        await this.reportScope();
        break;
      case "stack-expand-all":
        this.grid.setAllStacks(false);
        await this.reportScope();
        break;
      case "flip-stack-member":
        if (this.viewMode === "look") this.look.flipMember();
        else this.grid.flipActiveMember();
        await this.reportScope();
        break;
      // ---- look -------------------------------------------------------------
      case "look-nav":
        await this.lookNav(action.delta);
        break;
      case "look-close":
        await this.leaveLook();
        break;
      case "zoom-toggle":
        this.look.sendZoom("toggle");
        break;
      case "zoom-step":
        this.look.sendZoom("step", action.delta);
        break;
      case "zoom-fit":
        this.look.sendZoom("fit");
        break;
      case "zoom-100":
        this.look.sendZoom("actual");
        break;
      case "toggle-filmstrip":
        this.look.toggleFilmstrip();
        break;
      case "toggle-histogram":
        this.look.toggleHistogram();
        break;
      // ---- attention heatmap (DESIGN-ATTENTION-HEATMAP.md) ------------------
      case "toggle-heat":
        this.toggleHeat();
        break;
      case "toggle-attention-all-time":
        this.toggleAllTime();
        break;
      // ---- panels -----------------------------------------------------------
      case "rail-nav": {
        // Arrow routing follows the VISIBLE tab: collections are a flat
        // list (no expand/collapse), so left/right are no-ops there.
        if (this.shell.railTab === "collections") {
          if (action.dir === "up" || action.dir === "down")
            this.shell.railFocusKey = railMoveFocus(
              this.railCollectionRows(),
              this.shell.railFocusKey,
              action.dir,
            );
          break;
        }
        const rows = flatRows(this.railSections());
        if (action.dir === "up" || action.dir === "down") {
          this.shell.railFocusKey = railMoveFocus(
            rows,
            this.shell.railFocusKey,
            action.dir,
          );
        } else {
          const row = rows.find((r) => r.key === this.shell.railFocusKey);
          if (row !== undefined) {
            // toggleExpand now tracks BOTH sets (deep-tree ergonomics): an
            // explicit expand survives the auto-collapse depth.
            const next = toggleExpand(
              this.shell.railCollapsed,
              this.shell.railExpanded,
              row,
              action.dir,
            );
            this.shell.railCollapsed = next.collapsed;
            this.shell.railExpanded = next.expanded;
          }
        }
        break;
      }
      case "rail-enter": {
        if (this.shell.railTab === "collections") {
          const row = this.railCollectionRows().find(
            (r) => r.key === this.shell.railFocusKey,
          );
          if (row !== undefined) {
            await this.openCollection(row.id);
            this.shell.railFocused = false; // focus returns to the grid
          }
          break;
        }
        const rows = flatRows(this.railSections());
        const row = rows.find((r) => r.key === this.shell.railFocusKey);
        if (row !== undefined) {
          await this.openFolder(row.rootId, row.folder);
          this.shell.railFocused = false; // focus returns to the grid
        }
        break;
      }
      case "collection-open":
        await this.openCollection(action.id);
        break;
      case "add-to-collection": {
        // Multi-select adds the WHOLE selection, stack-expanded (a
        // collapsed pair contributes both members — the CAPTURE §3 target
        // rule); the thumb right-click already selected an unselected cell.
        // NO catch around the write: gathering is user truth (RETRIEVAL
        // §10.2) and a real persistence failure must not vanish silently
        // — the rejection propagates like rate()'s (the swallow idiom is
        // reserved for fire-and-forget OS verbs).
        const targets = this.collectionTargets();
        if (targets.length === 0) break;
        await ipc.addToCollection(action.id, targets);
        // Direct refresh (awaited, deterministic for tests); the
        // collections-changed event is the cross-window catch-all.
        await this.onCollectionsChanged((await ipc.listCollections()) ?? []);
        break;
      }
      case "new-collection-add": {
        // Capture the targets NOW, synchronously, before any await or UI
        // round-trip — the same stack-expanded selection the add verb uses.
        // With nothing to add there is no collection to mint for: bail (the
        // def gates on hasSelection || activeHash, so this is belt-and-braces).
        const targets = this.collectionTargets();
        if (targets.length === 0) break;
        // Hand the targets to the rail's inline creator (the ONE create UX);
        // its commit calls createCollectionAndAdd, closing the create+add
        // chain. The context menu is already closing around this dispatch.
        this.shell.beginNewCollectionWithTargets(targets);
        break;
      }
      case "remove-from-collection": {
        // The add verb's mirror: membership is evented, never destructive
        // (§10.1) — removal closes the open intervals and history stays.
        // Same no-catch rule: closing membership is user truth too.
        const targets = this.collectionTargets();
        if (targets.length === 0) break;
        await ipc.removeFromCollection(action.id, targets);
        await this.onCollectionsChanged((await ipc.listCollections()) ?? []);
        break;
      }
      case "rail-folder-open":
        await this.openFolder(action.rootId, action.folder);
        break;
      case "rail-folder-reveal":
        try {
          await ipc.revealFolder(action.rootId, action.folder);
        } catch {
          /* body lands with Stage A (os.rs) */
        }
        break;
      case "rescan-root":
        // The add-root optimistic bridge applies here too: a rescan's
        // walk has the same dark window before its first status emit. The
        // watchdog matters MOST here — a zero-change / deleted-path rescan
        // returns Ok but may emit no progress at all (the §6e strand).
        this.shell.expectIngest(INGEST_EXPECT_TIMEOUT_MS);
        try {
          await ipc.rescanRoot(action.rootId);
        } catch {
          /* unreachable backend in tests */
          this.shell.clearIngestExpecting();
        }
        break;
      case "rebuild-previews":
        // Fire-and-forget like Rescan: the pump regenerates in the
        // background; thumbs heal off `previews-changed` (no progress UI —
        // the ingest indicator already shows queue depth).
        try {
          await ipc.rebuildPreviews(action.rootId);
        } catch {
          /* unreachable backend in tests */
        }
        break;
      case "archive-root":
        // Non-destructive hide (folder-tree improvements): the root leaves
        // the active rail but its journal + memberships stay; restorable
        // from the Archived affordance.
        try {
          await this.archiveRoot(action.rootId);
        } catch {
          /* unreachable backend in tests */
        }
        break;
      case "unarchive-root":
        try {
          await this.unarchiveRoot(action.rootId);
        } catch {
          /* unreachable backend in tests */
        }
        break;
      case "add-root":
        await this.addRootFromPicker();
        break;
      case "open-inspector":
        this.inspector.openTab(action.tab);
        await this.inspector.load(this.actionContext().activeHash);
        break;
      case "close-inspector":
        this.inspector.close();
        break;
      // ---- journal row verbs (Stage C wires the flows) ------------------------
      case "journal-correct":
        this.inspector.editingEventId = action.eventId;
        break;
      case "journal-retract":
        // Retract → toast+Undo (RE-STATE — the slice owns the flow). This
        // dispatch case is the ONLY route for the journal panel's Retract
        // row: a committed retraction must also leave the Look overlay and
        // the pencil undo stack (CAPTURE §8.5 — otherwise Ctrl+Z targets
        // an already-retracted stroke); for non-strokes the cleanup is a
        // cheap no-op refetch.
        if (await this.inspector.retract(action.eventId))
          this.look.onStrokeRetracted(action.eventId);
        break;
      case "journal-redact":
        this.inspector.beginRedact(action.eventId); // → the one modal
        break;
      case "journal-toggle-retracted":
        this.inspector.showRetracted = !this.inspector.showRetracted;
        break;
      case "select-journal-targets":
        await this.selectJournalTargets(action.targets);
        break;
      // ---- OS integration (D4; bodies land with Stage A) ----------------------
      case "reveal-in-file-manager": {
        const hash = this.actionContext().activeHash;
        if (hash !== null)
          try {
            await ipc.revealInFileManager(hash);
          } catch {
            /* body lands with Stage A (os.rs) */
          }
        break;
      }
      case "copy-file-path": {
        const hash = this.actionContext().activeHash;
        if (hash !== null)
          try {
            const paths = await ipc.imageAbsPath(hash);
            // The shared register confirms the write (the menu row's
            // check flashes off this key — BACKLOG "Copy actions
            // confirm themselves"). The key is rebuilt from the action
            // kind (the def id) + the hash so it matches the row's
            // flashKey by construction, and a selection change inside
            // the flash window cannot relight the check on another image.
            if (paths.absPath !== null)
              await copyToClipboard(copyKey(action.kind, hash), paths.absPath);
          } catch {
            /* offline volume / unreachable backend: quiet no-op */
          }
        break;
      }
      case "open-with-default": {
        const hash = this.actionContext().activeHash;
        if (hash !== null)
          try {
            await ipc.openWithDefault(hash);
          } catch {
            /* body lands with Stage A (os.rs) */
          }
        break;
      }
      case "open-in-external-editor": {
        // Hand the original off to the configured editor (or the OS default
        // when the pref is empty). Silent catch like the sibling OS verbs
        // reveal/open-with-default: toast.svelte.ts is a closed 3-kind enum
        // (spec §7.5 guardrail), so an offline-volume / unreachable-backend
        // failure is a quiet no-op, never a toast.
        const hash = this.actionContext().activeHash;
        if (hash !== null)
          try {
            await ipc.openInExternalEditor(hash);
          } catch {
            /* offline volume / unreachable backend: quiet no-op */
          }
        break;
      }
      case "find-similar": {
        // "More like this": re-scope the grid to the active image's visual
        // neighbors. Triggered from the grid thumb menu OR the Look backdrop;
        // leave Look first so the user lands back on the grid looking at the
        // result set (the same surface the query scope renders in). The
        // filename is captured here for the residue; an empty/absent name
        // still reads fine ("similar to image"). A failed neighbor search is
        // a quiet no-op like the sibling OS verbs — never a toast.
        const hash = this.actionContext().activeHash;
        if (hash !== null) {
          if (this.viewMode === "look") await this.leaveLook();
          const filename = this.filenameFor(hash);
          try {
            await this.runSimilarScope(hash, filename);
          } catch {
            /* unreachable backend / empty index: quiet no-op */
          }
        }
        break;
      }
      // ---- search bar (M3 search-as-scope) ------------------------------------
      // search-nav / search-open-result are RETIRED: results are grid cells,
      // so grid focus-move + Enter (open-look) drive them — there is no
      // parallel result cursor. The union members stay (frozen, never
      // narrowed) but dispatch to nothing; their defs are gone, so no key
      // reaches them.
      case "search-nav":
      case "search-open-result":
        break;
      case "remove-last-chip":
        // Backspace on an empty bar input drops the last chip and re-scopes
        // live (the bar component dispatches this; the empty-input guard is
        // in the bar's keydown, mirroring the old def's `enabled`).
        if (this.query === "" && this.chips.length > 0)
          await this.removeChip(this.chips.length - 1);
        break;
      // ---- grease pencil (P5.1 — CAPTURE §8, UI §4.4) --------------------------
      case "pencil-pen":
        this.look.togglePencil();
        break;
      case "pencil-eraser":
        // Hold engages here (auto-repeat re-engages harmlessly); the
        // release is LookStage's raw keyup (the registry is keydown-only).
        this.look.eraserHeld = true;
        break;
      case "cycle-overlay":
        this.look.toggleOverlay();
        break;
      case "pencil-undo":
        await this.pencilUndo();
        break;
      case "journal-flash-stroke":
        if (this.viewMode === "look") this.look.flashStroke(action.eventId);
        break;
      // ---- voice capture (P6.4 — CAPTURE §6.4, §11; Space two-gesture) ---------
      case "toggle-mic":
        // The instantaneous pointer form (indicator segment click via
        // resolveAction arg "toggle"): a click IS a tap, plain toggle.
        this.shell.onIndicatorState(await ipc.toggleMic());
        break;
      case "mic-press": {
        // Space keydown — the two-gesture machine begins (logic/michold.ts):
        // from disarmed the mic arms NOW (both gestures want sound
        // flowing from the press; a PTT hold must not lose the utterance
        // onset), from armed nothing happens yet — the release decides.
        // Auto-repeat keydowns land here too and are absorbed by the
        // machine (the press timestamp survives, so the hold threshold
        // still measures from the FIRST press). The release half is a
        // raw window keyup in App.svelte — the hold-E precedent.
        const { state, intent } = micDown(this.micHold, {
          armed: this.shell.mic === "armedIdle" || this.shell.mic === "armedSpeaking",
          now: Date.now(),
        });
        this.micHold = state;
        if (intent === "arm") this.shell.onIndicatorState(await ipc.setMic(true));
        break;
      }
      // ---- the station's info seat: pin the expansion open --------------------
      case "toggle-station-detail":
        this.shell.toggleStationPinned();
        break;
    }
  }

  // ---------------------------------------------------------------------------
  // Space two-gesture mic — the release half (CAPTURE §6.4; machine in
  // logic/michold.ts). The registry is keydown-only, so App.svelte feeds
  // these as raw window facts (the hold-E precedent).
  // ---------------------------------------------------------------------------

  /** Tap-vs-hold tracker for the Space mic key. Plain field, not $state:
   * nothing renders from it — the indicator follows shell.mic, which the
   * IPC echoes drive. */
  micHold: MicHoldState = MIC_HOLD_IDLE;

  /** Raw Space keyup. Called UNCONDITIONALLY (even while typing, even after
   * the mic degraded mid-hold): the machine no-ops on a stray release and
   * set_mic is idempotent, so a hold can never wedge the mic open. A tap
   * from armed disarms here; a past-threshold release ships the PTT
   * utterance by disarming (the backend's disarm drain keeps trailing
   * finals — B52/B72 — so short bursts still land). */
  async micRelease() {
    const { state, intent } = micUp(this.micHold, Date.now());
    this.micHold = state;
    if (intent === "disarm") this.shell.onIndicatorState(await ipc.setMic(false));
  }

  /** Window blur mid-gesture: the keyup will never arrive, and a hot mic
   * the gesture opened must never wedge open (a mic armed BEFORE the
   * press stays armed — blur must not kill a deliberate toggle). */
  async micWindowBlur() {
    const { state, intent } = micBlur(this.micHold);
    this.micHold = state;
    if (intent === "disarm") this.shell.onIndicatorState(await ipc.setMic(false));
  }

  /** Rail rows over the shared roots/tree (logic/sources.ts providers).
   * Folder-tree improvements wire in here: the current root's tree is
   * FILTERED by the jump input (filterTree) and a filter raises the
   * auto-expand depth so every surviving match is reachable; without a
   * filter, deep folders auto-collapse past AUTO_EXPAND_DEPTH (lazy). */
  railSections() {
    const filter = this.shell.railFolderFilter;
    const filtering = filter.trim() !== "";
    const tree = filtering ? filterTree(this.tree, filter) : this.tree;
    return sections({
      roots: this.roots,
      tree,
      treeRootId: this.grid.rootId,
      collapsed: this.shell.railCollapsed,
      expanded: this.shell.railExpanded,
      // While filtering, show every match (Infinity beats any depth); else
      // honor the lazy cap so a deep root does not render its whole tree.
      autoExpandDepth: filtering ? Number.POSITIVE_INFINITY : Ui.AUTO_EXPAND_DEPTH,
    });
  }

  /** Type-to-filter the Folders tree + jump-to-folder. Sets the filter text
   * and, when there is a match, focuses the first one (and opens it so the
   * grid follows) — the "jump" half of the ergonomic win. A blank filter
   * just clears. Pure matching lives in logic/sources (filterTree /
   * firstMatchKey); this owns only the focus/open side effect. */
  setFolderFilter(text: string) {
    this.shell.railFolderFilter = text;
    const key = firstMatchKey(this.grid.rootId ?? "", this.tree, text);
    if (key !== null) this.shell.railFocusKey = key;
  }

  /** Enter in the filter input: jump to (open) the first matching folder. */
  async jumpToFilteredFolder() {
    const rows = flatRows(this.railSections());
    const row = rows.find((r) => r.key === this.shell.railFocusKey);
    if (row !== undefined) await this.openFolder(row.rootId, row.folder);
  }

  /** Archive a root (folder-tree improvements): non-destructive hide. The
   * `roots-changed` event refreshes the active list; we refresh the archived
   * snapshot here so the "Archived" affordance shows it immediately. */
  async archiveRoot(rootId: string) {
    await ipc.archiveRoot(rootId);
    await this.refreshRoots();
    this.archivedRoots = (await ipc.listArchivedRoots()) ?? [];
  }

  /** Restore an archived root to active, then open it (the rail drives the
   * grid surface, like a folder open). */
  async unarchiveRoot(rootId: string) {
    const root = await ipc.unarchiveRoot(rootId);
    await this.refreshRoots();
    this.archivedRoots = (await ipc.listArchivedRoots()) ?? [];
    await this.openFolder(root.rootId, "");
  }

  /** Rail rows for the Collections tab (logic/sources.ts provider). */
  railCollectionRows() {
    return collectionRows(this.collections);
  }

  // -- attention/engagement heatmap: dwell capture + heat-tint fetch ----------
  // (DESIGN-ATTENTION-HEATMAP.md) — a focused region kept localized for the
  // parallel semantic-graph merge. The cross-slice flows (openLook / leaveLook
  // / reportScope / window blur) call dwellRefocus / dwellPause; this owns the
  // episode lifecycle and the IPC report.

  /** Refocus the dwell tracker on whatever the user is now attending to: the
   * Look-viewed image (tier "look"), the visualizer's selected node (tier
   * "look" too — a selected node is a focused single image, DESIGN-VIEW-MODES.md
   * regression guard), or the grid selection (tier "grid"). Ends and flushes the
   * previous episode when the focus actually changed, then begins a fresh one. A
   * no-op re-report (same tier + hashes) keeps the current episode running, so a
   * steady Look-open accrues one continuous span. Called from every cross-slice
   * focus flow. */
  dwellRefocus() {
    let next: { source: DwellSource; hashes: string[] } | null;
    switch (this.viewMode) {
      case "look":
        next =
          this.look.currentHash === null
            ? null
            : { source: "look", hashes: [this.look.currentHash] };
        break;
      case "visualizer":
        // Single-image dwell on the selected node (full "look" weight), or null
        // when neutral (nothing selected — no attention to attribute).
        next =
          this.viewSelection === null
            ? null
            : { source: "look", hashes: [this.viewSelection] };
        break;
      case "grid":
        next =
          this.grid.sel.order.length > 0
            ? { source: "grid", hashes: this.grid.selectionTargets }
            : null;
        break;
    }
    // Unchanged focus: let the running episode keep accruing.
    if (sameFocus(this.dwellEpisode, next)) {
      this.armDwellIdle();
      return;
    }
    this.flushDwell();
    this.dwellEpisode =
      next === null ? null : beginEpisode(next.source, next.hashes, Date.now());
    this.armDwellIdle();
  }

  /** End + report the in-flight episode (leaving Look / deselect / switch /
   * window blur / idle). Fire-and-forget per focused hash: capture is light and
   * a dropped report just loses a little dwell (DESIGN). */
  flushDwell() {
    clearTimeout(this.dwellIdleTimer);
    const flushes = endEpisode(this.dwellEpisode, Date.now());
    this.dwellEpisode = null;
    for (const f of flushes) {
      void ipc.recordDwell(f.hash, f.source, f.elapsedMs).catch(() => {});
    }
  }

  /** Window blur / visibilitychange: pause dwell by flushing the current
   * episode (the app backgrounded — the user is no longer attending). Re-focus
   * on return re-begins a fresh episode via the next reportScope/flow. This +
   * the backend 60 s cap handle the walk-away case. */
  dwellPause() {
    this.flushDwell();
  }

  /** (Re)arm the idle-flush timer: a focus episode with no input for
   * IDLE_FLUSH_MS flushes (walk-away from the keyboard). App.svelte's activity
   * touch re-arms it; the harder guard is the backend's per-episode cap. */
  private armDwellIdle() {
    clearTimeout(this.dwellIdleTimer);
    if (this.dwellEpisode === null) return;
    this.dwellIdleTimer = setTimeout(() => this.flushDwell(), IDLE_FLUSH_MS);
  }

  /** Toggle the grid heat-tint (DESIGN §"Rendering"), persisted. Turning it on
   * fetches intensity for the current scope; off clears the cached map so no
   * cell glows. */
  toggleHeat() {
    this.heatOn = !this.heatOn;
    prefs.saveHeatOn(this.heatOn);
    // Force the next fetch regardless of the cached item signature (the scope
    // may be unchanged but the tint just turned on).
    this.heatItemsKey = "";
    if (this.heatOn) {
      void this.fetchIntensity();
    } else {
      this.setIntensity(new Map());
    }
  }

  /** Replace the intensity map and mirror it into the grid slice (where the
   * `attention` sort and the cell heat-tint read it). One funnel so the two
   * copies never drift. */
  private setIntensity(map: Map<string, number>) {
    this.intensity = map;
    this.grid.intensity = map;
  }

  /** Toggle the "All-time" recency switch (founder decision), persisted. Re-
   * fetches intensity with the new flag when the heat tint is showing. */
  toggleAllTime() {
    this.heatAllTime = !this.heatAllTime;
    prefs.saveHeatAllTime(this.heatAllTime);
    this.heatItemsKey = ""; // the flag changed: force a refetch
    if (this.heatOn) void this.fetchIntensity();
  }

  /** Fetch normalized intensity for the loaded grid scope and replace the map.
   * Guarded by a monotone token so a slow fetch cannot overwrite a newer
   * scope's. Silent on backend failure (tests/dev): the tint just stays dark. */
  async fetchIntensity() {
    if (!this.heatOn) return;
    // Key off the UNSORTED scope hashes so the `attention` sort can reorder by
    // the result without a fetch cycle (the sorted `items` depend on this map).
    const hashes = this.grid.scopeHashes;
    if (hashes.length === 0) {
      this.setIntensity(new Map());
      return;
    }
    const load = ++this.heatLoad;
    try {
      const scores = await ipc.imageIntensity(hashes, this.heatAllTime);
      if (load !== this.heatLoad) return; // a newer scope won
      this.setIntensity(new Map(scores.map((s) => [s.hash, s.intensity])));
    } catch {
      /* backend unavailable: leave the tint dark */
    }
  }

  // ---------------------------------------------------------------------------
  // Diversify / duplication-tolerance (DESIGN-DEDUP-AND-SIMILARITY.md)
  // ---------------------------------------------------------------------------

  /** Toggle the Diversify view filter, persisted. Turning it ON runs an
   * immediate (un-debounced) diversify pass for the current scope; turning it
   * OFF restores the full set by clearing the grid's shown filter and the hidden
   * count. The slider's tolerance is preserved across off/on so re-enabling
   * resumes where the reviewer left it. */
  toggleDiversify() {
    this.diversifyOn = !this.diversifyOn;
    prefs.saveDiversifyOn(this.diversifyOn);
    if (this.diversifyOn) {
      // Immediate on the explicit toggle (no drag to coalesce): the user asked
      // for the filter now. Force the next pass regardless of the cached scope
      // signature (the scope may be unchanged but the filter just turned on).
      this.diversifyScopeKey = "";
      void this.runDiversify();
    } else {
      this.applyDiversifyOff();
    }
  }

  /** Slider moved (0..100%): persist the value and re-run the pass, DEBOUNCED so
   * a continuous drag fires ONE backend pass on settle rather than one per
   * intermediate value (the diversify pass is far heavier than a keystroke
   * search). A no-op while the filter is off (the slider is only interactive when
   * on). */
  setDiversifyTolerance(percent: number) {
    this.diversifyTolerancePercent = percent;
    prefs.saveDiversifyTolerance(percent);
    if (!this.diversifyOn) return;
    clearTimeout(this.diversifyTimer);
    // Force a recompute on settle: the tolerance changed, so the cached scope
    // signature must not short-circuit it.
    this.diversifyScopeKey = "";
    this.diversifyTimer = setTimeout(() => void this.runDiversify(), DIVERSIFY_DEBOUNCE_MS);
  }

  /** Clear the Diversify filter: drop the grid's shown set (restoring the full
   * scope), zero the hidden count, and cancel any pending debounced pass. The
   * single funnel for "turn off" and "no signal to filter on". */
  private applyDiversifyOff() {
    clearTimeout(this.diversifyTimer);
    this.diversifyScopeKey = "";
    this.grid.diversifyShown = null;
    this.diversifyHidden = 0;
  }

  /** Re-run the active Diversify pass when the SCOPE's item-set changed since the
   * last pass (a cheap length + endpoints signature, the heat-tint precedent) —
   * reportScope fires far more often than the items change, and the shown set is
   * per-scope. Cheap no-op when the filter is off or the scope is unchanged.
   * Called from reportScope so the filter follows folder/collection/query/topic
   * switches automatically (the design's "Re-run when the scope changes while
   * active"). */
  private refreshDiversifyIfScopeChanged() {
    if (!this.diversifyOn) return;
    const h = this.grid.scopeHashes;
    const key = `${h.length}:${h[0] ?? ""}:${h[h.length - 1] ?? ""}`;
    if (key === this.diversifyScopeKey) return;
    this.diversifyScopeKey = key;
    void this.runDiversify();
  }

  /** Run a diversify_scope pass for the current scope at the current tolerance
   * and mirror the resulting `shown` set into the grid. Guarded by a monotone
   * token so a slow pass cannot overwrite a newer scope's / tolerance's result.
   * GRACEFUL like every lens command: an empty scope or a degraded rig clears the
   * filter (shows everything) and flags `diversifyDegraded` so the chrome can
   * explain why; a backend failure (tests/dev) leaves the filter cleared. */
  async runDiversify() {
    if (!this.diversifyOn) return;
    const hashes = this.grid.scopeHashes;
    if (hashes.length === 0) {
      // Nothing in scope: clear the filter and don't claim degradation (there is
      // simply nothing to diversify).
      this.grid.diversifyShown = null;
      this.diversifyHidden = 0;
      this.diversifyDegraded = false;
      return;
    }
    const tolerance = percentToTolerance(this.diversifyTolerancePercent);
    const load = ++this.diversifyLoad;
    try {
      // Reuse the SAME GraphScope the visualizer/topic commands resolve, so the
      // backend diversifies exactly the set the grid is scoped to.
      const report = await ipc.diversifyScope(this.graphScope(), tolerance);
      if (load !== this.diversifyLoad) return; // a newer pass owns the filter now
      this.diversifyDegraded = report.degraded;
      if (report.degraded) {
        // No CLIP signal: show everything (the honest "all shown"), the slider
        // disables, and the chrome shows "embed photos to diversify".
        this.grid.diversifyShown = null;
        this.diversifyHidden = 0;
        return;
      }
      const shown = new Set(report.shown);
      this.grid.diversifyShown = shown;
      // Hidden count off the loaded scope size minus the shown set (stays honest
      // if an item left the scope between the call and a re-list).
      this.diversifyHidden = hiddenCount(this.grid.scopeHashes.length, shown);
    } catch {
      // Backend unavailable (tests/dev): leave the filter cleared, never error.
      if (load !== this.diversifyLoad) return;
      this.grid.diversifyShown = null;
      this.diversifyHidden = 0;
    }
  }

  // The Duplicates lens (DESIGN-DEDUP-AND-SIMILARITY.md "Tier 1") — opt-in
  // near-dup DETECT + DISPLAY over the current grid scope. Mirrors the heat-tint
  // toggle's shape (toggle -> persist -> fetch-or-clear); DISPLAY ONLY, nothing
  // here deletes or writes a sidecar (cull is deferred to founder design).
  // ---------------------------------------------------------------------------

  /** Toggle the Duplicates lens, persisted. Turning it on scans the current
   * scope for near-dup groups; off clears the cached groups so the grid surface
   * returns to the ordinary grid. */
  toggleDuplicates() {
    this.dupesOn = !this.dupesOn;
    prefs.saveDupesOn(this.dupesOn);
    // Force the next scan regardless of the cached scope signature (the scope
    // may be unchanged but the lens just turned on).
    this.dupeScopeKey = "";
    if (this.dupesOn) {
      void this.fetchDuplicates();
    } else {
      this.dupeGroups = null;
    }
  }

  /** Set the looseness slider (Hamming threshold / 64) and rescan, persisted.
   * The COMPONENT debounces the drag (logic/dedup.debounce) so this only lands
   * on a settled value; it forces a rescan even at an unchanged scope because the
   * threshold, not the scope, changed. */
  setDupeThreshold(value: number) {
    if (value === this.dupeThreshold) return;
    this.dupeThreshold = value;
    prefs.saveDupeThreshold(value);
    if (this.dupesOn) {
      this.dupeScopeKey = ""; // the threshold changed: force a rescan
      void this.fetchDuplicates();
    }
  }

  /** Scan the loaded grid scope for near-dup groups and replace the cached set.
   * Reuses `graphScope()` (the same image set the grid/graph show) and passes
   * the explicit looseness threshold. Guarded by a monotone token so a slow scan
   * cannot overwrite a newer one's. Silent on backend failure (tests/dev): the
   * lens just shows the none-state. */
  async fetchDuplicates() {
    if (!this.dupesOn) return;
    const load = ++this.dupeLoad;
    // null marks "scanning" so the view can show a quiet in-flight line instead
    // of flashing the none-state between a scope change and its result.
    this.dupeGroups = null;
    try {
      const groups = await ipc.findNearDuplicates(
        this.graphScope(),
        this.dupeThreshold,
      );
      if (load !== this.dupeLoad) return; // a newer scan won
      this.dupeGroups = groups ?? [];
    } catch {
      if (load === this.dupeLoad) this.dupeGroups = [];
    }
  }

  /** Non-destructive "select the redundant copies" affordance (the design doc's
   * welcome-if-clean multi-select): replace the grid selection with every
   * non-representative member across the near-dup clusters, reusing the grid's
   * own selection model. NO delete — it just gathers the redundant ones so the
   * founder can act with the existing verbs (or simply SEE which would go). A
   * no-op when the lens is off or there is nothing redundant. */
  selectRedundantDuplicates() {
    if (!this.dupesOn || this.dupeGroups === null) return;
    const clusters = dedup.toClusters(this.dupeGroups, (h) => this.ratingOf(h));
    const hashes = dedup.redundantHashes(clusters);
    if (hashes.length === 0) return;
    // Build the selection directly from the hashes (the lens renders bare
    // hashes, not grid units, so it owns its own order); focus the first so the
    // indicator and any active-hash verb have a subject.
    this.grid.setSelection({ order: hashes, focus: 0, anchor: 0 });
  }

  /** The folded rating of an in-scope image, or null when unknown — the
   * representative pick prefers the highest-rated copy to keep. Looked up from
   * the loaded grid rawItems (the lens scopes to the same image set), so it is
   * present for the visible scope and null for anything not loaded. */
  private ratingOf(hash: string): number | null {
    return this.grid.rawItems.find((i) => i.hash === hash)?.rating ?? null;
  }

  /** Targets for the membership verbs: the WHOLE stack-expanded selection
   * (CAPTURE §3 — a collapsed pair contributes both members), falling
   * back to the active image; the thumb right-click already selected an
   * unselected cell. */
  private collectionTargets(): string[] {
    if (this.grid.selectionTargets.length > 0) return this.grid.selectionTargets;
    const active = this.actionContext().activeHash;
    return active !== null ? [active] : [];
  }
}

export const ui = new Ui();
