<script lang="ts">
  /**
   * Collection-notes composer (B71 — the UI slice; the P7.3 store, merge
   * rules, and commands already landed). The sibling of the inspector's
   * journal composer (inspector/JournalTab.svelte), one register quieter:
   * shown in the rail's Collections view when a collection is open, it reads
   * the grouping's append-only notes and offers a remark input.
   *
   * A collection note is about WHY these images are together — the
   * grouping's intent — a deliberately separate kind from per-image journal
   * events (K14: the record preserves the user's own words, here for the
   * collection rather than a single frame). Append-only: no edit, no delete.
   *
   * It owns a LOCAL CollectionNotesSlice driven off ui.collectionId, so the
   * contended app store stays untouched — the slice loads on open and on the
   * collections-changed snapshot (which is when noteCount may have moved).
   */
  import { ui } from "../../state/app.svelte";
  import { CollectionNotesSlice } from "../../state/collection-notes.svelte";
  import { chronological, formatNoteStamp } from "../../logic/collection-notes";

  const slice = new CollectionNotesSlice();

  // Follow the viewed collection: (re)load when the open collection changes,
  // and re-load when its snapshot ticks (a note appended elsewhere, or the
  // count moved). An effect, not a derived — loading is an imperative fetch.
  // Reading collections here is what re-runs us on collections-changed.
  $effect(() => {
    const id = ui.collectionId;
    void ui.collections; // dependency: the snapshot tick re-pulls the notes
    void slice.load(id);
  });

  const ordered = $derived(chronological(slice.notes));

  let draft = $state("");

  async function onComposeKeydown(e: KeyboardEvent) {
    // Enter commits; Shift+Enter newlines. Keys stay in the input (the
    // rail-create containment precedent) so the grid keymap never sees them.
    e.stopPropagation();
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (await slice.compose(draft)) draft = "";
    }
  }
</script>

{#if ui.collectionId !== null}
  <section class="notes" aria-label="Collection notes">
    <div class="heading">About this collection</div>

    <!-- quiet composer: author the grouping's intent where it is read -->
    <div class="composer">
      <textarea
        class="compose-input"
        rows="1"
        placeholder="Note the collection's intent…"
        aria-label="Note the collection's intent"
        bind:value={draft}
        onkeydown={onComposeKeydown}
        onfocus={() => (slice.composerFocused = true)}
        onblur={() => (slice.composerFocused = false)}
      ></textarea>
    </div>

    {#if ordered.length === 0}
      <p class="empty">Why are these together? Note the intent.</p>
    {:else}
      <ol class="list">
        {#each ordered as note (note.id)}
          <li class="note">
            <span class="stamp">{formatNoteStamp(note.ts)}</span>
            <span class="text">{note.text}</span>
          </li>
        {/each}
      </ol>
    {/if}
  </section>
{/if}

<style>
  /* The rail's quietest register: a faint top rule sets it apart from the
     collection rows above without shouting (the divider idiom). */
  .notes {
    flex: 0 0 auto;
    border-top: 1px solid var(--chrome);
    padding: 8px 0 4px;
    max-height: 40%;
    overflow-y: auto;
  }
  .heading {
    color: var(--text-dim);
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    padding: 0 10px 4px 18px;
  }
  /* Quiet composer (UI §8.2 register): faint until pointed at/focused —
     the JournalTab composer, verbatim spacing. */
  .composer {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 10px 6px 18px;
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
  .empty {
    margin: 2px 10px 4px 18px;
    color: var(--text-faint);
    font-size: 12px;
    line-height: 1.4;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 2px 10px 4px 18px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .note {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .stamp {
    color: var(--text-faint);
    font-size: 11px;
  }
  .text {
    color: var(--text);
    font-size: 12px;
    line-height: 1.4;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
