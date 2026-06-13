<script lang="ts">
  /**
   * Generic source-section renderer: folders today; M3's collections and
   * saved searches arrive as sibling SourceSections with ZERO edits here
   * (the M3-proofing is the whole point of this component).
   */
  // Lucide (BACKLOG "Adopt Lucide icons"): stroke chevrons for the folder
  // twist; Unplug for the offline-volume badge (Lucide ships no eject —
  // "disconnected" is the meaning the old eject glyph carried anyway).
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
    ontoggle,
  }: {
    sections: SourceSection[];
    focusKey: string | null;
    currentKey: string | null;
    onopen: (row: SourceRow) => void;
    oncontextmenu: (row: SourceRow, x: number, y: number) => void;
    /** Twist click (deep-tree ergonomics): expand/collapse WITHOUT opening
     * the folder, so a deep branch can be opened by mouse. */
    ontoggle: (row: SourceRow) => void;
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
          <!-- The twist toggles expand/collapse on its own (deep-tree
               ergonomics): a span, not a nested button (invalid HTML), with
               a role so it stays keyboard-reachable. stopPropagation keeps
               the row's open-folder click from also firing. -->
          <span
            class="twist"
            role="button"
            tabindex="-1"
            aria-label={row.expanded ? "Collapse folder" : "Expand folder"}
            onclick={(e) => {
              e.stopPropagation();
              ontoggle(row);
            }}
            onkeydown={(e) => {
              // Keyboard expand is primarily the row's ←/→ (defs/rail.ts);
              // this keeps the twist itself operable if focused.
              if (e.key === "Enter" || e.key === " ") {
                e.stopPropagation();
                e.preventDefault();
                ontoggle(row);
              }
            }}
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
