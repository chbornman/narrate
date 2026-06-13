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
import { scopeLabel, scopeTargets } from "../logic/scope";
import { nextLane, type SearchLane } from "../logic/searchmode";
import { afterCommit } from "../logic/advance";
import {
  collectionRows,
  flatRows,
  moveFocus as railMoveFocus,
  sections,
  toggleExpand,
} from "../logic/sources";
import type { Action } from "../logic/keymap";
import type { ActionContext } from "../actions/types";
import type {
  AppSettings,
  CollectionDto,
  FolderNode,
  IngestStatus,
  RootDto,
  StrokePayloadWire,
} from "../types/dto";
import type { Filter } from "../types/search";
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

/** Minimum free-text query length before a search runs: a single
 * character matches nearly everything and burns the <100 ms budget
 * (UI §5.1) on a result set nobody asked for. Shorter queries with no
 * chips clear the results instead. Module-local on purpose: nothing
 * outside this file consumes it, and exporting would invite UI copy to
 * couple to search-gating policy without review. */
const MIN_QUERY_CHARS = 2;

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
  | { kind: "query"; query: string; chips: Filter[]; within: GridScope };

export class Ui {
  // -- slices (contracts frozen by FOUNDATIONS) -------------------------------
  shell = new ShellSlice();
  grid = new GridSlice();
  look = new LookSlice();
  inspector = new InspectorSlice();

  // -- surfaces (the whole app: Grid, Look — UI §2.1) -------------------------
  // Search is no longer a surface (M3 search-as-scope): a query scopes the
  // GRID in place, so there are only two surfaces again. `searchOpen` and the
  // search-overlay return point are retired with the overlay.
  surface = $state<"grid" | "look">("grid");

