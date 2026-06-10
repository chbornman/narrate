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
  import { THUMB_STEPS } from "../../logic/sort";
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

  const geom = $derived(layout.snap(vw, THUMB_STEPS[ui.grid.thumbStep], GAP, PAD));
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

  function onScroll() {
    scrollTop = viewportEl?.scrollTop ?? 0;
    ui.grid.scrollAnchor = layout.captureAnchor(geom, scrollTop, units.length);
  }

  // Keep the active cell visible when keyboard focus moves.
  $effect(() => {
    const f = ui.grid.sel.focus;
    if (!restored || f < 0 || viewportEl === undefined) return;
    const top = layout.position(geom, f).y;
    const bottom = top + geom.cell;
    if (top < viewportEl.scrollTop) setScroll(top - GAP);
    else if (bottom > viewportEl.scrollTop + vh) setScroll(bottom - vh + GAP);
  });

  // ---- thumb pointer wiring ---------------------------------------------------

  function onThumbClick(idx: number, e: MouseEvent) {
    if (ui.shell.note.open) ui.cancelNote(); // transient closes; draft discarded
    ui.shell.railFocused = false; // pointer interaction returns key focus
    const hashes = ui.grid.unitHashes;
    let next: sel.SelState;
    if (e.shiftKey) next = sel.rangeTo(ui.grid.sel, hashes, idx);
    else if (e.metaKey || e.ctrlKey) next = sel.toggle(ui.grid.sel, hashes, idx);
    else next = sel.click(ui.grid.sel, hashes, idx);
    void ui.applySelection(next);
  }

  /** Double-click opens Look (featureset §0 — same verb as Enter/Space;
   * the click pair has already made the cell active). */
  function onThumbOpen() {
    void ui.perform({ kind: "open-look" });
  }

  /** Chevron control: make the pair active, then the one stack verb. */
  function onChevron(idx: number) {
    if (ui.shell.note.open) ui.cancelNote();
    ui.shell.railFocused = false;
    void (async () => {
      await ui.applySelection(sel.click(ui.grid.sel, ui.grid.unitHashes, idx));
      await ui.perform({ kind: "stack-toggle-active" });
    })();
  }

  function onThumbContextMenu(idx: number, e: MouseEvent) {
    e.preventDefault();
    const hashes = ui.grid.unitHashes;
    if (!sel.isSelected(ui.grid.sel, hashes[idx]))
      void ui.applySelection(sel.click(ui.grid.sel, hashes, idx));
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
          hasJournal={unit.primary.hasJournal}
          offline={unit.primary.offline}
          stack={ui.grid.unitStack(s.idx)}
          cellInfo={ui.grid.cellInfo}
          fileName={unit.primary.fileName}
          rating={unit.primary.rating}
          selected={sel.isSelected(ui.grid.sel, unit.primary.hash)}
          active={ui.grid.sel.focus === s.idx}
          size={geom.cell}
          onpointerselect={(e) => onThumbClick(s.idx, e)}
          onopen={onThumbOpen}
          onstacktoggle={() => onChevron(s.idx)}
          oncontextmenu={(e) => onThumbContextMenu(s.idx, e)}
        />
        {#if link.left || link.right}
          <!-- the shared pair underline: the left member spans the gap, so
               the line reads continuous across both cells (token color) -->
          <div
            class="stack-link"
            style:top="{geom.cell + 2}px"
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
