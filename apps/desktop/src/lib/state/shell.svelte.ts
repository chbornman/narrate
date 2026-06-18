/**
 * Shell slice (COMPLETE in FOUNDATIONS; frozen contract): chrome state —
 * lights-out, rail, cheatsheet, the ONE context-menu host's state,
 * surround, fullscreen, the note transient, scope echo + pulse, ingest,
 * indicator popover, debug panel. Pure state + persistence; IPC and
 * cross-slice flows live only in the composition root (app.svelte.ts).
 */
import * as note from "../logic/note";
import { transitionPops } from "../logic/station";
import type { MenuSurface } from "../actions/menus";
import type {
  IndicatorState,
  IngestStatus,
  RuntimeStatus,
  ScopeView,
} from "../types/dto";
import type { SurroundLevel } from "../theme/surround";
import { surround as surroundStore } from "../theme/surround-store.svelte";
import * as prefs from "./prefs";

export const SESSION_SCOPE: ScopeView = {
  kind: "session",
  count: 0,
  previewHashes: [],
};

/** Pulse coalescing window: 200 ms is exactly the "~5/s" ceiling UI §7.4
 * names — pulses landing within it merge into one render. Independent of
 * the indicator's own pulse DURATION (Indicator.svelte), which only
 * happens to be a nearby number. */
const PULSE_COALESCE_MS = 200;

/** The set of panels open at lights-out time (founder, June 12 2026:
 * Tab restores WHAT WAS OPEN — a snapshot, never a fixed default set).
 * Captured/restored by the composition root (cross-slice: the inspector
 * and filmstrip flags live on their own slices). */
export interface PanelSnapshot {
  rail: boolean;
  inspector: false | "metadata" | "journal";
  filmstrip: boolean;
}

export interface ContextMenuState {
  seat: MenuSurface;
  /** Pointer anchor; null = keyboard-summoned (host picks a default). */
  anchor: { x: number; y: number } | null;
  /** Seat argument (rail-folder row, journal entry id …). */
  arg?: unknown;
}

export class ShellSlice {
  /** Tab lights-out (featureset §0) — SNAPSHOT-RESTORE (founder, June 12
   * 2026): the root records which panels were open (panelSnapshot below),
   * closes them, and the next Tab restores exactly that set. Titlebar and
   * the grid header still gate on this flag directly. Exempt by ruling:
   * the indicator and an open note input. */
  chromeHidden = $state(false);
  /** The open-panel set at hide time; null while chrome shows. Plain
   * field, not $state: only the root reads it, at restore time. */
  panelSnapshot: PanelSnapshot | null = null;

  // -- rail (push panel; D5: `\` toggles) ------------------------------------
  railOpen = $state(false);
  /** Keyboard focus lives on the rail (arrow routing gates on THIS, not
   * railOpen — the rail is push-persistent). */
  railFocused = $state(false);
  railFocusKey = $state<string | null>(null);
  railCollapsed = $state<ReadonlySet<string>>(new Set());
  /** Deep-tree ergonomics (folder-tree improvements): keys the user has
   * EXPLICITLY expanded past the auto-collapse depth, so a hand-opened deep
   * branch survives re-renders. Paired with railCollapsed in toggleExpand. */
  railExpanded = $state<ReadonlySet<string>>(new Set());
  /** The Folders-tab filter / jump input text. Empty ⇒ the full tree shows;
   * non-empty narrows the current root's tree to matching folders + their
   * ancestors (logic/sources.filterTree). Local UI state, never persisted. */
  railFolderFilter = $state("");
  /** Archived roots shown (the collapsed "Archived" affordance is open). */
  railArchivedOpen = $state(false);
  /** Folders | Collections — two PEER tabs (founder, June 2026: collections
   * are the point; folders are mechanical). Arrow/Enter routing follows the
   * visible tab (app.svelte.ts perform cases). */
  railTab = $state<prefs.RailTab>("folders");
  /** Armed when the thumb menu's "New collection…" was picked (founder,
   * dogfood June 12 2026): the stack-expanded targets to drop into the
   * collection the rail's inline creator is about to mint. Holding the
   * targets HERE (captured synchronously at pick time) is what keeps the
   * create+add chain safe against the selection drifting while the user
   * types the name. null whenever no add is pending; the rail reads it to
   * auto-open its input and to route the commit through createCollectionAndAdd. */
  pendingNewCollection = $state<string[] | null>(null);

