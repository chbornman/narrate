<script lang="ts">
  /**
   * The virtualized Grid (UI §3.3) over DisplayUnit[] — STAGE A. Geometry
   * is logic/gridlayout.ts: integer-column snap (panel push re-snaps by
   * construction — vw is reactive), window + 1 screen overscan, pool-keyed
   * slots so DOM nodes (including <img>) are RECYCLED as the window moves
   * (P16); no blob URLs. The scroll anchor (first visible unit) is
   * captured on every scroll and restored on mount (Look round-trips) and
   * across column-count/row-height changes (resize, panel drag, size key).
   *
   * The canvas background is the SURROUND (D6 — tokens.css retunes it).
   * Pointer wiring: thumb click/dblclick/chevron; right-click on a thumb
   * opens the "thumb" seat (selecting it first if unselected), empty
   * gutter opens the "gutter" seat; a drag from EMPTY GUTTER rubber-bands
   * (logic/marquee.ts — a thumb drag never marquees), Ctrl = additive.
   */
  import { ui } from "../../state/app.svelte";
  import * as sel from "../../logic/selection";
  import * as layout from "../../logic/gridlayout";
  import * as marquee from "../../logic/marquee";
  import * as stacks from "../../logic/stacks";
  import { infoStripHeight } from "../../logic/cellinfo";
  import { THUMB_STEPS } from "../../logic/sort";
  import { PreviewPrioritizer } from "../../logic/previewprioritize";
  import * as ipc from "../../ipc/commands";
  import Thumb from "./Thumb.svelte";
  import Marquee from "./Marquee.svelte";

  const GAP = 8;
  const PAD = 10;
  /** Expanded pair members nudge toward each other (dogfood round 1: the
   * gap tightens between linked members; columns stay snapped). */
  const NUDGE = 2;

  let viewportEl: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let vw = $state(0);
  let vh = $state(0);

  // Cell-info is GLOBAL (one level for all cells), so its strip height is a
  // single fixed value reserved on EVERY row — rows stay uniform and the
  // virtualizer math is unchanged beyond the larger rowH.
  const infoStrip = $derived(infoStripHeight(ui.grid.cellInfo));
  const geom = $derived(layout.snap(vw, THUMB_STEPS[ui.grid.thumbStep], GAP, PAD, infoStrip));
  const units = $derived(ui.grid.units);
  // EXPANDED pair members read as linked (featureset §5 dogfood round 1):
  // adjacency math is pure (stacks.expandedLinks); rendering = an inward
  // nudge + a shared underline bridging the remaining gap.
  const links = $derived(stacks.expandedLinks(ui.grid.stackModel, geom.cols));
  const totalHeight = $derived(layout.totalHeight(geom, units.length));
  const range = $derived(layout.visibleRange(geom, scrollTop, vh, units.length));
  const poolSize = $derived(layout.poolSize(geom, vh));

  interface Slot {
    key: number;
    idx: number;
  }
  const slots = $derived.by(() => {
    const out: Slot[] = [];
    for (let idx = range.start; idx < range.end; idx++) {
      out.push({ key: idx % poolSize, idx });
    }
    return out;
  });

  // Geometry report: edge/page moves and panel re-snap math read these.
  $effect(() => {
    ui.grid.gridCols = geom.cols;
    ui.grid.gridRowsPerPage = layout.rowsPerPage(geom, vh);
  });

  // ---- viewport-first preview generation (OD-2) -----------------------------

  // Ask the backend to generate the previews the user is scrolled to FIRST.
  // The ingest preview pass otherwise runs in scan order, so after a fresh scan
  // or "Rebuild all previews" scrolling to row 200 waits on rows 1-199. The
  // prioritizer debounces scroll-settle, sends ONLY hashes whose thumb isn't
  // made yet (previewReady false), dedupes, and skips an unchanged window — so
  // it reorders SERVER generation without fighting the ordinary `<img>` thumb
  // load (a generated thumb still draws as a photoproof:// URL as before).
  const prioritizer = new PreviewPrioritizer((hashes) => {
    void ipc.prioritizePreviews(hashes).catch(() => {
      // Best-effort: a failed prioritize only means the pump keeps its existing
      // order; the thumb still heals off `previews-changed`. Never blocks the UI.
    });
  });
  // `range` (visible window) and `units` (re-list / re-pair) drive this; the
  // visible slice is the only scope the user is looking at, so we never
  // prioritize an offscreen scope. A scope swap replaces `units`, producing a
  // fresh window and a fresh send.
  $effect(() => {
    const visible = units.slice(range.start, range.end);
    prioritizer.notify(visible);
  });
  $effect(() => {
    // Cancel the debounce + forget the last-sent set when the grid unmounts.
    return () => prioritizer.reset();
  });

  // ---- scroll anchor (featureset §1: position preserved) --------------------

  /** Mount restore happened (gates the effects below so they cannot fight
   * the initial position). */
  let restored = $state(false);

  function setScroll(top: number) {
    if (viewportEl === undefined) return;
    viewportEl.scrollTop = top;
    scrollTop = viewportEl.scrollTop;
  }

  // Restore once the container has measured (Look round-trip / remount).
  $effect(() => {
    if (restored || viewportEl === undefined || vw <= 0) return;
    const anchor = ui.grid.scrollAnchor;
    if (anchor !== null) setScroll(layout.restoreScroll(geom, anchor, units.length, vh));
    restored = true;
  });

  // Re-snap stability: when the column count or row height changes (resize,
  // panel drag, thumb size), keep the anchored unit in place.
  let prevCols = 0;
  let prevRowH = 0;
  $effect(() => {
    const { cols, rowH } = geom;
    if (!restored || (cols === prevCols && rowH === prevRowH)) {
      prevCols = cols;
      prevRowH = rowH;
      return;
    }
    prevCols = cols;
    prevRowH = rowH;
    const anchor = ui.grid.scrollAnchor;
    if (anchor !== null) setScroll(layout.restoreScroll(geom, anchor, units.length, vh));
  });

  /** The IMAGE under the anchor index — mid-ingest scroll stability
   * (founder, dogfood round 3): every re-list can re-sort (exif fills
   * captureTs under capture-desc), moving content beneath a still
   * viewport. Pinning the anchor by hash is B64's identity rule applied
   * to scroll position. */
  let anchorHash: string | null = null;

  function onScroll() {
    scrollTop = viewportEl?.scrollTop ?? 0;
    const anchor = layout.captureAnchor(geom, scrollTop, units.length);
    ui.grid.scrollAnchor = anchor;
    anchorHash = anchor === null ? null : (ui.grid.unitHashes[anchor.index] ?? null);
  }

  // Items changed (ingest re-list, stack re-pair): keep the anchored
  // IMAGE where it was, not whatever now occupies its old index.
  let prevUnitHashes = ui.grid.unitHashes;
  $effect(() => {
    const hashes = ui.grid.unitHashes;
    if (hashes === prevUnitHashes) return;
    prevUnitHashes = hashes;
    const anchor = ui.grid.scrollAnchor;
    if (!restored || anchorHash === null || anchor === null) return;
    const idx = hashes.indexOf(anchorHash);
    if (idx < 0 || idx === anchor.index) return;
    const moved = { index: idx, offset: anchor.offset };
    ui.grid.scrollAnchor = moved;
    setScroll(layout.restoreScroll(geom, moved, hashes.length, vh));
  });

  // Keep the active cell visible when the USER moves focus (focusNav —
  // never on focus value alone: a re-list remaps focus by hash, and
  // following the remap would yank the viewport after a refresh the
  // user never asked for).
  let prevFocusNav = ui.grid.focusNav;
  $effect(() => {
    const nav = ui.grid.focusNav;
    const f = ui.grid.sel.focus;
    if (nav === prevFocusNav) return;
    prevFocusNav = nav;
    if (!restored || f < 0 || viewportEl === undefined) return;
    const top = layout.position(geom, f).y;
    // The cell is the strip plus the image box; scroll the WHOLE cell into view.
    const bottom = top + geom.info + geom.cell;
    if (top < viewportEl.scrollTop) setScroll(top - GAP);
    else if (bottom > viewportEl.scrollTop + vh) setScroll(bottom - vh + GAP);
  });

  // ---- thumb pointer wiring ---------------------------------------------------

  // Pointer handlers carry the rendered unit's HASH, never a slot index:
  // the virtualizer recycles slots and an ingest refresh can re-sort
  // `units` between render and click (exif passes fill captureTs
  // mid-session) — an index crossing that boundary acts on a different
  // image (founder dogfood, June 2026). The index each selection verb
  // needs is resolved against the CURRENT order at event time.
  function onThumbClick(hash: string, e: MouseEvent) {
    if (ui.shell.note.open) ui.cancelNote(); // transient closes; draft discarded
    ui.shell.railFocused = false; // pointer interaction returns key focus
    const hashes = ui.grid.unitHashes;
    const idx = hashes.indexOf(hash);
    if (idx < 0) return;
    let next: sel.SelState;
    if (e.shiftKey) next = sel.rangeTo(ui.grid.sel, hashes, idx);
    else if (e.metaKey || e.ctrlKey) next = sel.toggle(ui.grid.sel, hashes, idx);
    else next = sel.click(ui.grid.sel, hashes, idx);
    void ui.applySelection(next);
  }

  /** Double-click opens Look (featureset §0 — same verb as Enter; the
   * click pair has already made the cell active). */
  function onThumbOpen() {
    void ui.perform({ kind: "open-look" });
  }

  /** Chevron control: make the pair active, then toggle THAT pair — the
   * identity pinned by hash for the toggle too, because applySelection
   * awaits the set_scope IPC round-trip and a re-sort landing in that
   * gap retargets any focus-index path. */
  function onChevron(hash: string) {
    if (ui.shell.note.open) ui.cancelNote();
    ui.shell.railFocused = false;
    void (async () => {
      const hashes = ui.grid.unitHashes;
      const idx = hashes.indexOf(hash);
      if (idx < 0) return;
      await ui.applySelection(sel.click(ui.grid.sel, hashes, idx));
      ui.grid.togglePairByHash(hash);
    })();
  }

  function onThumbContextMenu(hash: string, e: MouseEvent) {
    e.preventDefault();
    const hashes = ui.grid.unitHashes;
    if (hashes.indexOf(hash) < 0) return;
    if (!sel.isSelected(ui.grid.sel, hash))
      void ui.applySelection(sel.click(ui.grid.sel, hashes, hashes.indexOf(hash)));
    ui.shell.openContextMenu("thumb", { x: e.clientX, y: e.clientY });
  }

  function onGutterContextMenu(e: MouseEvent) {
    if (e.target !== e.currentTarget) return; // thumbs handle their own
    e.preventDefault();
    ui.shell.openContextMenu("gutter", { x: e.clientX, y: e.clientY });
  }

  // ---- marquee (featureset §1; math in logic/marquee.ts) ------------------------

  interface Drag {
    origin: marquee.Point;
    current: marquee.Point;
    additive: boolean;
    active: boolean;
  }
  let drag = $state<Drag | null>(null);
  const marqueeRect = $derived(
    drag !== null && drag.active ? marquee.rectFrom(drag.origin, drag.current) : null,
  );

  function canvasPoint(e: PointerEvent): marquee.Point {
    const r = (viewportEl as HTMLDivElement).getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top + scrollTop };
  }

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0 || viewportEl === undefined) return;
    // Scrollbar zone is not gutter.
    const r = viewportEl.getBoundingClientRect();
    if (e.clientX - r.left >= viewportEl.clientWidth) return;
    const onThumb = (e.target as Element).closest(".cell") !== null;
    if (marquee.classifyDrag(onThumb) !== "marquee") return; // item-drag = M3
    const pt = canvasPoint(e);
    drag = { origin: pt, current: pt, additive: e.ctrlKey || e.metaKey, active: false };
    viewportEl.setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    if (drag === null) return;
    const current = canvasPoint(e);
    drag = {
      ...drag,
      current,
      active: drag.active || marquee.isDrag(drag.origin, current),
    };
  }

  function onPointerUp() {
    if (drag === null) return;
    const { origin, current, additive, active } = drag;
    drag = null;
    if (ui.shell.note.open) ui.cancelNote();
    ui.shell.railFocused = false;
    const hits = active
      ? marquee.hitTest(marquee.rectFrom(origin, current), geom, units.length)
      : []; // a plain gutter click clears (additive leaves the selection)
    void ui.applySelection(
      sel.marqueeMerge(ui.grid.sel, ui.grid.unitHashes, hits, additive),
    );
  }
