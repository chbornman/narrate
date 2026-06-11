/**
 * Grid slice — fields + method signatures FROZEN by FOUNDATIONS; Stage A
 * implementation. Owns folder/items/sort/size/selection/stacks/cell-info/
 * scroll-anchor state and produces the cross-stage seam values:
 *
 *   units            DisplayUnit[] (stack pairing — logic/stacks.ts)
 *   selectionTargets stack-EXPANDED ordered write-scope targets
 *                    (collapsed pair → display member first, JPEG then
 *                    RAW — CAPTURE event_targets.position, DECISIONS 4)
 *   activeHash       the active (focused) image
 *
 * Stack collapse state is LIVE and reversible (featureset §5): a global
 * flag + per-pair overrides (XOR), plus per-pair display flips (R in
 * Grid). Overrides/flips are session-local; the global flag persists.
 */
import * as sel from "../logic/selection";
import { sortItems, THUMB_STEPS, type SortMode } from "../logic/sort";
import * as stacks from "../logic/stacks";
import { nextLevel } from "../logic/cellinfo";
import type { GridItem } from "../types/dto";
import type { DisplayUnit } from "../types/display";
import * as prefs from "./prefs";

export type CellInfoLevel = prefs.CellInfoLevel;

export class GridSlice {
  // -- folder -------------------------------------------------------------------
  rootId = $state<string | null>(null);
  folder = $state("");

  // -- items / sort / size --------------------------------------------------------
  rawItems = $state<GridItem[]>([]);
  sort = $state<SortMode>("capture-desc");
  thumbStep = $state(1);

  // -- selection (focus ≡ active — logic/selection.ts) ---------------------------
  sel = $state<sel.SelState>(sel.EMPTY);

  // -- stacks (D1; logic/stacks.ts) ------------------------------------------------
  /** Global collapse state (live, reversible). */
  stackGlobalCollapsed = $state(true);
  /** Pair keys overriding the global state (per-pair toggle). */
  pairOverrides = $state<ReadonlySet<string>>(new Set());
  /** Pair keys flipped to the non-preferred member (R, session-local). */
  pairFlips = $state<ReadonlySet<string>>(new Set());
  /** Which member a collapsed pair displays (Settings → "Stacked pairs
   * show", dogfood round 1). The BACKEND settings store owns persistence
   * (both windows share it); the root's applySettings wires boot + live
   * edits here. */
  stackDisplay = $state<stacks.StackDisplayMember>("jpeg");

  // -- cell info (T cycle; logic/cellinfo.ts) ---------------------------------------
  cellInfo = $state<CellInfoLevel>("none");

  // -- scroll anchor (preserved across Look round-trips and folder revisits) ------
  scrollAnchor = $state<{ index: number; offset: number } | null>(null);

  /** `previews-changed` ping (hash-aware, the journal-changed seam):
   * thumbs matching `hashes` retry their preview load NOW with a fresh
   * budget — without it, any ingest outlasting the Thumb retry cap left
   * permanent blanks (founder dogfood, June 2026). `seq` makes each
   * event observable even when consecutive payloads carry equal sets. */
  previewPing = $state<{ seq: number; hashes: ReadonlySet<string> }>({
    seq: 0,
    hashes: new Set(),
  });

  onPreviewsChanged(hashes: string[]) {
    this.previewPing = { seq: this.previewPing.seq + 1, hashes: new Set(hashes) };
  }

  /** Viewport geometry, reported by Grid.svelte (page/edge move math). */
  gridCols = 1;
  gridRowsPerPage = 1;

  // -- derived seam values ---------------------------------------------------------
  items = $derived(sortItems(this.rawItems, this.sort));
  itemHashes = $derived(this.items.map((i) => i.hash));

  /** Pairing + collapse over the sorted items (units + per-unit pair key). */
  stackModel = $derived(
    stacks.buildUnits(this.items, {
      globalCollapsed: this.stackGlobalCollapsed,
      overrides: this.pairOverrides,
      flips: this.pairFlips,
      display: this.stackDisplay,
    }),
  );

