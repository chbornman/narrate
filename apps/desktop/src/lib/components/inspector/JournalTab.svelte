<script lang="ts">
  /**
   * Journal tab — STAGE C OWNS THIS FILE. A vertical timeline from day one
   * (M4's per-image timeline is a rendering upgrade, not a new surface):
   * chronological folded entries under session dividers (newest session
   * first — logic/journal.ts sessionGroups), revision folding with the
   * "edited" affordance, retracted behind the toggle, redacted stubs.
   *
   * THE row-verb routing point: every row verb rides ui.perform — the
   * dispatch case is the ONE place where a retraction also drops the
   * stroke from the Look overlay and the pencil undo stack (CAPTURE §8.5);
   * calling the inspector slice directly would skip that cleanup and let
   * Ctrl+Z target an already-retracted stroke.
   */
  import { ui } from "../../state/app.svelte";
  import type { Action } from "../../logic/keymap";
  import {
    retractedToggleLabel,
    sessionGroups,
    visibleRows,
  } from "../../logic/journal";
  import EmptyState from "../../primitives/EmptyState.svelte";
  import JournalEntry from "./JournalEntry.svelte";

  const groups = $derived(sessionGroups(ui.inspector.entries));

  function route(action: Action) {
    void ui.perform(action);
  }
</script>

<div class="tab-body">
  {#if ui.inspector.entries.length === 0}
    <EmptyState line="Nothing yet." />
  {:else}
    {#each groups as group (group.sessionId)}
      <div class="divider" role="separator">
        <span class="rule"></span>
        <span class="label">Session · {group.dateLabel} · {group.timeRange}</span>
        <span class="rule"></span>
      </div>
      {#each visibleRows(group.rows, ui.inspector.showRetracted) as entry (entry.id)}
        <JournalEntry
          {entry}
          editing={ui.inspector.editingEventId === entry.id}
          onaction={route}
          oncorrect={(eventId, text) => void ui.inspector.commitCorrection(eventId, text)}
        />
      {/each}
      {#if group.retractedCount > 0}
        <button
          class="retracted-toggle"
          onclick={() => void ui.perform({ kind: "journal-toggle-retracted" })}
        >
          [{retractedToggleLabel(group.retractedCount, ui.inspector.showRetracted)}]
        </button>
      {/if}
    {/each}
  {/if}
</div>

<style>
  .tab-body {
    position: relative;
    min-height: 200px;
    padding: 6px 0 12px;
  }
  .divider {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 10px 4px;
  }
  .divider .rule {
    flex: 1;
    height: 1px;
    background: var(--chrome);
  }
  .divider .label {
    color: var(--text-dim);
    font-size: 11px;
    white-space: nowrap;
  }
  .retracted-toggle {
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 11px;
    padding: 2px 10px 2px 18px;
  }
  .retracted-toggle:hover {
    color: var(--text);
  }
</style>
