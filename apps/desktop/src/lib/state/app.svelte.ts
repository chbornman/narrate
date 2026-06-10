/**
 * Composition root (Svelte 5 runes): exports `ui` — the shell/grid/look/
 * inspector slices plus search state — the perform(Action) router, and the
 * actionContext() snapshot the keymap/menus/cheatsheet read. Slices never
 * import each other; CROSS-SLICE FLOWS LIVE ONLY HERE: openLook (entry
 * selection → LookEntry order via looknav.navigationSet), leaveLook (same
 * image active, flip-aware; the grid restores its own scroll anchor on
 * mount), goHome (G), auto-advance wiring (logic/advance.ts), the
 * inspector following the active image, the drag-folder drop-confirm
 * (featureset §6), and scope reporting (report, then render the echo —
 * UI §3.4; the backend owns scope semantics).
 */
import * as ipc from "../ipc/commands";
import * as sel from "../logic/selection";
import * as note from "../logic/note";
import { escapeAction, type EscapeContext } from "../logic/escape";
import { navigationSet } from "../logic/looknav";
import { scopeTargets } from "../logic/scope";
import { afterCommit } from "../logic/advance";
import {
  flatRows,
  moveFocus as railMoveFocus,
  sections,
  toggleExpand,
} from "../logic/sources";
import type { Action } from "../logic/keymap";
import type { ActionContext } from "../actions/types";
import type { AppSettings, FolderNode, RootDto } from "../types/dto";
import type { LookEntry } from "../types/display";
import type { Filter, SearchResults } from "../types/search";
import * as prefs from "./prefs";
import { ShellSlice } from "./shell.svelte";
import { GridSlice } from "./grid.svelte";
import { LookSlice } from "./look.svelte";
import { InspectorSlice } from "./inspector.svelte";

/** Clipboard write with the webview fallback: navigator.clipboard needs a
 * secure context some webviews (webkit2gtk dev origins) don't grant, so
 * "Copy file path" falls back to the classic textarea + execCommand path
 * (platform smoke check named in DOGFOOD §visual, Appendix B). */
async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    /* fall through */
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.className = "pp-offscreen";
  document.body.appendChild(ta);
  ta.select();
  document.execCommand("copy");
  ta.remove();
}

export class Ui {
  // -- slices (contracts frozen by FOUNDATIONS) -------------------------------
  shell = new ShellSlice();
  grid = new GridSlice();
  look = new LookSlice();
  inspector = new InspectorSlice();

  // -- surfaces (the whole app: Grid, Look, Search — UI §2.1) -----------------
  surface = $state<"grid" | "look">("grid");
  searchOpen = $state(false);
  /** Search remembers its return point (UI §2.2, I1). */
  searchReturn: "grid" | "look" = "grid";

  // -- roots & folder tree (shared by rail + grid) ----------------------------
  roots = $state<RootDto[]>([]);
  tree = $state<FolderNode[]>([]);

