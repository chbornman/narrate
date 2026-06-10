<script lang="ts">
  /**
   * The capture indicator (UI §7, DECISIONS I3): bottom-right capsule,
   * ~24 px, always present. M1 states: scope dot + count (`● 3` /
   * `● session`; `● 0` never renders), the 2 px ingest hairline, and the
   * single ~300 ms pulse per committed event. The mic glyph is ABSENT until
   * ASR is ready (§7.3) — silently absent in M1.
   *
   * Toasts: none. M1 has no retraction/redaction surfaces, and nothing else
   * may toast (§7.5) — so no toast component exists in this build.
   */
  import { ui } from "../state/app.svelte";
  import { scopeLabel } from "../logic/scope";
  import { scopeTargets } from "../logic/scope";
  import { thumbUrl } from "../ipc/urls";

  const label = $derived(scopeLabel(ui.scope.kind, ui.scope.count));
  const pct = $derived(
    ui.ingest.total > 0 ? Math.min(100, (100 * ui.ingest.done) / ui.ingest.total) : 0,
  );

  // Hover popover: micro-thumbnails of the scoped images, up to 8 then "+N"
  // (UI §7.2). Sourced from the local selection model (the same lists the
  // scope derives from); the echoed scope stays authoritative for the count.
  const popThumbs = $derived(
    scopeTargets({
      surface: ui.surface,
      searchOpen: ui.searchOpen,
      gridSelection: ui.sel.order,
      searchSelection: ui.searchSel.order,
      lookHash: ui.lookHash,
    }).slice(0, 8),
  );
  const popMore = $derived(Math.max(0, ui.scope.count - 8));

  let pulsing = $state(false);
  let pulseTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (ui.pulseCount === 0) return;
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
  onpointerenter={() => (ui.popoverOpen = true)}
  onpointerleave={() => (ui.popoverOpen = false)}
>
  {#if ui.popoverOpen && ui.scope.kind !== "session" && popThumbs.length > 0}
    <div class="popover">
      {#each popThumbs as h (h)}
        <img src={thumbUrl(h)} alt="" draggable="false" />
      {/each}
      {#if popMore > 0}<span class="more">+{popMore}</span>{/if}
    </div>
  {/if}

  <button class="capsule" onclick={() => ui.summonNote()} aria-label="Write a note">
    {#if ui.ingest.running}
      <span
        class="hairline"
        title="Indexing — {ui.ingest.done.toLocaleString()} of {ui.ingest.total.toLocaleString()}"
      >
        <span class="hairline-fill" style:width="{pct}%"></span>
      </span>
    {/if}
    <span class="dot" class:pulsing>●</span>
    <span class="count">{label}</span>
    <!-- mic glyph absent until ASR is ready (M1: never) -->
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
    gap: 6px;
    height: 24px;
    padding: 0 10px;
    border-radius: 12px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    overflow: hidden;
  }
  .dot {
    color: var(--text-dim);
    font-size: 9px;
    transition: color 120ms ease-out, text-shadow 120ms ease-out;
  }
  .dot.pulsing {
    color: var(--text);
    text-shadow: 0 0 6px var(--text-dim);
  }
  .count {
    color: var(--text-dim);
    font-size: 12px;
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
  .popover {
    position: absolute;
    bottom: 30px;
    right: 0;
    display: flex;
    gap: 4px;
    align-items: center;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 6px;
    padding: 6px;
  }
  .popover img {
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
