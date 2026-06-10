/**
 * Central UI state (Svelte 5 runes). Orchestration only: every rule lives in
 * the pure modules under ../logic and is unit-tested there; the backend owns
 * scope/session/event semantics (the UI reports, the core echoes — UI §3.4).
 */
import * as ipc from "../ipc/commands";
import * as sel from "../logic/selection";
import * as note from "../logic/note";
import { escapeAction, type EscapeContext } from "../logic/escape";
import { scopeTargets } from "../logic/scope";
import { sortItems, THUMB_STEPS, type SortMode } from "../logic/sort";
import type { Action } from "../logic/keymap";
import type {
  FolderNode,
  GridItem,
  IngestStatus,
  RootDto,
  ScopeView,
} from "../types/dto";
import type { Filter, SearchResults } from "../types/search";
import * as prefs from "./prefs";

const SESSION_SCOPE: ScopeView = { kind: "session", count: 0, previewHashes: [] };

export class Ui {
  // -- surfaces (the whole app: Grid, Look, Search — UI §2.1) ---------------
  surface = $state<"grid" | "look">("grid");
  searchOpen = $state(false);
  /** Search remembers its return point (UI §2.2, I1). */
  searchReturn: "grid" | "look" = "grid";

  // -- roots, folders, rail --------------------------------------------------
  roots = $state<RootDto[]>([]);
  tree = $state<FolderNode[]>([]);
  rootId = $state<string | null>(null);
  folder = $state("");
  railOpen = $state(false);
  railPinned = $state(false);

  // -- grid -------------------------------------------------------------------
  rawItems = $state<GridItem[]>([]);
  sort = $state<SortMode>("capture-desc");
  thumbStep = $state(1);
  items = $derived(sortItems(this.rawItems, this.sort));
  itemHashes = $derived(this.items.map((i) => i.hash));
  sel = $state<sel.SelState>(sel.EMPTY);
  sortMenuOpen = $state(false);

  // -- look ---------------------------------------------------------------------
  /** Prev/next order: grid order, or search-result order when entered from
   * Search (UI §4.3). */
  lookOrder = $state<string[]>([]);
  lookIndex = $state(-1);
  filmstrip = $state(false);
  /** Zoom commands flow keyboard → Look (zoom state lives in the component). */
  lookZoom = $state<{ seq: number; op: "toggle" | "step" | "fit"; delta?: 1 | -1 }>({
    seq: 0,
    op: "fit",
  });
  lookHash = $derived(
    this.lookIndex >= 0 && this.lookIndex < this.lookOrder.length
      ? this.lookOrder[this.lookIndex]
      : null,
  );

  // -- search -------------------------------------------------------------------
  query = $state("");
  chips = $state<Filter[]>([]);
  results = $state<SearchResults | null>(null);
  searchFocus = $state(-1);
  searchSel = $state<sel.SelState>(sel.EMPTY);
  resultHashes = $derived(
    this.results === null ? [] : this.results.images.map((i) => i.image_hash),
  );

  // -- capture / indicator --------------------------------------------------------
  scope = $state<ScopeView>(SESSION_SCOPE);
  note = $state<note.NoteState>(note.CLOSED);
  /** Monotonic pulse counter; the indicator animates on change (UI §7.4). */
  pulseCount = $state(0);
  lastPulseAt = 0;
  popoverOpen = $state(false);
  ingest = $state<IngestStatus>({ running: false, done: 0, total: 0, errors: 0 });
  debugOpen = $state(false);

  // ---------------------------------------------------------------------------
  // boot
  // ---------------------------------------------------------------------------

  async init() {
    this.thumbStep = prefs.loadThumbStep();
    this.railPinned = prefs.loadRailPinned();
    this.filmstrip = prefs.loadFilmstrip();
    this.roots = await ipc.listRoots();
    const last = prefs.loadLastFolder();
    if (last && this.roots.some((r) => r.rootId === last.rootId)) {
      await this.openFolder(last.rootId, last.folder);
    } else if (this.roots.length > 0) {
      await this.openFolder(this.roots[0].rootId, "");
    }
    this.ingest = await ipc.ingestStatus();
    await this.reportScope();
  }

  get folderName(): string {
    if (this.folder !== "") return this.folder.split("/").pop() ?? this.folder;
    const root = this.roots.find((r) => r.rootId === this.rootId);
    return root?.displayName ?? "Photoproof";
  }

  // ---------------------------------------------------------------------------
  // scope reporting (CAPTURE §3 — report, then render the echo)
  // ---------------------------------------------------------------------------

