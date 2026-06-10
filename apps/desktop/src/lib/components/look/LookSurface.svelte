<script lang="ts">
  /**
   * Look (UI §4) — stage + bottomEdge region. STAGE B OWNS look/* from
   * parallel kickoff. The bottomEdge hosts the Filmstrip now; the M4
   * stroke-scrubber is an ALTERNATE CHILD of the same region (both obey
   * Tab). Right-click on the backdrop opens the look-backdrop seat
   * (Surround ▸ etc. — D6).
   */
  import { ui } from "../../state/app.svelte";
  import { displayUrl } from "../../ipc/urls";
  import { displayedHash } from "../../logic/looknav";
  import LookStage from "./LookStage.svelte";
  import Filmstrip from "./Filmstrip.svelte";

  function onBackdropContextMenu(e: MouseEvent) {
    e.preventDefault();
    ui.shell.openContextMenu("look-backdrop", { x: e.clientX, y: e.clientY });
  }

  // [nice] Preload the ±1 neighbors' display previews (featureset §2): a
  // throwaway Image() primes the photoproof:// HTTP cache so the < 150 ms
  // ←/→ swap budget holds (UI §13).
  $effect(() => {
    const { order, index, flips } = ui.look;
    for (const i of [index - 1, index + 1]) {
      const entry = order[i];
      if (entry === undefined) continue;
      new Image().src = displayUrl(displayedHash(entry, flips));
    }
  });
</script>

<div class="look-surface" oncontextmenu={onBackdropContextMenu} role="presentation">
  <LookStage />
  {#if ui.look.filmstrip && !ui.shell.chromeHidden}
    <Filmstrip />
  {/if}
</div>

<style>
  .look-surface {
    position: absolute;
    inset: 0;
    overflow: hidden;
    background: var(--surround); /* D6: the Look backdrop is the surround */
  }
</style>
