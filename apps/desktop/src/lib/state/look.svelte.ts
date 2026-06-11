/**
 * Look slice (fields + method signatures FROZEN by FOUNDATIONS; Stage B
 * implementation). Owns the LookEntry navigation order, member flips
 * (R — logic/looknav.ts), the zoom session that persists across ←/→
 * (featureset §2 — logic/zoom.ts carryOver derives the live transform in
 * LookStage), and the filmstrip toggle.
 *
 * `zoomSession` is the canonical zoom state ({mode, scale, centerFrac} —
 * dimension-independent); LookStage writes it on every gesture and
 * derives the transform from it, so ←/→ persistence and panel-resize
 * re-anchoring fall out by construction. `zoomCmd` stays the keyboard →
 * stage command channel (anchoring needs the stage's pointer position).
 */
import type { LookEntry } from "../types/display";
import { toggleFlip } from "../logic/looknav";
import { pushUndo, removeUndo, type UndoEntry } from "../logic/pencil";
import * as prefs from "./prefs";

/** Zoom session carried across ←/→ (Stage B: zoom.ts carryOver). */
export interface ZoomSessionState {
  mode: "fit" | "actual" | "free";
  scale: number;
  centerFrac: { x: number; y: number };
}

export interface ZoomCommand {
  seq: number;
  op: "toggle" | "step" | "fit" | "actual";
  delta?: 1 | -1;
}

export class LookSlice {
  /** Navigation set = entry selection (featureset §2); built by the root's
   * openLook (INTEGRATION wires the nav-set rule via looknav.ts). */
  order = $state<LookEntry[]>([]);
  index = $state(-1);

  filmstrip = $state(false);

  /** Display hashes flipped to their alt member (R), keyed by entry
   * display hash (logic/looknav.ts toggleFlip/displayedHash). */
  flips = $state<ReadonlySet<string>>(new Set());

  zoomSession = $state<ZoomSessionState | null>(null);
  /** Whether the stage is at fit — gates Space close vs hold-to-pan. */
  atFit = $state(true);
  /** Keyboard → stage command channel (zoom state lives in the stage). */
  zoomCmd = $state<ZoomCommand>({ seq: 0, op: "fit" });

  // -- grease pencil (P5.1 — CAPTURE §8, UI §4.4) -----------------------------
  /** Sticky pencil mode: persists across ←/→, resets on return to Grid. */
  pencilMode = $state(false);
  /** Tracing-paper overlay: per app-RUN session state, never persisted;
   * defaults ON (UI §4.4). Survives leaving Look. */
  overlayVisible = $state(true);
  /** Hold-E engaged (release is a raw keyup fact in LookStage). */
  eraserHeld = $state(false);
  /** Space held (raw key fact, tracked ONCE in LookStage through the
   * stageOwnsRawKeys gate — looknav.ts spaceHeldNext; PencilOverlay
   * consumes it to yield the pointer for Space-pan). */
  spaceHeld = $state(false);
  /** A stroke is in flight (pointer captured by the overlay). */
  penDown = $state(false);
  /** Undo stack (§8.5): depth 10, in-memory, THIS process only, holding
   * only strokes authored THIS SESSION (DECISIONS C4). Survives leaving
   * Look — its scope is the session, not the Look visit — and clears when
   * the backend rotates sessions across the 30-minute idle boundary. */
  undoStack = $state<readonly UndoEntry[]>([]);
  /** The session every stacked stroke belongs to (add_stroke's echo);
   * null while the stack is empty. */
  undoSessionId = $state<string | null>(null);
  /** Bumped on every stroke mutation; the overlay refetches the fold. */
  strokesVersion = $state(0);
  /** Mid-draw cancel channel (Ctrl+Z during pen-down — local, unlogged). */
  penCancelSeq = $state(0);
  /** Journal stroke-row click → flash that stroke on the overlay (§8.2). */
  strokeFlash = $state<{ id: string; seq: number } | null>(null);

  current = $derived(
    this.index >= 0 && this.index < this.order.length ? this.order[this.index] : null,
  );

  /** Hash currently displayed (honoring an R flip). */
  currentHash = $derived.by(() => {
    const e = this.current;
    if (e === null) return null;
    return this.flips.has(e.display) && e.alt !== null ? e.alt : e.display;
  });

  /** Write-scope targets for the viewed entry: displayed member first,
   * then the hidden pair member (DECISIONS 4; "● 2" for a collapsed pair). */
  currentTargets = $derived.by(() => {
    const e = this.current;
    if (e === null) return [] as string[];
    const display = this.currentHash as string;
    const other = e.alt === null ? null : display === e.display ? e.alt : e.display;
    return other === null ? [display] : [display, other];
  });