  /** Grid CELLS: a collapsed pair is ONE unit (display member on top). */
  units = $derived<DisplayUnit[]>(this.stackModel.units);
  unitHashes = $derived(this.units.map((u) => u.primary.hash));

  /** The ACTIVE image (featureset §1) — Look entry, inspector, R flip. */
  activeHash = $derived(
    this.sel.focus >= 0 && this.sel.focus < this.units.length
      ? this.units[this.sel.focus].primary.hash
      : null,
  );
  /** Pair MEMBERSHIP (collapsed or expanded) — gates the stack menu row. */
  activeIsPair = $derived(
    this.sel.focus >= 0 && this.sel.focus < this.units.length
      ? this.stackModel.pairs[this.sel.focus] !== null
      : false,
  );
  /** The active cell is a COLLAPSED pair (two targets, R flips display). */
  activePairCollapsed = $derived(
    this.sel.focus >= 0 && this.sel.focus < this.units.length
      ? this.units[this.sel.focus].alt !== null
      : false,
  );

  /** Stack-EXPANDED ordered targets (CAPTURE §3): a collapsed pair in the
   * selection reports BOTH hashes, display member first (the indicator
   * truthfully reads "● 2" for one selected cell). */
  selectionTargets = $derived<string[]>(stacks.expandTargets(this.sel.order, this.units));

  loadPrefs() {
    this.thumbStep = prefs.loadThumbStep();
    this.cellInfo = prefs.loadCellInfo();
    this.stackGlobalCollapsed = prefs.loadStackGlobal();
  }

  /** Replace the item list (folder open / ingest refresh), reconciling
   * selection and focus. Focus follows the IMAGE, not its index: an
   * ingest refresh re-sorts whenever an exif pass fills captureTs
   * mid-session, and a merely length-clamped focus index silently
   * retargets every active-cell verb at a different image (founder
   * dogfood, June 2026). fixupSelection owns the hash-mapping invariant
   * (hidden alts follow their display member; vanished hashes drop). */
  setItems(items: GridItem[]) {
    const prevActive = this.activeHash;
    this.rawItems = items;
    this.fixupSelection(prevActive);
  }

  setSelection(next: sel.SelState) {
    this.sel = next;
  }

  setSort(mode: SortMode) {
    this.sort = mode;
    if (this.rootId !== null) prefs.saveSort(this.rootId, this.folder, mode);
  }

  setThumbStep(step: number) {
    this.thumbStep = Math.max(0, Math.min(THUMB_STEPS.length - 1, step));
    prefs.saveThumbStep(this.thumbStep);
  }

  cycleCellInfo() {
    this.cellInfo = nextLevel(this.cellInfo);
    prefs.saveCellInfo(this.cellInfo);
  }

  // -- stack verbs (signatures frozen) ----------------------------------------------

  /** The unit's collapse state ("solo" cells have no chevron). */
  unitStack(index: number): "solo" | "collapsed" | "expanded" {
    if (index < 0 || index >= this.units.length) return "solo";
    if (this.stackModel.pairs[index] === null) return "solo";
    return this.units[index].alt !== null ? "collapsed" : "expanded";
  }

  /** Per-pair live collapse/expand for the unit at `index` (the chevron
   * control); toggleActiveStack routes the menu/active verb here. */
  togglePairAt(index: number) {
    const key = index >= 0 && index < this.units.length ? this.stackModel.pairs[index] : null;
    if (key === null) return;
    const prevActive = this.activeHash;
    const next = new Set(this.pairOverrides);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    this.pairOverrides = next;
    this.fixupSelection(prevActive);
  }

  toggleActiveStack() {
    this.togglePairAt(this.sel.focus);
  }

