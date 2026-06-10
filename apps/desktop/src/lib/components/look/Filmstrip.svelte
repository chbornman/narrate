<script lang="ts">
  /**
   * The Look filmstrip (UI §4.1, `F`, default hidden) — STAGE B. Stack-
   * aware: each cell shows the entry's DISPLAYED member, so an R flip is
   * visible here too (featureset §5). Obeys Tab via LookSurface's gate;
   * shows the same two badges as the Grid, nothing more.
   */
  import { ui } from "../../state/app.svelte";
  import { thumbUrl } from "../../ipc/urls";
  import { displayedHash } from "../../logic/looknav";

  const stripStart = $derived(Math.max(0, ui.look.index - 8));
  const stripEntries = $derived(ui.look.order.slice(stripStart, stripStart + 17));
  const gridByHash = $derived(new Map(ui.grid.items.map((i) => [i.hash, i])));
</script>

<div class="filmstrip">
  {#each stripEntries as entry, i (entry.display)}
    {@const shown = displayedHash(entry, ui.look.flips)}
    {@const item = gridByHash.get(shown)}
    <button
      class="strip-thumb"
      class:current={stripStart + i === ui.look.index}
      onclick={() => {
        ui.look.index = stripStart + i;
        void ui.reportScope();
      }}
      aria-label="Show image"
    >
      <img src={thumbUrl(shown)} alt="" draggable="false" />
      {#if item?.hasJournal}<span class="journal-dot"></span>{/if}
      {#if item?.offline}<span class="offline-badge">⏏</span>{/if}
    </button>
  {/each}
</div>

<style>
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
    border-color: var(--selection);
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
    background: var(--journal-dot);
  }
  .offline-badge {
    position: absolute;
    right: 3px;
    top: 1px;
    color: var(--text-dim);
    font-size: 9px;
  }
</style>
