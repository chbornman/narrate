/**
 * Shell slice (COMPLETE in FOUNDATIONS; frozen contract): chrome state —
 * lights-out, rail, cheatsheet, the ONE context-menu host's state,
 * surround, fullscreen, the note transient, scope echo + pulse, ingest,
 * indicator popover, debug panel. Pure state + persistence; IPC and
 * cross-slice flows live only in the composition root (app.svelte.ts).
 */
import * as note from "../logic/note";
import type { MenuSurface } from "../actions/menus";
import type { IndicatorState, IngestStatus, ScopeView } from "../types/dto";
import type { SurroundLevel } from "../theme/surround";
import * as prefs from "./prefs";

export const SESSION_SCOPE: ScopeView = {
  kind: "session",
  count: 0,
  previewHashes: [],
};

export interface ContextMenuState {
  seat: MenuSurface;
  /** Pointer anchor; null = keyboard-summoned (host picks a default). */
  anchor: { x: number; y: number } | null;
  /** Seat argument (rail-folder row, journal entry id …). */
  arg?: unknown;
}

export class ShellSlice {
  /** Tab lights-out (featureset §0). Region OPEN state survives — every
   * chrome region renders `open && !chromeHidden`, so restore is
   * automatic. Exempt by ruling: the indicator and an open note input. */
  chromeHidden = $state(false);

  // -- rail (push panel; D5: `\` toggles) ------------------------------------
  railOpen = $state(false);
  /** Keyboard focus lives on the rail (arrow routing gates on THIS, not
   * railOpen — the rail is push-persistent). */
  railFocused = $state(false);
  railFocusKey = $state<string | null>(null);
  railCollapsed = $state<ReadonlySet<string>>(new Set());

  // -- overlays / hosts --------------------------------------------------------
  cheatsheetOpen = $state(false);
  contextMenu = $state<ContextMenuState | null>(null);
  popoverOpen = $state(false); // indicator scope popover

  // -- viewing comfort (D6) ----------------------------------------------------
  surround = $state<SurroundLevel>("black");
  fullscreen = $state(false);

  // -- capture echo ------------------------------------------------------------
  note = $state<note.NoteState>(note.CLOSED);
  scope = $state<ScopeView>(SESSION_SCOPE);
  /** CAPTURE §11 mic state (rendered by the mic mode segment); the shell
   * reports "disarmed"/unavailable until P6.2 wires the live engine. */
  mic = $state<IndicatorState["mic"]>("disarmed");
  /** §5.4: a still-streaming utterance's bound scope (tether rendering). */
  streamingUtterance = $state<IndicatorState["streamingUtterance"]>(null);
  asrUnavailable = $state(true);
  /** Monotonic pulse counter; the indicator animates on change (UI §7.4). */
  pulseCount = $state(0);
  lastPulseAt = 0;
  ingest = $state<IngestStatus>({ running: false, done: 0, total: 0, errors: 0 });

  debugOpen = $state(false);

  loadPrefs() {
    this.surround = prefs.loadSurround();
    this.railOpen = prefs.loadRailOpen();
  }

  toggleLightsOut() {
    this.chromeHidden = !this.chromeHidden;
  }

  toggleRail() {
    this.railOpen = !this.railOpen;
    // Opening via `\` hands the rail keyboard focus; closing returns it.
    this.railFocused = this.railOpen;
    prefs.saveRailOpen(this.railOpen);
  }

  setSurround(level: SurroundLevel) {
    this.surround = level;
    prefs.saveSurround(level);
  }

  toggleCheatsheet() {
    this.cheatsheetOpen = !this.cheatsheetOpen;
  }

  openContextMenu(seat: MenuSurface, anchor: { x: number; y: number } | null, arg?: unknown) {
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
   * No text content ever rides this channel. */
  onIndicatorState(state: IndicatorState) {
    this.onScopeEcho(state.currentScope);
    this.mic = state.mic;
    this.streamingUtterance = state.streamingUtterance;
    this.asrUnavailable = state.degraded.asrUnavailable;
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

  /** Pulse coalescing: rapid events render distinct pulses, coalesced
   * above ~5/s (UI §7.4). */
  onPulse(now: number = Date.now()) {
    if (now - this.lastPulseAt < 200) return;
    this.lastPulseAt = now;
    this.pulseCount += 1;
  }
}