  /** The RAW/JPEG pair-mate of `hash`, or null for solo images and hashes
   * not on the surface. Consumed by the journal's "+N others" mark
   * (DECISIONS B61: the mate is the same picture — never an "other").
   * Resolved through the unit HOSTING the hash: a collapsed pair carries
   * the mate as its hidden alt; an expanded pair hosts the mate in its
   * own unit under the same pair key (collapse state must not change what
   * counts as a different image). */
  pairMateOf(hash: string): string | null {
    const i = this.units.findIndex(
      (u) => u.primary.hash === hash || u.alt?.hash === hash,
    );
    if (i < 0) return null;
    const unit = this.units[i];
    if (unit.alt !== null)
      return unit.primary.hash === hash ? unit.alt.hash : unit.primary.hash;
    const key = this.stackModel.pairs[i];
    if (key === null) return null;
    const j = this.stackModel.pairs.findIndex((k, idx) => k === key && idx !== i);
    return j >= 0 ? this.units[j].primary.hash : null;
  }

  /** The chevron's path: toggle the pair HOSTING `hash` (display member
   * or hidden alt) resolved at execute time. Pointer identity must
   * survive an items re-sort landing inside the click's own async
   * selection round-trip — the focus-index route toggled a different
   * image's pair when that happened (founder dogfood, June 2026). The
   * keyboard/menu verb stays focus-based: its subject IS the focus at
   * perform time. */
  togglePairByHash(hash: string) {
    const index = this.units.findIndex(
      (u) => u.primary.hash === hash || u.alt?.hash === hash,
    );
    if (index >= 0) this.togglePairAt(index);
  }

  setAllStacks(collapsed: boolean) {
    const prevActive = this.activeHash;
    this.stackGlobalCollapsed = collapsed;
    this.pairOverrides = new Set();
    prefs.saveStackGlobal(collapsed);
    this.fixupSelection(prevActive);
  }

  /** "Stacked pairs show: JPEG | RAW" — takes effect live. Flips are
   * relative to the preferred member, so they reset; selection follows
   * each cell's new display member (persistence is the backend's). */
  setStackDisplay(member: stacks.StackDisplayMember) {
    if (member === this.stackDisplay) return;
    const prevActive = this.activeHash;
    this.stackDisplay = member;
    this.pairFlips = new Set();
    this.fixupSelection(prevActive);
  }

  /** R on a COLLAPSED pair in Grid: flip the displayed member (featureset
   * §5; targets re-order display-first — the look slice mirrors this). */
  flipActiveMember() {
    const i = this.sel.focus;
    if (i < 0 || i >= this.units.length || this.units[i].alt === null) return;
    const key = this.stackModel.pairs[i];
    if (key === null) return;
    const next = new Set(this.pairFlips);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    this.pairFlips = next;
    // The active cell keeps its position; the selection follows the new
    // display member (the old one is now the hidden alt).
    this.fixupSelection(this.unitHashes[i] ?? null);
  }

  /** Re-point selection at surviving cells after collapse/flip changed
   * unit membership: a hash that became a hidden alt follows its cell's
   * display member; vanished hashes drop; order + dedup preserved. */
  private fixupSelection(prevActive: string | null) {
    const toDisplay = new Map<string, string>();
    for (const u of this.units) {
      toDisplay.set(u.primary.hash, u.primary.hash);
      if (u.alt !== null) toDisplay.set(u.alt.hash, u.primary.hash);
    }
    const order: string[] = [];
    for (const h of this.sel.order) {
      const mapped = toDisplay.get(h);
      if (mapped !== undefined && !order.includes(mapped)) order.push(mapped);
    }
    const focusHash = prevActive === null ? null : (toDisplay.get(prevActive) ?? null);
    const focus =
      focusHash === null
        ? Math.min(this.sel.focus, this.units.length - 1)
        : this.unitHashes.indexOf(focusHash);
    this.sel = { order, focus, anchor: focus };
  }

  /**
   * Auto-advance in Grid: select the next unit after the active one
   * (single-selection invariant preserved). Returns false at the end.
   */
  advanceActive(): boolean {
    const next = this.sel.focus + 1;
    if (this.sel.focus < 0 || next >= this.units.length) return false;
    this.sel = sel.click(this.sel, this.unitHashes, next);
    return true;
  }
}
