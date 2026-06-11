<script lang="ts">
  /**
   * Journal tab — STAGE C OWNS THIS FILE. A vertical timeline from day one
   * (M4's per-image timeline is a rendering upgrade, not a new surface):
   * chronological folded entries under session dividers (newest session
   * first — logic/journal.ts sessionGroups), revision folding with the
   * "edited" affordance, retracted behind the toggle, redacted stubs.
   *
   * The quiet inline COMPOSER (BACKLOG: compose entries from the journal
   * panel) sits above the timeline: a remark input (Enter commits,
   * Shift+Enter newlines, Esc exits focus first via escape layer 5) and a
   * small 0–5 rating row — both bound to the PANEL's image as a single
   * explicit target (the deliberate CAPTURE §4 panel-context variant; the
   * N transient keeps the submit-time-scope rule and its role).
   *
   * THE row-verb routing point: every row verb rides ui.perform — the
   * dispatch case is the ONE place where a retraction also drops the
   * stroke from the Look overlay and the pencil undo stack (CAPTURE §8.5);
   * calling the inspector slice directly would skip that cleanup and let
   * Ctrl+Z target an already-retracted stroke.
   */
  import { ui } from "../../state/app.svelte";
  import type { Action } from "../../logic/keymap";
  import type { JournalEntryDto } from "../../types/dto";
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

  /** Right-click on a row → the journal-row seat; the seat arg is the
   * entry's ordered target list (actions/menus.ts renders the model). */
  function onRowContextMenu(entry: JournalEntryDto, x: number, y: number) {
    ui.shell.openContextMenu("journal-row", { x, y }, entry.targets);
  }

  // ---- composer (panel-local like the row verbs; pointer + Enter only) ----

  let composeDraft = $state("");

  async function onComposeKeydown(e: KeyboardEvent) {
    // Enter commits; Shift+Enter newlines; Esc reaches escape layer 5
    // through the global handler (text inputs exit first, §0).
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (await ui.composeRemark(composeDraft)) composeDraft = "";
    }
  }
</script>

<div class="tab-body">
  {#if ui.inspector.hash !== null}
    <!-- quiet composer: author entries where they are read (founder r2) -->
    <div class="composer">
      <textarea
        class="compose-input"
        rows="1"
        placeholder="Add a remark…"
        aria-label="Add a remark"
        bind:value={composeDraft}
        onkeydown={onComposeKeydown}
        onfocus={() => (ui.inspector.composerFocused = true)}
        onblur={() => (ui.inspector.composerFocused = false)}
      ></textarea>
      <span class="compose-rate" role="group" aria-label="Rate this image">
        {#each [0, 1, 2, 3, 4, 5] as v (v)}
          <button
            title={v === 0 ? "Clear rating" : `Rate ${v}`}
            onclick={() => void ui.composeRating(v)}
          >
            {v}
          </button>
        {/each}
      </span>
    </div>
  {/if}
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
          inspectedHash={ui.inspector.hash}
          editing={ui.inspector.editingEventId === entry.id}
          onaction={route}
          oncorrect={(eventId, text) => void ui.inspector.commitCorrection(eventId, text)}
          oncontextmenu={onRowContextMenu}
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
  /* Quiet composer (UI §8.2 register): faint until pointed at/focused. */
  .composer {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px 8px 18px;
  }
  .compose-input {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: 1px solid var(--chrome);
    border-radius: 4px;
    color: var(--text);
    font: inherit;
    padding: 3px 6px;
    resize: none;
  }
  .compose-input::placeholder {
    color: var(--text-faint);
  }
  .compose-input:hover,
  .compose-input:focus {
    border-color: var(--chrome-strong);
    background: var(--bg-raised);
    outline: none;
  }
  .compose-rate {
    flex: 0 0 auto;
    display: flex;
    gap: 2px;
  }
  .compose-rate button {
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 11px;
    padding: 1px 3px;
    border-radius: 3px;
  }
  .compose-rate button:hover {
    color: var(--text);
    background: var(--bg-raised);
  }
</style>
