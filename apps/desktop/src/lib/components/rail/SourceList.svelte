<script lang="ts">
  /**
   * Generic source-section renderer: folders today; M3's projects and
   * saved searches arrive as sibling SourceSections with ZERO edits here
   * (the M3-proofing is the whole point of this component).
   */
  // Lucide (BACKLOG "Adopt Lucide icons"): stroke chevrons for the folder
  // twist; Unplug for the offline-volume badge (Lucide ships no eject —
  // "disconnected" is the meaning the old ⏏ carried here anyway).
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import Unplug from "@lucide/svelte/icons/unplug";
  import type { SourceRow, SourceSection } from "../../logic/sources";

  let {
    sections,
    focusKey,
    currentKey,
    onopen,
    oncontextmenu,
  }: {
    sections: SourceSection[];
    focusKey: string | null;
    currentKey: string | null;
    onopen: (row: SourceRow) => void;
    oncontextmenu: (row: SourceRow, x: number, y: number) => void;
  } = $props();

  const showHeads = $derived(sections.length > 1);
</script>

<nav class="sources" aria-label="Sources">
  {#each sections as section (section.id)}
    {#if showHeads}<h2>{section.label}</h2>{/if}
    {#each section.rows as row (row.key)}
      <button
        class="row"
        class:focused={row.key === focusKey}
        class:current={row.key === currentKey}
        style:padding-left="{10 + row.depth * 14}px"
        onclick={() => onopen(row)}
        oncontextmenu={(e) => {
          e.preventDefault();
          oncontextmenu(row, e.clientX, e.clientY);
        }}
      >
        {#if row.hasChildren}
          <span class="twist"
            >{#if row.expanded}<ChevronDown size={12} />{:else}<ChevronRight
                size={12}
              />{/if}</span
          >
        {/if}
        <span class="label">{row.label}</span>
        {#if row.offline}<span class="badge" title="Volume offline"><Unplug size={11} /></span>{/if}
      </button>
    {/each}
  {/each}
</nav>

<style>
  .sources {
    padding: 6px 0 12px;
  }
  h2 {
    font-size: 11px;
    font-weight: 600;
    color: var(--text-faint);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin: 8px 10px 2px;
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
  .twist {
    color: var(--text-faint);
    flex: 0 0 auto;
    display: inline-flex; /* svg baseline → flex centering */
    align-items: center;
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    color: var(--text-faint);
    display: inline-flex;
    align-items: center;
  }
</style>
