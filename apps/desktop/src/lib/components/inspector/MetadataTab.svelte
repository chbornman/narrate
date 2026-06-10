<script lang="ts">
  /**
   * Metadata tab — STAGE C OWNS THIS FILE. Read-only EXIF subset rows
   * (K16 stands: NO metadata editing, ever; from the db's EXIF subset, no
   * new parsing); copyable hash/path. Renders inspector.metadata through
   * logic/metadata.ts — the component is a thin table over labeled rows.
   */
  import { ui } from "../../state/app.svelte";
  import { metadataRows } from "../../logic/metadata";
  import EmptyState from "../../primitives/EmptyState.svelte";

  const rows = $derived(
    ui.inspector.metadata === null ? [] : metadataRows(ui.inspector.metadata),
  );

  async function copy(value: string) {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      /* clipboard unavailable (permissions/tests): the value stays selectable */
    }
  }
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
              aria-label="Copy {row.label}"
              onclick={() => void copy(row.value)}
            >
              ⧉
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
  }
  .mrow:hover .copy,
  .copy:focus {
    visibility: visible;
  }
  .copy:hover {
    color: var(--text);
  }
</style>