  async reportScope() {
    const targets = scopeTargets({
      surface: this.surface,
      searchOpen: this.searchOpen,
      gridSelection: this.sel.order,
      searchSelection: this.searchSel.order,
      lookHash: this.lookHash,
    });
    try {
      const echoed = await ipc.setScope(targets);
      this.scope = echoed;
      // A scope change while the note input is open cancels it (logic/note.ts).
      this.note = note.onScopeChanged(this.note, echoed);
    } catch {
      /* backend unavailable (tests/dev): scope keeps last echo */
    }
  }

  // ---------------------------------------------------------------------------
  // folders & grid
  // ---------------------------------------------------------------------------

  async refreshRoots() {
    this.roots = await ipc.listRoots();
    if (this.rootId !== null && !this.roots.some((r) => r.rootId === this.rootId)) {
      this.rootId = null;
      this.rawItems = [];
    }
  }

  async openFolder(rootId: string, folder: string) {
    this.rootId = rootId;
    this.folder = folder;
    this.sort = prefs.loadSort(rootId, folder);
    this.sel = sel.EMPTY;
    this.rawItems = await ipc.listFolder(rootId, folder);
    this.tree = await ipc.folderTree(rootId);
    prefs.saveLastFolder(rootId, folder);
    if (!this.railPinned) this.railOpen = false;
    await this.reportScope();
  }

  /** Incremental refresh during ingest: keeps selection/focus (UI §3.3). */
  async refreshItems() {
    if (this.rootId === null) return;
    this.rawItems = await ipc.listFolder(this.rootId, this.folder);
    this.sel = sel.reconcile(this.sel, this.itemHashes);
  }

  setSort(mode: SortMode) {
    this.sort = mode;
    if (this.rootId !== null) prefs.saveSort(this.rootId, this.folder, mode);
  }

  setThumbStep(step: number) {
    this.thumbStep = Math.max(0, Math.min(THUMB_STEPS.length - 1, step));
    prefs.saveThumbStep(this.thumbStep);
  }

  async applySelection(next: sel.SelState) {
    this.sel = next;
    await this.reportScope();
  }

  // ---------------------------------------------------------------------------
  // Look
  // ---------------------------------------------------------------------------

  async openLook(hash: string, fromSearch: boolean) {
    const order = fromSearch ? this.resultHashes : this.itemHashes;
    const idx = order.indexOf(hash);
    if (idx < 0) return;
    this.lookOrder = [...order];
    this.lookIndex = idx;
    this.surface = "look";
    if (fromSearch) this.searchOpen = false;
    await this.reportScope();
  }

  async lookNav(delta: number) {
    const next = this.lookIndex + delta;
    if (next < 0 || next >= this.lookOrder.length) return;
    this.lookIndex = next;
    await this.reportScope();
  }

  async leaveLook() {
    // Back to Grid with the same image focused (UI §2.2).
    const hash = this.lookHash;
    this.surface = "grid";
    this.lookIndex = -1;
    if (hash !== null) {
      const idx = this.itemHashes.indexOf(hash);
      if (idx >= 0) this.sel = { ...this.sel, focus: idx };
    }
    await this.reportScope();
  }

  // ---------------------------------------------------------------------------
  // Search overlay
  // ---------------------------------------------------------------------------

  async openSearch() {
    if (this.searchOpen) return;
    this.searchReturn = this.surface;
    this.searchOpen = true;
    this.searchFocus = -1;
    this.searchSel = sel.EMPTY;
    await this.reportScope();
  }

  async closeSearch() {
    this.searchOpen = false;
    this.searchSel = sel.EMPTY;
    // Returns to the invoking surface (I1) — surface was never changed.
    await this.reportScope();
  }

  async runSearch() {
    const trimmed = this.query.trim();
    if (this.chips.length === 0 && trimmed.length < 2) {
      this.results = null;
      this.searchFocus = -1;
      return;
    }
    this.results = await ipc.search(this.query, this.chips);
    this.searchFocus = this.results.images.length > 0 ? 0 : -1;
    this.searchSel = sel.EMPTY;
  }

  async removeChip(index: number) {
    this.chips = this.chips.filter((_, i) => i !== index);
    await this.runSearch();
  }

  // ---------------------------------------------------------------------------
  // capture: notes, ratings, pulses
  // ---------------------------------------------------------------------------

  summonNote() {
    this.note = note.summon(this.scope);
  }

  cancelNote() {
    this.note = note.cancel(this.note);
  }

  async submitNote(text: string) {
    const { state } = note.submit(this.note);
    this.note = state; // vanishes immediately (UI §6)
    await ipc.addNote(text);
  }

