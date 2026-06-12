<script lang="ts">
  /**
   * Metadata tab — STAGE C OWNS THIS FILE. Read-only EXIF subset rows
   * (K16 stands: NO metadata editing, ever; from the db's EXIF subset, no
   * new parsing); copyable hash/path. Renders inspector.metadata through
   * logic/metadata.ts — the component is a thin table over labeled rows.
   */
  import Check from "@lucide/svelte/icons/check";
  import { ui } from "../../state/app.svelte";
  import { metadataRows } from "../../logic/metadata";
  import { copyFlash, copyKey, copyToClipboard } from "../../primitives/copyflash.svelte";
  import EmptyState from "../../primitives/EmptyState.svelte";

  const rows = $derived(
    ui.inspector.metadata === null ? [] : metadataRows(ui.inspector.metadata),
  );

  // The shared copy register (BACKLOG "Copy actions confirm themselves"):
  // the glyph flashes to a check while copyFlash carries this row's key.
  // A failed write (permissions/tests) flashes nothing — the value stays
  // selectable as the fallback affordance. The key carries the image hash
  // so arrow-advancing to another image inside the flash window cannot
  // light the check next to a value that was never copied.
  const flashKey = (label: string) =>
    copyKey(`metadata:${label}`, ui.inspector.metadata?.hash ?? "");
</script>

<div class="tab-body">
  {#if ui.inspector.metadata === null}
    <EmptyState line="No image." />
  {:else}
    <dl>
      {#each rows as row (row.label)}
        <div class="mrow">
          <dt>{row.label}</dt>
          <dd class:dim={row.dim}>{row.value}</dd>
          {#if row.copyable}
            <button
              class="copy"
              class:copied={copyFlash.key === flashKey(row.label)}
              aria-label="Copy {row.label}"
              onclick={() => void copyToClipboard(flashKey(row.label), row.value)}
            >
              {#if copyFlash.key === flashKey(row.label)}
                <Check size={11} aria-hidden="true" />
              {:else}
                ⧉
              {/if}
            </button>
          {/if}
        </div>
      {/each}
    </dl>
  {/if}
</div>

<style>
  .tab-body {
    position: relative;
    min-height: 200px;
  }
  dl {
    margin: 0;
    padding: 8px 0;
  }
  .mrow {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 3px 10px;
  }
  dt {
    flex: 0 0 86px;
    color: var(--text-faint);
    font-size: 11px;
  }
  dd {
    margin: 0;
    min-width: 0;
    color: var(--text);
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
  }
  dd.dim {
    color: var(--text-faint);
  }
  .copy {
    margin-left: auto;
    flex: 0 0 auto;
    border: none;
    background: transparent;
    color: var(--text-faint);
    padding: 0 2px;
    visibility: hidden;
    /* Lucide check is an <svg>: flex-center it alongside the text glyph. */
    display: inline-flex;
    align-items: center;
  }
  .mrow:hover .copy,
  .copy:focus,
  .copy.copied {
    /* `.copied` keeps the confirmation visible even if the pointer has
     * already left the row — the flash must not depend on hover. */
    visibility: visible;
  }
  .copy:hover,
  .copy.copied {
    color: var(--text);
  }
</style>