  // -- overlays / hosts --------------------------------------------------------
  cheatsheetOpen = $state(false);
  contextMenu = $state<ContextMenuState | null>(null);
  popoverOpen = $state(false); // station hover expansion (pointer-tracked)
  /** The info seat's pin (registry row `toggle-station-detail`): holds the
   * station expansion open until dismissed — hover keeps working when
   * unpinned. Esc and outside-click clear BOTH flags (closeStation), so
   * the pinned body peels on the same escape layer the popover always had. */
  stationPinned = $state(false);
  /** Short rising chips popping from the station (the generalized
   * note-pop move): note shipped, mic armed/disarmed, utterance captured.
   * Station.svelte renders and retires them on animation end — no toasts. */
  pops = $state<{ id: number; text: string }[]>([]);
  #popSeq = 0;

  // -- viewing comfort (D6) ----------------------------------------------------
  /** The active image-backdrop surround level. NOW OWNED by the surround store
   * (theme/surround-store.svelte.ts), which composes the follow-theme | manual
   * mode with the live resolved theme. This getter keeps the shell as the read
   * seam (App.svelte's data-surround, the set-surround action's ctx.surround)
   * so those callers are untouched. */
  get surround(): SurroundLevel {
    return surroundStore.level;
  }
  fullscreen = $state(false);

  // -- UI scale (desktop conventions: Cmd+= / Cmd+− / Cmd+0) -------------------
  /** Webview zoom factor — the whole CHROME scales (distinct from Look's
   * image zoom, which lives on plain keys in the look slice). This slice
   * owns the state + persistence only; the actual webview call is IPC-
   * adjacent and lives in the composition root (app.svelte.ts perform). */
  uiZoom = $state<number>(1);

  // -- capture echo ------------------------------------------------------------
  note = $state<note.NoteState>(note.CLOSED);
  scope = $state<ScopeView>(SESSION_SCOPE);
  /** CAPTURE §11 mic state (rendered by the mic mode segment); the shell
   * reports "disarmed"/unavailable until P6.2 wires the live engine. */
  mic = $state<IndicatorState["mic"]>("disarmed");
  /** §5.4: a still-streaming utterance's bound scope (tether rendering). */
  streamingUtterance = $state<IndicatorState["streamingUtterance"]>(null);
  /** THE one coherent ASR-readiness story (P6.2 reconciliation):
   * `asrReady` is the EXISTENCE gate — RUNTIME §8.3 readiness, live off
   * the `runtime-status` channel; mic surfaces (glyph, the Space mic row,
   * Settings § Microphone) exist only when true. `asrUnavailable` is the
   * DEGRADED gate — CAPTURE §11's indicator flag, live off the
   * `indicator-state` channel; it renders the muted-mic state when the
   * user MEANT to capture but the ASR died. Ready=false ⇒ surfaces
   * absent; ready=true + unavailable=true ⇒ the quiet struck-through
   * glyph (UI §7.3). */
  asrReady = $state(false);
  asrUnavailable = $state(true);
  /** Latest RUNTIME snapshot (settings Models rows, the consent card,
   * download progress). */
  runtime = $state<RuntimeStatus | null>(null);
  /** Monotonic pulse counter; the indicator animates on change (UI §7.4). */
  pulseCount = $state(0);
  lastPulseAt = 0;
  ingest = $state<IngestStatus>({
    running: false,
    done: 0,
    total: 0,
    errors: 0,
    passes: [],
    scanning: false,
    discovered: 0,
    offlineVolumes: [],
    vectorsVersion: 0,
  });
  /** Optimistic "ingest is coming" bridge (founder, June 2026): between
   * the add-root/rescan click and the pump's first scanning=true emit,
   * `ingest.running` still reads false — and the empty grid would lie
   * "No photographs" over a folder about to be walked. Raised synchronously
   * by the perform sink (app.svelte.ts) through expectIngest(); cleared by
   * the FIRST real status event (onIngestProgress — the backend's walk-aware
   * `running` takes over from there), when the add/rescan call terminally
   * errors, OR by the watchdog below.
   *
   * WHY the watchdog: the clear used to depend on an ingest-progress event
   * ARRIVING. A rescan/add that returns Ok but emits NO progress (a path
   * deleted out from under us, a zero-change rescan, an instant no-op) leaves
   * the flag raised forever, so an empty grid reads "Indexing…" until restart
   * (AUDIT-FRONTEND-COUPLING A2 / STATE-MACHINE §6e — the *confirmed* strand).
   * Raising the flag therefore ARMS a deadline that stands it down if no real
   * status lands in time; a real status event cancels the pending watchdog.
   * In-memory only, never persisted: a refresh/restart starts false and boot
   * fetches real status. */
  ingestExpecting = $state(false);
  /** Pending watchdog handle for the bridge above; null whenever no expect is
   * in flight. Plain field (not $state): only the slice's own arm/clear logic
   * reads it, never the view. */
  #ingestExpectTimer: ReturnType<typeof setTimeout> | null = null;