  // -- roots & folder tree (shared by rail + grid) ----------------------------
  roots = $state<RootDto[]>([]);
  tree = $state<FolderNode[]>([]);

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
   * a collection open?". A query scoped OVER a collection counts as that
   * collection being open (the residue still points there). null otherwise. */
  collectionId = $derived<string | null>(
    this.gridScope.kind === "collection"
      ? this.gridScope.id
      : this.gridScope.kind === "query" && this.gridScope.within.kind === "collection"
        ? this.gridScope.within.id
        : null,
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
    try {
      this.applySettings(await ipc.settingsGet());
    } catch {
      /* backend unavailable (tests/dev): defaults stand */
    }
    this.roots = await ipc.listRoots();
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

  // ---------------------------------------------------------------------------
  // scope reporting (CAPTURE §3 — report, then render the echo)
  // ---------------------------------------------------------------------------

  async reportScope() {
    const targets = scopeTargets({
      surface: this.surface,
      // Search is no longer a separate selection surface (M3): query results
      // ARE grid cells, so the write scope is the grid selection in every
      // non-Look case. searchOpen/searchSelection are held false/empty to
      // keep scope.ts's pure contract satisfied without a search surface.
      searchOpen: false,
      gridSelection: this.grid.selectionTargets, // stack-expanded upstream
      searchSelection: [],
      lookTargets: this.look.currentTargets,
    });
    try {
      const echoed = await ipc.setScope(targets);
      this.shell.onScopeEcho(echoed);
    } catch {
      /* backend unavailable (tests/dev): scope keeps last echo */
    }
    // The inspector shows the ACTIVE image's truth (featureset §3); every
    // active-hash change flows through here (focus moves, ←/→ in Look,
    // stack flips), so an open inspector follows the eye.
    const active =
      this.surface === "look" ? this.look.currentHash : this.grid.activeHash;
    if (this.inspector.open !== false && this.inspector.hash !== active)
      await this.inspector.load(active);
    // Membership marks follow the active image the same way: the thumb
    // menu's checkmarks must be honest at open time, not after a click.
    await this.refreshActiveMemberships();
  }

  /** Load the ACTIVE image's current memberships (collections_for_image).
   * Skipped entirely while no collections exist — the common no-collection
   * session pays zero extra IPC per focus move. `force` re-fetches for an
   * unchanged hash (a collections-changed snapshot may mean THIS image's
   * membership moved in another window). */
  private async refreshActiveMemberships(force = false) {
    const active =
      this.surface === "look" ? this.look.currentHash : this.grid.activeHash;
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
    // Optimistic; the first real status event clears it (onIngestProgress).
    this.shell.ingestExpecting = true;
    let root: RootDto;
    try {
      root = await ipc.addRoot(dir);
    } catch (e) {
      this.shell.ingestExpecting = false; // terminal: no scan will run
      throw e;
    }
    await this.refreshRoots();
    await this.openFolder(root.rootId, "");
  }

  async openFolder(rootId: string, folder: string) {
    // Opening a folder always lands on the Grid: navigating sources while in
    // Look exits the single-image view (founder dogfood, round 1).
    if (this.surface === "look") await this.leaveLook();
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
    if (this.surface === "look") await this.leaveLook();
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
    // instantly-finished scan must not leave "Indexing" stranded.
    this.shell.ingestExpecting = false;
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
    this.surface = "look";
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
    this.surface = "grid";
    this.look.close();
    if (hash !== null) {
      const idx = this.grid.units.findIndex(
        (u) => u.primary.hash === hash || u.alt?.hash === hash,
      );
      if (idx >= 0) this.grid.sel = { ...this.grid.sel, focus: idx };
    }
    await this.reportScope();
  }

  /** G — universal "go home" (featureset §0). Clears any query scope too:
   * home is the underlying folder/collection, not a search result set. */
  async goHome() {
    if (this.surface === "look") {
      await this.leaveLook();
      return;
    }
    if (this.gridScope.kind === "query") {
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

  /** The source a NEW query scopes over: when the grid already shows a
   * query, re-typing keeps the SAME underlying source (`within`) — a query
   * is never scoped over another query. Otherwise the current folder/
   * collection scope is the source. */
  private queryWithin(): GridScope {
    return this.gridScope.kind === "query" ? this.gridScope.within : this.gridScope;
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
    const load = ++this.gridLoad;
    const results = await ipc.search(this.query, this.chips, mode);
    if (load !== this.gridLoad) return; // a newer scope owns the grid now
    const hashes = results.images.map((i) => i.image_hash);
    // Enrich result hashes → GridItems (in fused order; list_images
    // preserves the order given). The grid's relevance sort keeps it.
    const items = hashes.length === 0 ? [] : ((await ipc.listImages(hashes)) ?? []);
    if (load !== this.gridLoad) return;
    this.grid.setItems(items);
    await this.reportScope();
  }

  /** Re-point the grid from a query scope back to its underlying source and
   * re-list, WITHOUT clearing the bar input or leaving Look.
   *
   * Surface-safe: a query can be committed and then a result opened in Look,
   * so this swaps the grid scope UNDERNEATH Look (Look has its own Esc layer
   * below). It re-points gridScope at the source, restores its sort, and
   * re-lists via refreshItems (the scope-aware feeder) — never through
   * openFolder/openCollection, which would leaveLook and peel two Esc layers
   * at once. No-op when not in a query scope. */
  private async returnToSource() {
    if (this.gridScope.kind !== "query") return;
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

  /** First Escape / one-key residue clear / G: drop the query scope and
   * return the grid to its underlying source. The bar input clears too — an
   * EXPLICIT clear is the only thing that wipes the text, and the residue's
   * whole point is that you SEE where you land. */
  async clearQueryScope() {
    const wasQuery = this.gridScope.kind === "query";
    this.clearQueryInput();
    // returnToSource re-lists AND reports when it was a query; when it wasn't
    // (defensive — callers gate) the input still cleared, so report directly.
    if (wasQuery) await this.returnToSource();
    else await this.reportScope();
  }

  /** Bar edit removed the last chip (Backspace on an empty input). Re-runs
   * the live lexical lane so the grid re-scopes immediately. */
  async removeChip(index: number) {
    this.chips = this.chips.filter((_, i) => i !== index);
    await this.runQueryScope("lexical");
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
    this.shell.ingestExpecting = true;
    let first: string | null = null;
    for (const path of paths) {
      try {
        const root = await ipc.addRoot(path);
        first ??= root.rootId;
      } catch (e) {
        this.dropError = e instanceof Error ? e.message : String(e);
        // Only a FULLY refused drop stands the bridge down — once one
        // root registered, its scan is real and the events will clear it.
        if (first === null) this.shell.ingestExpecting = false;
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
      surface: this.surface,
      searchOpen: false,
      gridSelection: this.grid.selectionTargets,
      searchSelection: [],
      lookTargets: this.look.currentTargets,
    });
    if (targets.includes(hash)) await this.inspector.load(hash);
  }

  /** Auto-advance wiring (logic/advance.ts): multi-select rating never
   * advances or destroys the selection. */
  private async advanceAfter(commit: "rating" | "note") {
    const outcome = afterCommit({
      autoAdvance: this.autoAdvance,
      surface: this.surface,
      commit,
      selectionCount: this.surface === "look" ? 1 : this.grid.sel.order.length,
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
    if (this.surface === "look") {
      this.surface = "grid";
      this.look.close();
    }
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
      inspectorOpen: this.inspector.open !== false,
      queryScopeActive: this.gridScope.kind === "query",
      searchBarFocused: this.barFocused,
      surface: this.surface,
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
      surface: this.surface,
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
      activeHash:
        this.surface === "look" ? this.look.currentHash : this.grid.activeHash,
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
      queryActive: this.gridScope.kind === "query",
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
        if (this.surface === "look") this.look.flipMember();
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
          if (row !== undefined)
            this.shell.railCollapsed = toggleExpand(
              this.shell.railCollapsed,
              row,
              action.dir,
            );
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
        // walk has the same dark window before its first status emit.
        this.shell.ingestExpecting = true;
        try {
          await ipc.rescanRoot(action.rootId);
        } catch {
          /* unreachable backend in tests */
          this.shell.ingestExpecting = false;
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
        if (this.surface === "look") this.look.flashStroke(action.eventId);
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

  /** Rail rows over the shared roots/tree (logic/sources.ts providers). */
  railSections() {
    return sections({
      roots: this.roots,
      tree: this.tree,
      treeRootId: this.grid.rootId,
      collapsed: this.shell.railCollapsed,
    });
  }

  /** Rail rows for the Collections tab (logic/sources.ts provider). */
  railCollectionRows() {
    return collectionRows(this.collections);
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
