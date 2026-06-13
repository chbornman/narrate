<script lang="ts">
  /**
   * One journal row — STAGE C OWNS THIS FILE. Row states (verbatim remark ·
   * rating · stroke micro-preview (P5.1) · "[redacted]" stub ·
   * retracted struck-through) + the quiet hover actions (UI §8.3):
   * Select (select-from-note — the entry's full target set in the grid) ·
   * Correct (inline revision) · Retract (toast + Undo = RE-STATE, E4) ·
   * Redact… (the one modal). Multi-target entries carry a quiet "+N
   * others" affordance (sibling targets — BACKLOG polish). Availability is
   * pure row logic (logic/journal.ts rowActions/siblingTargetsLabel); the
   * row emits Actions — routing is the parent's (JournalTab). Verbatim
   * user words only, ever (R3).
   */
  // Lucide chevrons for the "edited" fold (BACKLOG "Adopt Lucide icons");
  // the accessible name stays the word "edited" — tests query by it.
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import type { Action } from "../../logic/keymap";
  import {
    formatTime,
    ratingLine,
    rowActions,
    siblingTargetsLabel,
  } from "../../logic/journal";
  import type { JournalEntryDto } from "../../types/dto";
  import StrokePreview from "./StrokePreview.svelte";

  let {
    entry,
    inspectedHash = null,
    pairMateHash = null,
    editing = false,
    onaction,
    oncorrect,
    oncontextmenu,
  }: {
    entry: JournalEntryDto;
    /** The panel's image — "+N others" counts beyond it. */
    inspectedHash?: string | null;
    /** The inspected image's RAW/JPEG pair member (DECISIONS B61): the
     * mate is the same picture, never an "other" — JournalTab resolves
     * it from ui.grid so the row logic stays pure. */
    pairMateHash?: string | null;
    /** ui.inspector.editingEventId === entry.id (escape layer 4 owns it). */
    editing?: boolean;
    onaction: (action: Action) => void;
    /** Commit of the inline correction (empty text just closes). */
    oncorrect: (eventId: string, text: string) => void;
    /** Right-click → the journal-row context-menu seat (parent routes). */
    oncontextmenu?: (entry: JournalEntryDto, x: number, y: number) => void;
  } = $props();

  const time = $derived(formatTime(entry.ts));
  const actions = $derived(rowActions(entry));
  const siblings = $derived(
    siblingTargetsLabel(entry.targets, inspectedHash, pairMateHash),
  );

  function onContextMenu(e: MouseEvent) {
    if (oncontextmenu === undefined) return;
    e.preventDefault();
    oncontextmenu(entry, e.clientX, e.clientY);
  }

  /** "edited" affordance: expand shows the original, dimmed (UI §8.2). */
  let showOriginal = $state(false);

  let draft = $state("");
  $effect(() => {
    // Entering edit seeds the draft with the current FOLDED text.
    if (editing) draft = entry.text ?? "";
  });

  function onEditKeydown(e: KeyboardEvent) {
    // Enter commits; Shift+Enter newlines; Esc reaches escape layer 4
    // through the global handler (text inputs exit first, §0).
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      oncorrect(entry.id, draft);
    }
  }
</script>

