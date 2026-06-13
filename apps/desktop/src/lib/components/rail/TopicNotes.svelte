<script lang="ts">
  /**
   * Topic-notes composer (founder decision: topics get their own append-only
   * note log, mirroring CollectionNotes.svelte). Shown in the rail's Topics
   * view when a saved topic is open, it reads the topic's append-only notes and
   * offers a remark input.
   *
   * A topic note is the user's authored text ABOUT the topic - its definition,
   * what it is for, the refinement intent. Append-only: no edit, no delete (K14:
   * the record preserves the user's own words). This adds a note log keyed to
   * the topic id; it does NOT change how the topic's images/affinity are
   * computed (still always read-time, never stored membership).
   *
   * It owns a LOCAL TopicNotesSlice driven off ui.topicDetailId, so the
   * contended app store stays untouched - the slice loads when the open topic
   * changes (and when the topics list ticks, in case the open topic was
   * affected). WHY it follows the topic ID rather than the scoping phrase: two
   * saved topics may share a phrase pinned to different spaces, and the note log
   * is per-topic-record (the same id the backend FK hangs the note off).
   */
  import { ui } from "../../state/app.svelte";
  import { TopicNotesSlice } from "../../state/topic-notes.svelte";
  import { chronological, formatNoteStamp } from "../../logic/topic-notes";

  const slice = new TopicNotesSlice();

  // Follow the open topic: (re)load when the open topic changes, and re-load
  // when the topics list ticks (a CRUD that may have touched it). An effect,
  // not a derived - loading is an imperative fetch.
  $effect(() => {
    const id = ui.topicDetailId;
    void ui.topics; // dependency: a topics CRUD tick re-pulls the notes
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

{#if ui.topicDetailId !== null}
  <section class="notes" aria-label="Topic notes">
    <div class="heading">About this topic</div>

    <!-- quiet composer: author the topic's intent where it is read -->
    <div class="composer">
      <textarea
        class="compose-input"
        rows="1"
        placeholder="Note this topic's intent..."
        aria-label="Note this topic's intent"
        bind:value={draft}
        onkeydown={onComposeKeydown}
        onfocus={() => (slice.composerFocused = true)}
        onblur={() => (slice.composerFocused = false)}
      ></textarea>
    </div>

    {#if ordered.length === 0}
      <p class="empty">What is this topic for? Note the intent.</p>
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
     topic rows above without shouting (the divider idiom). */
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
  /* Quiet composer (UI §8.2 register): faint until pointed at/focused -
     the CollectionNotes composer, verbatim spacing. */
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
