<script lang="ts">
  /**
   * The virtualized Grid (UI §3.3): only visible rows + one screen of
   * overscan are mounted; slots are keyed by POOL INDEX (item % poolSize),
   * so DOM nodes — including <img> elements — are RECYCLED as the window
   * moves (webview img recycling, DECISIONS P16). No blob URLs; thumbnails
   * are photoproof:// protocol URLs straight from the preview cache.
   */
  import { ui } from "../state/app.svelte";
  import * as sel from "../logic/selection";
  import { THUMB_STEPS } from "../logic/sort";
  import Thumb from "./Thumb.svelte";

  const GAP = 8;
  const PAD = 10;

  let viewportEl: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let vw = $state(0);
  let vh = $state(0);

  const cell = $derived(THUMB_STEPS[ui.thumbStep]);
  const cols = $derived(Math.max(1, Math.floor((vw - PAD * 2 + GAP) / (cell + GAP))));
  const rowH = $derived(cell + GAP);
  const totalRows = $derived(Math.ceil(ui.items.length / cols));
  const totalHeight = $derived(totalRows * rowH + PAD * 2);

  // Window + 1 screen overscan above and below (UI §3.3).
  const startRow = $derived(Math.max(0, Math.floor((scrollTop - vh) / rowH)));
  const endRow = $derived(Math.min(totalRows, Math.ceil((scrollTop + 2 * vh) / rowH)));
  const startIdx = $derived(startRow * cols);
  const endIdx = $derived(Math.min(ui.items.length, endRow * cols));
  // Pool large enough for the maximal visible window at this geometry; the
  // ring mapping idx → idx % poolSize stays collision-free while
  // endIdx - startIdx ≤ poolSize.
  const poolSize = $derived(Math.max(1, (Math.ceil((3 * vh) / rowH) + 2) * cols));

  interface Slot {
    key: number;
    idx: number;
  }
  const slots = $derived.by(() => {
    const out: Slot[] = [];
    for (let idx = startIdx; idx < endIdx; idx++) {
      out.push({ key: idx % poolSize, idx });
    }
    return out;
  });

  $effect(() => {
    ui.gridCols = cols;
  });

  // Keep the focused cell visible when keyboard focus moves.
  $effect(() => {
    const f = ui.sel.focus;
    if (f < 0 || viewportEl === undefined) return;
    const row = Math.floor(f / cols);
    const top = PAD + row * rowH;
    const bottom = top + cell;
    if (top < viewportEl.scrollTop) viewportEl.scrollTop = top - GAP;
    else if (bottom > viewportEl.scrollTop + vh)
      viewportEl.scrollTop = bottom - vh + GAP;
  });

  function onThumbClick(idx: number, e: MouseEvent) {
    if (ui.note.open) ui.cancelNote(); // transient closes; draft discarded
    const hashes = ui.itemHashes;
    let next: sel.SelState;
    if (e.shiftKey) next = sel.rangeTo(ui.sel, hashes, idx);
    else if (e.metaKey || e.ctrlKey) next = sel.toggle(ui.sel, hashes, idx);
    else next = sel.click(ui.sel, hashes, idx);
    void ui.applySelection(next);
  }
</script>

<div
  class="viewport"
  role="grid"
  aria-label="Photographs"
  bind:this={viewportEl}
  bind:clientWidth={vw}
  bind:clientHeight={vh}
  onscroll={() => (scrollTop = viewportEl?.scrollTop ?? 0)}
>
  <div class="canvas" style:height="{totalHeight}px">
    {#each slots as s (s.key)}
      {@const item = ui.items[s.idx]}
      <div
        class="cell"
        style:transform="translate({PAD + (s.idx % cols) * (cell + GAP)}px, {PAD +
          Math.floor(s.idx / cols) * rowH}px)"
      >
        <Thumb
          hash={item.hash}
          hasJournal={item.hasJournal}
          offline={item.offline}
          selected={sel.isSelected(ui.sel, item.hash)}
          focused={ui.sel.focus === s.idx}
          size={cell}
          onpointerselect={(e) => onThumbClick(s.idx, e)}
        />
      </div>
    {/each}
  </div>
</div>

<style>
  .viewport {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    overflow-x: hidden;
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
</style>
