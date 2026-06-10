<script lang="ts">
  /**
   * The Grid surface: header + virtualized grid (which hosts the marquee
   * in canvas space), plus the Ctrl+wheel size wiring — the SAME
   * thumb-size action the slider and -/= dispatch (featureset §1), so the
   * three inputs can never drift. Wheel deltas accumulate to one step per
   * notch so trackpad pixel-deltas don't sprint through the size range.
   */
  import { ui } from "../../state/app.svelte";
  import GridHeader from "./GridHeader.svelte";
  import Grid from "./Grid.svelte";

  /** One size step per accumulated notch (mouse notch ≈ 100–120,
   * trackpads stream smaller deltas). */
  const WHEEL_NOTCH = 50;
  let wheelAcc = 0;

  function onWheel(e: WheelEvent) {
    if (!e.ctrlKey && !e.metaKey) return; // plain wheel only scrolls
    e.preventDefault();
    if (Math.sign(e.deltaY) !== Math.sign(wheelAcc)) wheelAcc = 0; // direction flip
    wheelAcc += e.deltaY;
    if (Math.abs(wheelAcc) < WHEEL_NOTCH) return;
    const delta = wheelAcc < 0 ? 1 : -1;
    wheelAcc = 0;
    void ui.perform({ kind: "thumb-size", delta });
  }
</script>

<div class="grid-surface" onwheel={onWheel} role="presentation">
  {#if !ui.shell.chromeHidden}
    <GridHeader />
  {/if}
  <div class="grid-area" class:full={ui.shell.chromeHidden}>
    <Grid />
  </div>
</div>

<style>
  .grid-surface {
    position: absolute;
    inset: 0;
  }
  .grid-area {
    position: absolute;
    top: 30px;
    left: 0;
    right: 0;
    bottom: 0;
  }
  .grid-area.full {
    top: 0;
  }
</style>