  async rate(value: number) {
    // Session scope: rating keys do nothing (CAPTURE §10).
    if (this.scope.kind === "session") return;
    await ipc.setRating(value);
  }

  /** Pulse coalescing: rapid events render distinct pulses, coalesced above
   * ~5/s (UI §7.4). */
  onPulse(now: number = Date.now()) {
    if (now - this.lastPulseAt < 200) return;
    this.lastPulseAt = now;
    this.pulseCount += 1;
  }

  // ---------------------------------------------------------------------------
  // Escape — strict back-one-layer (UI §2.2, I1)
  // ---------------------------------------------------------------------------

  escapeContext(): EscapeContext {
    return {
      noteInputOpen: this.note.open,
      sortMenuOpen: this.sortMenuOpen,
      indicatorPopoverOpen: this.popoverOpen,
      debugPanelOpen: this.debugOpen,
      searchOpen: this.searchOpen,
      surface: this.surface,
      hasSelection: this.sel.order.length > 0,
    };
  }

  async escape() {
    switch (escapeAction(this.escapeContext())) {
      case "close-note-input":
        this.cancelNote();
        break;
      case "close-sort-menu":
        this.sortMenuOpen = false;
        break;
      case "close-indicator-popover":
        this.popoverOpen = false;
        break;
      case "close-debug-panel":
        this.debugOpen = false;
        break;
      case "leave-search":
        await this.closeSearch();
        break;
      case "leave-look":
        await this.leaveLook();
        break;
      case "clear-selection":
        await this.applySelection(sel.clear(this.sel));
        break;
      case "none":
        break; // never quits
    }
  }

  // ---------------------------------------------------------------------------
  // keyboard action sink (dispatching tested in logic/keymap; effects here)
  // ---------------------------------------------------------------------------

  async perform(action: Action) {
    switch (action.kind) {
      case "escape":
        await this.escape();
        break;
      case "open-search":
        await this.openSearch();
        break;
      case "summon-note":
        this.summonNote();
        break;
      case "open-settings":
        await ipc.openSettingsWindow();
        break;
      case "quit":
        await ipc.quit();
        break;
      case "toggle-debug-panel":
        this.debugOpen = !this.debugOpen;
        break;
      case "rate":
        await this.rate(action.value);
        break;
      case "open-look": {
        if (this.searchOpen) {
          const hash = this.resultHashes[this.searchFocus];
          if (hash !== undefined) await this.openLook(hash, true);
        } else {
          const hash = this.itemHashes[this.sel.focus];
          if (hash !== undefined) await this.openLook(hash, false);
        }
        break;
      }
      case "toggle-rail":
        this.railOpen = !this.railOpen;
        break;
      case "focus-move":
        await this.applySelection(
          sel.moveFocus(this.sel, this.itemHashes, this.gridCols, action.dir, action.extend),
        );
        break;
      case "toggle-select-focused":
        await this.applySelection(sel.toggle(this.sel, this.itemHashes, this.sel.focus));
        break;
      case "select-all":
        await this.applySelection(sel.selectAll(this.sel, this.itemHashes));
        break;
      case "open-sort-menu":
        this.sortMenuOpen = !this.sortMenuOpen;
        break;
      case "thumb-size":
        this.setThumbStep(this.thumbStep + action.delta);
        break;
      case "look-nav":
        await this.lookNav(action.delta);
        break;
      case "search-nav": {
        const delta = action.dir === "up" || action.dir === "left" ? -1 : 1;
        const n = this.resultHashes.length;
        if (n > 0)
          this.searchFocus = Math.max(0, Math.min(n - 1, this.searchFocus + delta));
        break;
      }
      case "search-open-result": {
        const hash = this.resultHashes[this.searchFocus];
        if (hash !== undefined) await this.openLook(hash, true);
        break;
      }
      case "remove-last-chip":
        if (this.query === "" && this.chips.length > 0)
          await this.removeChip(this.chips.length - 1);
        break;
      case "zoom-toggle":
        this.lookZoom = { seq: this.lookZoom.seq + 1, op: "toggle" };
        break;
      case "zoom-step":
        this.lookZoom = { seq: this.lookZoom.seq + 1, op: "step", delta: action.delta };
        break;
      case "zoom-fit":
        this.lookZoom = { seq: this.lookZoom.seq + 1, op: "fit" };
        break;
      case "toggle-filmstrip":
        this.filmstrip = !this.filmstrip;
        prefs.saveFilmstrip(this.filmstrip);
        break;
      case "rail-nav":
      case "rail-enter":
        break; // handled by the Rail component
    }
  }

  /** Current grid column count, reported by the Grid component. */
  gridCols = 1;
}

export const ui = new Ui();
