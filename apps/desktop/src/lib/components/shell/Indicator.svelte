<script lang="ts">
  /**
   * The capture indicator (UI §7), REWRITTEN as a renderer over
   * logic/segments.ts: ingest hairline · scope (the pulse target) ·
   * n-of-m in Look · mode segments (auto-advance; reserved mic seat).
   * Hover = scope popover (micro-thumbnails via the Popover primitive);
   * click = summon the note input.
   *
   * EXEMPT from Tab lights-out (coordinator ruling: the indicator is
   * capture-state truth — modes must stay visible; future mic evidence
   * lives here. Founder sign-off flagged in DECISIONS 5).
   */
  import { ui } from "../../state/app.svelte";
  import { segments } from "../../logic/segments";
  import { scopeTargets } from "../../logic/scope";
  import { thumbUrl } from "../../ipc/urls";
  import Popover from "../../primitives/Popover.svelte";

  let capsuleEl: HTMLButtonElement | undefined = $state();

  const segs = $derived(
    segments({
      ingest: ui.shell.ingest,
      scope: ui.shell.scope,
      lookPosition:
        ui.surface === "look" && ui.look.order.length > 0
          ? { index: ui.look.index, total: ui.look.order.length }
          : null,
      ctx: ui.actionContext(),
    }),
  );
  const hairline = $derived(segs.find((s) => s.id === "ingest"));
  const textSegs = $derived(segs.filter((s) => s.id !== "ingest"));

  // Hover popover: micro-thumbnails of the scoped images, up to 8 then
  // "+N" (UI §7.2). Sourced from the same lists the scope derives from;
  // the echoed scope stays authoritative for the count.
  const popThumbs = $derived(
    scopeTargets({
      surface: ui.surface,
      searchOpen: ui.searchOpen,
      gridSelection: ui.grid.selectionTargets,
      searchSelection: ui.searchSel.order,
      lookTargets: ui.look.currentTargets,
    }).slice(0, 8),
  );
  const popMore = $derived(Math.max(0, ui.shell.scope.count - 8));

  let pulsing = $state(false);
  let pulseTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (ui.shell.pulseCount === 0) return;
    pulsing = false;
    clearTimeout(pulseTimer);
    requestAnimationFrame(() => (pulsing = true));
    pulseTimer = setTimeout(() => (pulsing = false), 320);
  });
</script>

<div
  class="indicator"
  role="status"
  aria-label="Capture indicator"
  onpointerenter={() => (ui.shell.popoverOpen = true)}
  onpointerleave={() => (ui.shell.popoverOpen = false)}
>
  {#if ui.shell.popoverOpen && ui.shell.scope.kind !== "session" && popThumbs.length > 0 && capsuleEl !== undefined}
    <Popover anchor={capsuleEl} placement="top-end" ondismiss={() => (ui.shell.popoverOpen = false)}>
      <div class="pop-thumbs">
        {#each popThumbs as h (h)}
          <img src={thumbUrl(h)} alt="" draggable="false" />
        {/each}
        {#if popMore > 0}<span class="more">+{popMore}</span>{/if}
      </div>
    </Popover>
  {/if}

  <button
    bind:this={capsuleEl}
    class="capsule"
    onclick={() => ui.summonNote()}
    aria-label="Write a note"
  >
    {#if hairline !== undefined}
      <span class="hairline" title={hairline.title}>
        <span
          class="hairline-fill"
          style:width="{Math.round((hairline.fraction ?? 0) * 100)}%"
        ></span>
      </span>
    {/if}
    {#each textSegs as seg (seg.id)}
      <span
        class="segment"
        class:scope={seg.pulse === true}
        class:pulsing={seg.pulse === true && pulsing}
        title={seg.title}>{seg.text}</span
      >
    {/each}
    <!-- mic glyph absent until ASR is ready (P4.2: never) — its seat is
         reserved by segments.ts ordering -->
  </button>
</div>

<style>
  .indicator {
    position: fixed;
    right: 14px;
    bottom: 12px;
    z-index: 50;
  }
  .capsule {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 24px;
    padding: 0 10px;
    border-radius: 12px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    overflow: hidden;
  }
  .segment {
    color: var(--text-dim);
    font-size: 12px;
    white-space: nowrap;
  }
  .segment.scope {
    transition:
      color 120ms ease-out,
      text-shadow 120ms ease-out;
  }
  .segment.scope.pulsing {
    color: var(--text);
    text-shadow: 0 0 6px var(--text-dim);
  }
  .hairline {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 2px;
    background: var(--chrome);
  }
  .hairline-fill {
    display: block;
    height: 2px;
    background: var(--text-dim);
    transition: width 300ms linear;
  }
  .pop-thumbs {
    display: flex;
    gap: 4px;
    align-items: center;
    padding: 6px;
  }
  .pop-thumbs img {
    width: 36px;
    height: 36px;
    object-fit: cover;
    border-radius: 2px;
    background: var(--bg-raised);
  }
  .more {
    color: var(--text-dim);
    font-size: 11px;
    padding: 0 4px;
  }
</style>
