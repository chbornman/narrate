<script lang="ts">
  /**
   * Journal stroke micro-preview (UI §8.2, P5.1): the stroke path drawn
   * over a small thumbnail at the row, via the same geometry module the
   * Look overlay renders from (logic/stroke.ts) and the existing
   * photoproof:// thumbnail protocol. Clicking flashes the stroke on the
   * Look overlay (the parent routes the Action).
   *
   * Drawn as stored — the thumbnail is display-oriented, and orientation
   * drift compensation is the Look overlay's job (it knows the current
   * display orientation; the row does not re-fetch metadata).
   */
  import { thumbUrl } from "../../ipc/urls";
  import { denormalize, longEdge, QUANT, svgPathFor, wireToNorm } from "../../logic/stroke";
  import { tooltip } from "../../primitives/tooltip";
  import type { StrokeDto } from "../../types/dto";

  let {
    stroke,
    hash,
    onflash,
  }: {
    stroke: StrokeDto;
    hash: string;
    onflash: () => void;
  } = $props();

  /** Thumbnail's natural (display-oriented) dims — the path's pixel box. */
  let dims = $state<{ w: number; h: number } | null>(null);

  const path = $derived(
    dims === null
      ? ""
      : svgPathFor(stroke.points.map((pt) => denormalize(wireToNorm(pt), dims!))),
  );
  // baseW is stored in QUANT-ths of the image long edge (the EVENTS §3.3
  // wire format) — the denominator is the wire constant, not a tuning.
  const width = $derived(
    dims === null ? 1 : Math.max((stroke.baseW / QUANT) * longEdge(dims), 1),
  );
</script>

<button
  class="stroke-preview"
  onclick={onflash}
  aria-label="Flash this stroke in Look"
  {@attach tooltip({ text: "Flash this stroke in Look" })}
>
  <img
    src={thumbUrl(hash)}
    alt=""
    draggable="false"
    onload={(e) => {
      const img = e.currentTarget as HTMLImageElement;
      dims = { w: img.naturalWidth, h: img.naturalHeight };
    }}
  />
  {#if dims !== null}
    <svg viewBox="0 0 {dims.w} {dims.h}" preserveAspectRatio="xMidYMid meet">
      <path
        d={path}
        fill="none"
        stroke="var(--red-pencil)"
        stroke-width={width}
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  {/if}
</button>

<style>
  .stroke-preview {
    position: relative;
    display: inline-block;
    width: 72px;
    border: none;
    background: var(--bg-raised);
    border-radius: 3px;
    padding: 0;
    overflow: hidden;
    line-height: 0;
  }
  .stroke-preview img {
    width: 100%;
    height: auto;
    display: block;
  }
  .stroke-preview svg {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }
</style>
