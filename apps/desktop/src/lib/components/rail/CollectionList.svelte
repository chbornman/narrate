<script lang="ts">
  /**
   * Collections tab rows (B71 — the rail's Folders/Collections peer tab,
   * first slice): the SourceList row idiom — quiet full-width buttons —
   * with a member-count badge. Shelved/done collections render
   * status-DIMMED, never hidden: a shelved series is still a series.
   * No context menu seat in this slice; rename/status verbs come with
   * the collection view design round.
   */
  import type { CollectionRow } from "../../logic/sources";

  let {
    rows,
    focusKey,
    currentKey,
    onopen,
  }: {
    rows: CollectionRow[];
    focusKey: string | null;
    currentKey: string | null;
    onopen: (row: CollectionRow) => void;
  } = $props();
</script>

<nav class="collections" aria-label="Collections">
  {#if rows.length === 0}
    <!-- say the next action (the empty-folder line's precedent) -->
    <p class="empty">No collections yet. Gather images into one below.</p>
  {/if}
  {#each rows as row (row.key)}
    <button
      class="row"
      class:focused={row.key === focusKey}
      class:current={row.key === currentKey}
      class:dim={row.dim}
      title={row.dim ? `${row.label} (${row.status})` : row.label}
      onclick={() => onopen(row)}
    >
      <span class="label">{row.label}</span>
      <span class="count">{row.memberCount}</span>
    </button>
  {/each}
</nav>

<style>
  .collections {
    padding: 6px 0 12px;
  }
  .empty {
    margin: 8px 10px;
    color: var(--text-faint);
    font-size: 12px;
    line-height: 1.4;
  }
  .row {
    display: flex;
    width: 100%;
    align-items: center;
    gap: 6px;
    border: none;
    background: transparent;
    color: var(--text-dim);
    text-align: left;
    padding: 4px 10px;
    border-radius: 0;
  }
  .row:hover,
  .row.focused {
    background: var(--bg-raised);
    color: var(--text);
  }
  .row.current {
    color: var(--text);
  }
  .row.dim {
    color: var(--text-faint);
  }
  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    flex: 0 0 auto;
    color: var(--text-faint);
    font-size: 11px;
  }
</style>
