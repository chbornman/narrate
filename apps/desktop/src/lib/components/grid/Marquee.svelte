<script lang="ts">
  /**
   * Marquee rubber-band RENDERING only — the math (drag classification,
   * threshold, rect → indices, Ctrl-additive merge) lives in
   * logic/marquee.ts + logic/selection.ts marqueeMerge; pointer wiring is
   * Grid.svelte. Canvas-space rect so mid-drag scroll stays truthful.
   */
  import type { Rect } from "../../logic/marquee";

  let { rect }: { rect: Rect | null } = $props();
</script>

{#if rect !== null}
  <div
    class="marquee"
    style:transform="translate({rect.x}px, {rect.y}px)"
    style:width="{rect.w}px"
    style:height="{rect.h}px"
  ></div>
{/if}

<style>
  .marquee {
    position: absolute;
    top: 0;
    left: 0;
    background: var(--marquee);
    border: 1px solid var(--selection);
    pointer-events: none;
  }
</style>
