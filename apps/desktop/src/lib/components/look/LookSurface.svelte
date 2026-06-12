<script lang="ts">
  /**
   * Look (UI §4) — the stage. STAGE B OWNS look/* from parallel kickoff.
   * The Filmstrip LIFTED OUT to the center column's bottom edge panel
   * (App.svelte — founder, June 12 2026: the strip is shared by Grid and
   * Look and spans the canvas width by construction); the M4 stroke-
   * scrubber joins that same bottom region. Right-click on the backdrop
   * opens the look-backdrop seat (Surround ▸ etc. — D6).
   *
   * The bottom panel still PUSHES the stage up; it never overlays it
   * (founder, June 2026: Look's canvas is the one surface where covered
   * pixels matter). The stage's container dims shrink, so the live
   * transform re-derives from the canonical zoom session (zoom.ts
   * carryOver/clampOffsets — U13) and the pencil overlay redraws, all by
   * construction — now on panel DRAGS too, not just the F toggle.
   */
  import { ui } from "../../state/app.svelte";
  import { displayUrl } from "../../ipc/urls";
  import { displayedHash } from "../../logic/looknav";
  import LookStage from "./LookStage.svelte";

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
  <div class="stage-region">
    <LookStage />
  </div>
</div>

<style>
  .look-surface {
    position: absolute;
    inset: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    background: var(--surround); /* D6: the Look backdrop is the surround */
  }
  /* The stage's viewport: shrinks when the filmstrip opens below the
   * surface (the center column's bottom panel pushes, never covers). */
  .stage-region {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
  }
</style>
