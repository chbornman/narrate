<script lang="ts">
  /**
   * Look (UI §4) — the canonical name. Edge-to-edge single image at the
   * preview (display) tier; no chrome over the image, ever. Pan/zoom per
   * §4.2; the filmstrip (`F`) is the only optional element.
   */
  import { ui } from "../state/app.svelte";
  import { displayUrl, thumbUrl } from "../ipc/urls";

  let cw = $state(0);
  let ch = $state(0);
  let natW = $state(0);
  let natH = $state(0);

  // View transform: fit (scale to contain) or explicit { scale, tx, ty }.
  let fit = $state(true);
  let scale = $state(1);
  let tx = $state(0);
  let ty = $state(0);

  const fitScale = $derived(
    natW > 0 && natH > 0 && cw > 0 && ch > 0
      ? Math.min(cw / natW, ch / natH)
      : 1,
  );
  const effScale = $derived(fit ? fitScale : scale);
  const effTx = $derived(fit ? (cw - natW * fitScale) / 2 : tx);
  const effTy = $derived(fit ? (ch - natH * fitScale) / 2 : ty);

  const hash = $derived(ui.lookHash);

  // Reset to fit on image change (state persists per §4.4 only for pencil,
  // which is M2a; zoom resetting to fit on prev/next is the quiet default).
  $effect(() => {
    void hash;
    fit = true;
  });

  // Zoom commands from the keyboard map (App dispatches; state lives here).
  $effect(() => {
    const cmd = ui.lookZoom;
    if (cmd.seq === 0) return;
    if (cmd.op === "fit") {
      fit = true;
    } else if (cmd.op === "toggle") {
      if (fit) zoomTo(1, cw / 2, ch / 2);
      else fit = true;
    } else if (cmd.op === "step") {
      const factor = cmd.delta === 1 ? 1.25 : 0.8;
      zoomTo(effScale * factor, cw / 2, ch / 2);
    }
  });

  /** Zoom toward a container point, keeping it stationary on screen. */
  function zoomTo(nextScale: number, px: number, py: number) {
    const s0 = effScale;
    const s1 = Math.max(fitScale * 0.5, Math.min(8, nextScale));
    const ix = (px - effTx) / s0;
    const iy = (py - effTy) / s0;
    scale = s1;
    tx = px - ix * s1;
    ty = py - iy * s1;
    fit = false;
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    zoomTo(effScale * factor, e.offsetX, e.offsetY);
  }

  // Drag pans (pencil is M2a; drag always pans in M1).
  let dragging = false;
  let lastX = 0;
  let lastY = 0;
  function onPointerDown(e: PointerEvent) {
    dragging = true;
    lastX = e.clientX;
    lastY = e.clientY;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onPointerMove(e: PointerEvent) {
    if (!dragging || fit) return;
    tx += e.clientX - lastX;
    ty += e.clientY - lastY;
    lastX = e.clientX;
    lastY = e.clientY;
  }
  function onPointerUp() {
    dragging = false;
  }

  const stripStart = $derived(Math.max(0, ui.lookIndex - 8));
  const stripItems = $derived(ui.lookOrder.slice(stripStart, stripStart + 17));
  const gridByHash = $derived(new Map(ui.items.map((i) => [i.hash, i])));
</script>

<div
  class="look"
  bind:clientWidth={cw}
  bind:clientHeight={ch}
  onwheel={onWheel}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  role="img"
  aria-label="Photograph"
>
  {#if hash !== null}
    <img
      src={displayUrl(hash)}
      alt=""
      draggable="false"
      style:transform="translate({effTx}px, {effTy}px) scale({effScale})"
      onload={(e) => {
        const img = e.currentTarget as HTMLImageElement;
        natW = img.naturalWidth;
        natH = img.naturalHeight;
      }}
    />
  {/if}

  {#if ui.filmstrip}
    <div class="filmstrip">
      {#each stripItems as h, i (h)}
        {@const item = gridByHash.get(h)}
        <button
          class="strip-thumb"
          class:current={stripStart + i === ui.lookIndex}
          onclick={() => void ui.lookNav(stripStart + i - ui.lookIndex)}
          aria-label="Show image"
        >
          <img src={thumbUrl(h)} alt="" draggable="false" />
          {#if item?.hasJournal}<span class="journal-dot"></span>{/if}
          {#if item?.offline}<span class="offline-badge">⏏</span>{/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .look {
    position: absolute;
    inset: 0;
    overflow: hidden;
    background: var(--bg); /* near-black canvas; no borders, no info bar */
  }
  .look > img {
    position: absolute;
    top: 0;
    left: 0;
    transform-origin: 0 0;
    will-change: transform;
    user-select: none;
    -webkit-user-select: none;
  }
  .filmstrip {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 80px; /* ≤ 88 px (§4.1) */
    display: flex;
    gap: 4px;
    align-items: center;
    padding: 4px 8px;
    background: color-mix(in srgb, var(--bg) 85%, transparent);
    overflow-x: auto;
  }
  .strip-thumb {
    position: relative;
    flex: 0 0 auto;
    width: 68px;
    height: 68px;
    padding: 0;
    border: 1px solid transparent;
    background: var(--bg-raised);
    border-radius: 2px;
    overflow: hidden;
  }
  .strip-thumb.current {
    border-color: var(--focus);
  }
  .strip-thumb img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }
  .journal-dot {
    position: absolute;
    right: 4px;
    bottom: 4px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--red-dull);
  }
  .offline-badge {
    position: absolute;
    right: 3px;
    top: 1px;
    color: var(--text-dim);
    font-size: 9px;
  }
</style>