  loadPrefs() {
    this.filmstrip = prefs.loadFilmstrip();
  }

  open(order: LookEntry[], index: number) {
    this.order = order;
    this.index = index;
    // A fresh Look session: default entry = Fit (featureset §2) and no
    // carried flips; persistence applies WITHIN a session, never across.
    this.zoomSession = null;
    this.flips = new Set();
    this.atFit = true;
    this.zoomCmd = { seq: this.zoomCmd.seq + 1, op: "fit" };
    // Belt over the LookStage surface gate: whatever a prior visit left
    // behind, a fresh Look never starts with the pencil's pointer yielded.
    this.spaceHeld = false;
  }

  close() {
    this.index = -1;
    this.order = [];
    this.zoomSession = null;
    // Pencil mode resets to OFF on return to Grid (UI §4.4). The overlay
    // visibility and the undo stack deliberately survive (per app-run /
    // per SESSION — CAPTURE §8.5; the stack clears on session rotation
    // via syncUndoSession, not on leaving Look).
    this.pencilMode = false;
    this.eraserHeld = false;
    this.spaceHeld = false; // its tracker (LookStage) unmounts with Look
    this.penDown = false;
  }

  /** ←/→ within the navigation set. Returns false at the edges. */
  next(delta: 1 | -1): boolean {
    const i = this.index + delta;
    if (i < 0 || i >= this.order.length) return false;
    this.index = i;
    return true;
  }

  toggleFilmstrip() {
    this.filmstrip = !this.filmstrip;
    prefs.saveFilmstrip(this.filmstrip);
  }

  /** R: flip the displayed pair member; lone images no-op (featureset §5). */
  flipMember() {
    const e = this.current;
    if (e === null) return;
    this.flips = toggleFlip(this.flips, e);
  }

  sendZoom(op: ZoomCommand["op"], delta?: 1 | -1) {
    this.zoomCmd = { seq: this.zoomCmd.seq + 1, op, delta };
  }

  // ---- grease pencil (P5.1) --------------------------------------------------

  /** B: sticky toggle. Refused while the overlay is hidden (the def's
   * enabled gate is the keyboard guard; this guards pointer/menu paths). */
  togglePencil() {
    if (!this.overlayVisible) return;
    this.pencilMode = !this.pencilMode;
    if (!this.pencilMode) this.eraserHeld = false;
  }

  /** O: overlay toggle. Hiding the paper exits pencil mode (UI §4.4 —
   * you cannot draw on paper you cannot see). */
  toggleOverlay() {
    this.overlayVisible = !this.overlayVisible;
    if (!this.overlayVisible) {
      this.pencilMode = false;
      this.eraserHeld = false;
    }
  }

  /** Pen-up committed: stack the stroke (depth 10) and refresh overlays.
   * The stack is session-scoped (§8.5/C4): a stroke landing in a NEW
   * backend session (the 30-minute boundary rotated under us) starts a
   * fresh stack — prior-session strokes are journal-panel/eraser work,
   * never Ctrl+Z. */
  onStrokeCommitted(id: string, hash: string, sessionId: string) {
    if (sessionId !== this.undoSessionId) {
      this.undoStack = [];
      this.undoSessionId = sessionId;
    }
    this.undoStack = pushUndo(this.undoStack, { id, hash });
    this.strokesVersion += 1;
  }

  /** A stroke got retracted (undo, eraser, or the journal panel): drop it
   * from the stack wherever it sits and refresh. */
  onStrokeRetracted(id: string) {
    this.undoStack = removeUndo(this.undoStack, id);
    this.strokesVersion += 1;
  }

  /** Session echo from any activity touch (report_activity/§2.1): session
   * closure is lazy, so a changed id IS the close — drop the whole stack
   * (§8.5 "cleared at session close"); its strokes belong to a session
   * that no longer exists. */
  syncUndoSession(sessionId: string) {
    if (this.undoSessionId !== null && sessionId !== this.undoSessionId) {
      this.undoStack = [];
      this.undoSessionId = null;
    }
  }

  peekUndo(): UndoEntry | null {
    return this.undoStack.length > 0 ? this.undoStack[this.undoStack.length - 1] : null;
  }

  /** Ctrl+Z during pen-down: ask the overlay to discard the in-flight
   * stroke (local and unlogged — UI §4.4). */
  requestPenCancel() {
    this.penCancelSeq += 1;
  }

  flashStroke(id: string) {
    this.strokeFlash = { id, seq: (this.strokeFlash?.seq ?? 0) + 1 };
  }
}
