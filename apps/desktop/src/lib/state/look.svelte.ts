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
  }

  close() {
    this.index = -1;
    this.order = [];
    this.zoomSession = null;
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
}