  /** Raise the optimistic bridge AND arm its watchdog. Centralized here (one
   * arm site, one clear site) so the set→clear lifecycle is symmetric and the
   * timer can't leak across the three perform call sites. `timeoutMs` is the
   * deadline the composition root owns (it lives with the other ingest timing
   * constants in app.svelte.ts); the slice stays value-agnostic. */
  expectIngest(timeoutMs: number) {
    this.#clearIngestExpectTimer();
    this.ingestExpecting = true;
    // No real status by the deadline ⇒ stand the bridge down; the empty grid
    // falls back to its honest "No photographs" copy rather than stranding on
    // "Indexing…" forever.
    this.#ingestExpectTimer = setTimeout(() => {
      this.#ingestExpectTimer = null;
      this.ingestExpecting = false;
    }, timeoutMs);
  }

  /** Stand the bridge down and cancel any pending watchdog. The single clear
   * sink for every path: the first real ingest-progress event, a terminal
   * add/rescan error, and the watchdog's own fire all route through here so a
   * late timer can never fire a spurious clear after the flag already cleared. */
  clearIngestExpecting() {
    this.#clearIngestExpectTimer();
    this.ingestExpecting = false;
  }

  #clearIngestExpectTimer() {
    if (this.#ingestExpectTimer !== null) {
      clearTimeout(this.#ingestExpectTimer);
      this.#ingestExpectTimer = null;
    }
  }

  debugOpen = $state(false);

  // -- first-run welcome card (BACKLOG: how your data is stored) --------------
  /** Open until dismissed; whether it returns next launch is the toggle's
   * call. NOT flipped in the constructor: tests build Ui instances with no
   * App mounted, and the card belongs to the booted shell (loadPrefs runs
   * from ui.init, the same seam every other pref uses). */
  welcomeOpen = $state(false);
  /** The card's "don't show this again" toggle, default ON: the common
   * path reads the storage story exactly once. Lives on the slice (not the
   * component) so the Esc path through app.svelte.ts honors it too. */
  welcomeDontShowAgain = $state(true);

  loadPrefs() {
    // Surround is owned by the surround store (loads its own prefs at
    // construction); the shell reads it through the `surround` getter.
    this.railOpen = prefs.loadRailOpen();
    this.railTab = prefs.loadRailTab();
    this.uiZoom = prefs.loadUiZoom();
    this.welcomeOpen = !prefs.loadWelcomeSeen();
  }

  /** One UI-zoom step along the ladder; clamps at the ends (a Cmd+= at
   * max is simply inert — no wraparound, the browser convention). */
  stepUiZoom(delta: 1 | -1) {
    const steps = prefs.UI_ZOOM_STEPS;
    const at = (steps as readonly number[]).indexOf(this.uiZoom);
    // loadUiZoom validates by membership, so -1 means an in-memory value
    // went off-ladder (impossible today); recover from the design size.
    const from = at >= 0 ? at : (steps as readonly number[]).indexOf(1);
    const next = Math.min(steps.length - 1, Math.max(0, from + delta));
    this.uiZoom = steps[next];
    prefs.saveUiZoom(this.uiZoom);
  }

  resetUiZoom() {
    this.uiZoom = 1;
    prefs.saveUiZoom(1);
  }

  /** Switch the rail tab (pointer on the tab strip). Keyboard focus stays
   * on the rail; the focus key is left alone — a key from the hidden tab
   * simply matches no row until arrows re-anchor it. */
  setRailTab(tab: prefs.RailTab) {
    this.railTab = tab;
    prefs.saveRailTab(tab);
  }

  /** Arm the rail's inline creator to mint-and-add (thumb menu "New
   * collection…"). Opens the rail, flips to the Collections tab so the
   * input mounts in its footer, and stashes the targets captured at pick
   * time. The rail watches `pendingNewCollection` to auto-open the input;
   * its commit routes through createCollectionAndAdd, its cancel clears the
   * stash. WHY summon the rail here rather than render an input in the
   * menu: the menu model is pure data (rows carry Actions, never widgets),
   * and the rail already owns the one collection-name input — reusing it
   * keeps a single create UX (the founder's ask). */
  beginNewCollectionWithTargets(targets: string[]) {
    this.pendingNewCollection = targets;
    this.railOpen = true;
    this.setRailTab("collections");
  }

  /** Clear the pending mint-and-add stash (commit done, or cancelled). */
  clearPendingNewCollection() {
    this.pendingNewCollection = null;
  }

  /** Every dismissal path (the card's button AND Esc) lands here and
   * honors the toggle — Esc must never be a trap that brings the card
   * back forever (§0: Esc is sacred, not second-class). */
  dismissWelcome() {
    this.welcomeOpen = false;
    prefs.saveWelcomeSeen(this.welcomeDontShowAgain);
  }

  toggleRail() {
    this.railOpen = !this.railOpen;
    // Opening via `\` hands the rail keyboard focus; closing returns it.
    this.railFocused = this.railOpen;
    prefs.saveRailOpen(this.railOpen);
  }

  /** The set-surround action's sink (D6 backdrop/gutter right-click). Delegates
   * to the surround store: picking a level implies MANUAL mode and persists. */
  setSurround(level: SurroundLevel) {
    surroundStore.pick(level);
  }

  toggleCheatsheet() {
    this.cheatsheetOpen = !this.cheatsheetOpen;
  }

  /** The info seat's verb (`toggle-station-detail`). */
  toggleStationPinned() {
    this.stationPinned = !this.stationPinned;
  }

  /** Every dismissal path for the station expansion — Esc (via the
   * indicator-popover escape layer) and outside-click — clears hover AND
   * pin together: a "dismissed" station that pops back open on the next
   * reactive tick would make Esc a liar. */
  closeStation() {
    this.popoverOpen = false;
    this.stationPinned = false;
  }

  /** Flash a chip from the station (the pop move). Subtle by contract:
   * a short rising chip, no toast, no sound. */
  stationPop(text: string) {
    this.pops = [...this.pops, { id: ++this.#popSeq, text }];
  }

  dismissPop(id: number) {
    this.pops = this.pops.filter((p) => p.id !== id);
  }

  openContextMenu(
    seat: MenuSurface,
    anchor: { x: number; y: number } | null,
    arg?: unknown,
  ) {
    this.contextMenu = { seat, anchor, arg };
  }

  closeContextMenu() {
    this.contextMenu = null;
  }

  summonNote() {
    this.note = note.summon(this.scope);
  }

  cancelNote() {
    this.note = note.cancel(this.note);
  }

  /** Core scope echo: render it; a scope change cancels an open note
   * (summon-time binding holds — logic/note.ts). */
  onScopeEcho(view: ScopeView) {
    this.scope = view;
    this.note = note.onScopeChanged(this.note, view);
  }

  /** The full CAPTURE §11 contract: scope + mic + streaming + degraded.
   * No text content ever rides this channel. Mic transitions pop from the
   * station (logic/station.ts transitionPops — pure, tested): arm,
   * disarm, and an utterance finalizing (the push-to-talk release). */
  onIndicatorState(state: IndicatorState) {
    const prev = { mic: this.mic, streaming: this.streamingUtterance !== null };
    this.onScopeEcho(state.currentScope);
    this.mic = state.mic;
    this.streamingUtterance = state.streamingUtterance;
    this.asrUnavailable = state.degraded.asrUnavailable;
    const next = {
      mic: state.mic,
      streaming: state.streamingUtterance !== null,
    };
    for (const text of transitionPops(prev, next)) this.stationPop(text);
  }

  /** §5.4 tether input for logic/segments.ts: the bound scope, flagged
   * when it differs from the live selection (full preview-list compare —
   * two different single images must still tether). */
  streamingSegment(): { kind: string; count: number; differs: boolean } | null {
    const s = this.streamingUtterance;
    if (s === null) return null;
    const bound = s.boundScope;
    const differs =
      bound.kind !== this.scope.kind ||
      bound.count !== this.scope.count ||
      bound.previewHashes.join(",") !== this.scope.previewHashes.join(",");
    return { kind: bound.kind, count: bound.count, differs };
  }

  /** The RUNTIME snapshot (boot fetch + `runtime-status` events): §8.3
   * readiness gates features into existence — silently (UI R4). */
  onRuntimeStatus(status: RuntimeStatus) {
    this.runtime = status;
    this.asrReady = status.asrReady;
  }

  /** Pulse coalescing: rapid events render distinct pulses, coalesced
   * above ~5/s (UI §7.4). */
  onPulse(now: number = Date.now()) {
    if (now - this.lastPulseAt < PULSE_COALESCE_MS) return;
    this.lastPulseAt = now;
    this.pulseCount += 1;
  }
}