  // -- search ------------------------------------------------------------------
  query = $state("");
  chips = $state<Filter[]>([]);
  results = $state<SearchResults | null>(null);
  searchFocus = $state(-1);
  searchSel = $state<sel.SelState>(sel.EMPTY);
  resultHashes = $derived(
    this.results === null ? [] : this.results.images.map((i) => i.image_hash),
  );

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
    await this.reportScope();
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
      searchOpen: this.searchOpen,
      gridSelection: this.grid.selectionTargets, // stack-expanded upstream
      searchSelection: this.searchSel.order,
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
  }

  // ---------------------------------------------------------------------------
  // folders & grid
  // ---------------------------------------------------------------------------

  async refreshRoots() {
    this.roots = await ipc.listRoots();
    if (
      this.grid.rootId !== null &&
      !this.roots.some((r) => r.rootId === this.grid.rootId)
    ) {
      this.grid.rootId = null;
      this.grid.rawItems = [];
    }
  }

  async openFolder(rootId: string, folder: string) {
    // Opening a folder always lands on the Grid: navigating sources while in
    // Look exits the single-image view (founder dogfood, round 1).
    if (this.surface === "look") await this.leaveLook();
    this.grid.rootId = rootId;
    this.grid.folder = folder;
    this.grid.sort = prefs.loadSort(rootId, folder);
    this.grid.sel = sel.EMPTY;
    this.grid.setItems(await ipc.listFolder(rootId, folder));
    this.tree = await ipc.folderTree(rootId);
    prefs.saveLastFolder(rootId, folder);
    await this.reportScope();
  }

  /** Incremental refresh during ingest: keeps selection/focus (UI §3.3). */
  async refreshItems() {
    if (this.grid.rootId === null) return;
    this.grid.setItems(await ipc.listFolder(this.grid.rootId, this.grid.folder));
  }

  async applySelection(next: sel.SelState) {
    this.grid.setSelection(next);
    await this.reportScope();
  }

  // ---------------------------------------------------------------------------
  // Look (cross-slice flow; INTEGRATION finishes nav-set + anchor restore)
  // ---------------------------------------------------------------------------

  async openLook(hash: string, fromSearch: boolean) {
    // Navigation set = entry selection (featureset §2): a ≥2 selection
    // including the entry cycles within it (GRID order — looknav.ts);
    // otherwise the whole folder / result list. Search results carry no
    // pairs, so the same rule applies over bare result hashes.
    let order: LookEntry[];
    let idx: number;
    if (fromSearch) {
      const selSet = new Set(this.searchSel.order);
      const scoped =
        selSet.size >= 2 && selSet.has(hash)
          ? this.resultHashes.filter((h) => selSet.has(h))
          : this.resultHashes;
      idx = scoped.indexOf(hash);
      if (idx < 0) return;
      order = scoped.map((h) => ({ display: h, alt: null }));
    } else {
      const nav = navigationSet(this.grid.units, this.grid.sel.order, hash);
      if (nav === null) return;
      ({ order, index: idx } = nav);
    }
    this.look.open(order, idx);
    this.surface = "look";
    if (fromSearch) this.searchOpen = false;
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

  /** G — universal "go home" (featureset §0). */
  async goHome() {
    this.searchOpen = false;
    if (this.surface === "look") {
      await this.leaveLook();
      return;
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
    let first: string | null = null;
    for (const path of paths) {
      try {
        const root = await ipc.addRoot(path);
        first ??= root.rootId;
      } catch (e) {
        this.dropError = e instanceof Error ? e.message : String(e);
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
    const { state } = note.submit(this.shell.note);
    this.shell.note = state; // vanishes immediately (UI §6)
    const committed = await ipc.addNote(text);
    if (committed) {
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

  /** The inspector is LIVE: any commit that targets the inspected image
   * re-folds its journal/metadata immediately (founder dogfood, round 1) —
   * never wait for an active-image change. */
  private async refreshInspectorIfTargeted() {
    const hash = this.inspector.hash;
    if (this.inspector.open === false || hash === null) return;
    const targets = scopeTargets({
      surface: this.surface,
      searchOpen: this.searchOpen,
      gridSelection: this.grid.selectionTargets,
      searchSelection: this.searchSel.order,
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
  // Escape — the 12-layer order (logic/escape.ts)
  // ---------------------------------------------------------------------------

  escapeContext(): EscapeContext {
    return {
      redactionModalOpen: this.inspector.redactTargetId !== null,
      dropConfirmOpen: this.dropPaths !== null,
      contextMenuOpen: this.shell.contextMenu !== null,
      journalEditOpen: this.inspector.editingEventId !== null,
      noteInputOpen: this.shell.note.open,
      cheatsheetOpen: this.shell.cheatsheetOpen,
      indicatorPopoverOpen: this.shell.popoverOpen,
      debugPanelOpen: this.shell.debugOpen,
      inspectorOpen: this.inspector.open !== false,
      searchOpen: this.searchOpen,
      surface: this.surface,
      hasSelection: this.grid.sel.order.length > 0,
    };
  }

  async escape() {
    switch (escapeAction(this.escapeContext())) {
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
      case "close-note-input":
        this.cancelNote();
        break;
      case "close-cheatsheet":
        this.shell.cheatsheetOpen = false;
        break;
      case "close-indicator-popover":
        this.shell.popoverOpen = false;
        break;
      case "close-debug-panel":
        this.shell.debugOpen = false;
        break;
      case "close-inspector":
        this.inspector.close();
        break;
      case "leave-search":
        await this.closeSearch();
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
      searchOpen: this.searchOpen,
      inputFocused: input?.inputFocused ?? false,
      searchInputFocused: input?.searchInputFocused ?? false,
      queryEmpty: this.query === "",
      hasSelection: this.searchOpen
        ? this.searchSel.order.length > 0
        : this.grid.sel.order.length > 0,
      selectionCount: this.searchOpen
        ? this.searchSel.order.length
        : this.grid.sel.order.length,
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
      lookAtFit: this.look.atFit,
      debugEnabled: this.debugEnabled,
      asrReady: false, // P4.2: no ASR
      sort: this.grid.sort,
      thumbStep: this.grid.thumbStep,
      surround: this.shell.surround,
      filmstrip: this.look.filmstrip,
      pencilMode: false, // reserved (M2a)
      micArmed: false, // reserved (M2b)
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
        this.shell.toggleLightsOut();
        break;
      case "toggle-rail":
        this.shell.toggleRail();
        break;
      case "toggle-cheatsheet":
        this.shell.toggleCheatsheet();
        break;
      case "open-search":
        await this.openSearch();
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
        if (this.searchOpen) {
          const hash = this.resultHashes[this.searchFocus];
          if (hash !== undefined) await this.openLook(hash, true);
        } else {
          const hash = this.grid.unitHashes[this.grid.sel.focus];
          if (hash !== undefined) await this.openLook(hash, false);
        }
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
      // ---- panels -----------------------------------------------------------
      case "rail-nav": {
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
        const rows = flatRows(this.railSections());
        const row = rows.find((r) => r.key === this.shell.railFocusKey);
        if (row !== undefined) {
          await this.openFolder(row.rootId, row.folder);
          this.shell.railFocused = false; // focus returns to the grid
        }
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
        try {
          await ipc.rescanRoot(action.rootId);
        } catch {
          /* unreachable backend in tests */
        }
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
        // Retract → toast+Undo (RE-STATE — the slice owns the flow);
        // JournalTab routes here AND calls the slice directly: both paths
        // land on the same method.
        await this.inspector.retract(action.eventId);
        break;
      case "journal-redact":
        this.inspector.beginRedact(action.eventId); // → the one modal
        break;
      case "journal-toggle-retracted":
        this.inspector.showRetracted = !this.inspector.showRetracted;
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
            if (paths.absPath !== null) await copyText(paths.absPath);
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
      // ---- search -------------------------------------------------------------
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
      // ---- reserved rows: dispatch to nothing until their packets -------------
      case "toggle-mic":
      case "pencil-pen":
      case "pencil-eraser":
      case "pencil-visibility":
      case "cycle-overlay":
        break;
    }
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
}

export const ui = new Ui();