<div class="row" class:retracted={entry.retracted} oncontextmenu={onContextMenu} role="presentation">
  <span class="time" title={entry.source}>{time}</span>

  {#if entry.kind === "redacted"}
    <!-- content gone, continuity preserved (EVENTS §3.6, Q2) -->
    <span class="redacted">[redacted]</span>
  {:else if entry.kind === "rating"}
    <span class="text dim">{ratingLine(entry.rating)}</span>
  {:else if entry.kind === "stroke"}
    {#if entry.stroke != null && entry.targets.length > 0}
      <!-- micro-preview (P5.1): the path over a small thumb; clicking
           flashes the stroke on the Look overlay (UI §8.2) -->
      <StrokePreview
        stroke={entry.stroke}
        hash={entry.targets[0]}
        onflash={() =>
          onaction({ kind: "journal-flash-stroke", eventId: entry.id })}
      />
    {:else}
      <span class="text dim">[stroke]</span>
    {/if}
    {#if entry.linkedEvent !== null}
      <!-- the subtle link mark (UI §8.2): drawn while speaking — the
           backend resolves the one-way pointer in BOTH directions -->
      <span class="linked" title="Linked - drawn and spoken together">⇠ linked</span>
    {/if}
  {:else if editing}
    <!-- svelte-ignore a11y_autofocus — the edit is user-summoned -->
    <textarea
      class="edit"
      rows="2"
      autofocus
      bind:value={draft}
      onkeydown={onEditKeydown}
      aria-label="Correct this entry"
    ></textarea>
  {:else}
    <span class="body">
      <span class="text"
        >“{entry.text}”{#if entry.linkedEvent !== null}
          <span class="linked" title="Linked - drawn and spoken together">⇠ linked</span
          >{/if}</span
      >
      {#if entry.corrected}
        <button
          class="edited"
          onclick={() => (showOriginal = !showOriginal)}
          aria-expanded={showOriginal}
        >
          edited {#if showOriginal}<ChevronDown size={11} />{:else}<ChevronRight
              size={11}
            />{/if}
        </button>
      {/if}
      {#if showOriginal && entry.originalText !== null}
        <span class="original">originally: “{entry.originalText}”</span>
      {/if}
    </span>
  {/if}

  {#if siblings !== null}
    <!-- sibling targets (BACKLOG): the event also wrote on other images -->
    <span class="siblings" title="This entry targets other images too - Select picks them all">
      {siblings}
    </span>
  {/if}

  {#if !editing && (actions.select || actions.correct || actions.retract || actions.redact)}
    <span class="actions">
      {#if actions.select}
        <button
          onclick={() =>
            onaction({ kind: "select-journal-targets", targets: entry.targets })}
        >
          Select
        </button>
      {/if}
      {#if actions.correct}
        <button onclick={() => onaction({ kind: "journal-correct", eventId: entry.id })}>
          Correct
        </button>
      {/if}
      {#if actions.retract}
        <button onclick={() => onaction({ kind: "journal-retract", eventId: entry.id })}>
          Retract
        </button>
      {/if}
      {#if actions.redact}
        <button onclick={() => onaction({ kind: "journal-redact", eventId: entry.id })}>
          Redact…
        </button>
      {/if}
    </span>
  {/if}
</div>

<style>
  .row {
    position: relative;
    display: flex;
    gap: 8px;
    align-items: baseline;
    padding: 4px 10px 4px 18px;
  }
  /* The vertical timeline (M4-proofing): a hairline + a dot per row. */
  .row::before {
    content: "";
    position: absolute;
    left: 8px;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--chrome);
  }
  .row::after {
    content: "";
    position: absolute;
    left: 6px;
    top: 11px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--journal-dot);
  }
  .row.retracted {
    text-decoration: line-through;
    opacity: 0.5;
  }
  .time {
    color: var(--text-faint);
    font-size: 11px;
    flex: 0 0 auto;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    /* Take the row's slack: without flex-grow the (space-reserving)
     * right-side spans squeezed the quote to its 1ch overflow-wrap
     * minimum — text rendered vertically (founder dogfood, June 2026). */
    flex: 1 1 auto;
    min-width: 0;
  }
  .text {
    color: var(--text);
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
  }
  .text.dim {
    color: var(--text-dim);
  }
  .redacted {
    color: var(--text-faint);
    font-style: italic;
  }
  /* The subtle link mark (UI §8.2) — quiet, achromatic. */
  .linked {
    color: var(--text-faint);
    font-size: 11px;
    white-space: nowrap;
    margin-left: 6px;
  }
  /* Sibling targets ("+N others") — same quiet register as the link mark. */
  .siblings {
    color: var(--text-faint);
    font-size: 11px;
    white-space: nowrap;
    flex: 0 0 auto;
  }
  .edited {
    align-self: flex-start;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 11px;
    padding: 0;
    display: inline-flex; /* keep the chevron svg on the text's midline */
    align-items: center;
    gap: 2px;
  }
  .original {
    color: var(--text-faint);
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
  }
  .edit {
    flex: 1;
    background: var(--bg-raised);
    border: 1px solid var(--chrome-strong);
    border-radius: 4px;
    color: var(--text);
    font: inherit;
    padding: 4px 6px;
    resize: vertical;
  }
  /* Quiet hover actions (UI §8.3): present, invisible until pointed at.
   * An OVERLAY chip, not a flex sibling — visibility:hidden still
   * reserves layout width, and four verbs + "+N others" in a 320px
   * panel left the quote a 1ch sliver. Overlaying keeps both §8.3
   * promises: zero width when quiet, zero reflow when revealed. */
  .actions {
    position: absolute;
    right: 8px;
    top: 2px;
    display: flex;
    gap: 6px;
    visibility: hidden;
    background: var(--bg-raised);
    border-radius: 3px;
    padding: 2px 4px;
  }
  .row:hover .actions,
  .row:focus-within .actions {
    visibility: visible;
  }
  .actions button {
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 11px;
    padding: 0 2px;
  }
  .actions button:hover {
    color: var(--text);
  }
</style>