</script>

<div
  class="viewport"
  role="grid"
  aria-label="Photographs"
  tabindex="-1"
  bind:this={viewportEl}
  bind:clientWidth={vw}
  bind:clientHeight={vh}
  onscroll={onScroll}
  oncontextmenu={onGutterContextMenu}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={() => (drag = null)}
>
  <div
    class="canvas"
    style:height="{totalHeight}px"
    role="presentation"
    oncontextmenu={onGutterContextMenu}
  >
    {#each slots as s (s.key)}
      {@const unit = units[s.idx]}
      {@const pos = layout.position(geom, s.idx)}
      {@const link = links[s.idx] ?? { left: false, right: false }}
      {@const dx = link.right ? NUDGE : link.left ? -NUDGE : 0}
      <div class="cell" style:transform="translate({pos.x + dx}px, {pos.y}px)">
        <Thumb
          hash={unit.primary.hash}
          previewReady={unit.primary.previewReady ?? true}
          previewPing={ui.grid.previewPing}
          hasJournal={unit.primary.hasJournal}
          offline={unit.primary.offline}
          stack={ui.grid.unitStack(s.idx)}
          cellInfo={ui.grid.cellInfo}
          fileName={unit.primary.fileName}
          rating={unit.primary.rating}
          selected={sel.isSelected(ui.grid.sel, unit.primary.hash)}
          active={ui.grid.sel.focus === s.idx}
          size={geom.cell}
          infoStrip={geom.info}
          intensity={ui.heatOn ? (ui.grid.intensity.get(unit.primary.hash) ?? 0) : 0}
          onpointerselect={(e) => onThumbClick(unit.primary.hash, e)}
          onopen={onThumbOpen}
          onstacktoggle={() => onChevron(unit.primary.hash)}
          oncontextmenu={(e) => onThumbContextMenu(unit.primary.hash, e)}
        />
        {#if link.left || link.right}
          <!-- the shared pair underline: the left member spans the gap, so
               the line reads continuous across both cells (token color) -->
          <div
            class="stack-link"
            style:top="{geom.info + geom.cell + 2}px"
            style:width="{geom.cell + (link.right ? GAP - 2 * NUDGE : 0)}px"
          ></div>
        {/if}
      </div>
    {/each}
    <Marquee rect={marqueeRect} />
  </div>
</div>

<style>
  .viewport {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    overflow-x: hidden;
    background: var(--surround);
  }
  .canvas {
    position: relative;
  }
  .cell {
    position: absolute;
    top: 0;
    left: 0;
    will-change: transform;
  }
  /* Expanded-pair link: a quiet shared underline in the row gap. */
  .stack-link {
    position: absolute;
    left: 0;
    height: 2px;
    border-radius: 1px;
    background: var(--text-faint);
    pointer-events: none;
  }
</style>
