<script lang="ts">
  /**
   * The Topics rail tab rows (DESIGN-TOPICS-COLLECTIONS.md): the topic SET the
   * graph and this tab share. Two groups, visually distinguished:
   *
   *  - MANUAL topics (ui.topics, saved phrases): full rows like CollectionList,
   *    each removable; selecting one scopes the grid to its ranked images
   *    (ui.openTopic -> the `topic` gridScope -> ranked, descending-affinity).
   *  - SUGGESTED topics (clusterTopics for the current scope, the note-grounded
   *    cluster labels): a lighter "add me" affordance; clicking a suggestion
   *    PROMOTES it to a saved manual topic (ui.addTopic). Already-saved
   *    suggestions are hidden (they live in the manual group).
   *
   * The add-topic input is the rail footer (SourceRail), mirroring the
   * collection creator. Theme-aware (tokens only); no em-dashes (gate).
   */
  import { onMount } from "svelte";
  import { ui } from "../../state/app.svelte";
  import * as ipc from "../../ipc/commands";
  import { collectionKey } from "../../logic/sources";
  import { tooltip } from "../../primitives/tooltip";

  // The current grid scope's note-grounded cluster suggestions (the same
  // clusterTopics command the graph rail uses). Fetched on mount + whenever the
  // scope source changes, so the suggestions track what the grid is showing.
  let clusters = $state<ipc.ClusterTopic[]>([]);
  let loadToken = 0;

  async function loadClusters() {
    const token = ++loadToken;
    try {
      const got = (await ipc.clusterTopics(ui.graphScope())) ?? [];
      if (token === loadToken) clusters = got;
    } catch {
      if (token === loadToken) clusters = []; // degraded rig: no suggestions
    }
  }

  onMount(() => void loadClusters());

  // Re-fetch suggestions when the underlying source changes (a folder/collection
  // switch). untrack-free: reading ui.folderName is a cheap signature for "the
  // source moved" without depending on the whole reactive surface.
  let lastScopeKey = "";
  $effect(() => {
    const key = JSON.stringify(ui.graphScope());
    if (key !== lastScopeKey) {
      lastScopeKey = key;
      void loadClusters();
    }
  });

  // The topic currently scoping the grid (so its row reads as "current").
  const currentPhrase = $derived(
    ui.gridScope.kind === "topic" ? ui.gridScope.phrase : null,
  );

  // Suggestions minus the already-saved manual phrases (a promoted suggestion
  // moves to the manual group, so showing it twice would be noise).
  const savedPhrases = $derived(new Set(ui.topics.map((t) => t.phrase)));
  const freshSuggestions = $derived(clusters.filter((c) => !savedPhrases.has(c.label)));

  // collectionKey is imported only to keep the key-namespace shape obvious; the
  // topic rows use a `topics:` namespace of their own.
  const topicKey = (id: string) => `topics:${id}`;
  void collectionKey;
</script>

<nav class="topics" aria-label="Topics">
  {#if ui.topics.length === 0 && freshSuggestions.length === 0}
    <p class="empty">No topics yet. Add a phrase below, or promote a suggestion.</p>
  {/if}

  <!-- MANUAL topics: saved phrases, selectable + removable. -->
  {#each ui.topics as topic (topic.id)}
    <div
      class="row"
      class:current={topic.phrase === currentPhrase}
      data-key={topicKey(topic.id)}
    >
      <button
        class="open"
        title={topic.phrase}
        onclick={() => void ui.openTopic(topic.phrase, topic.id)}
      >
        <span class="label">{topic.phrase}</span>
      </button>
      <button
        class="remove"
        aria-label={`Remove topic ${topic.phrase}`}
        onclick={() => void ui.removeTopic(topic.id)}
        {@attach tooltip({ text: "Remove this saved topic" })}
      >
        x
      </button>
    </div>
  {/each}

  <!-- SUGGESTED topics: note-grounded cluster labels, a lighter "add me"
       affordance. Clicking promotes the suggestion to a saved manual topic. -->
  {#if freshSuggestions.length > 0}
    <div class="suggest-head">suggested</div>
    {#each freshSuggestions as c (c.label)}
      <button
        class="row suggest"
        title={`Add "${c.label}" (cluster of ${c.size})`}
        onclick={() => void ui.addTopic(c.label)}
      >
        <span class="plus" aria-hidden="true">+</span>
        <span class="label">{c.label}</span>
        <span class="size">{c.size}</span>
      </button>
    {/each}
  {/if}
</nav>

<style>
  .topics {
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
    gap: 4px;
    padding: 0 6px 0 10px;
  }
  .row .open {
    flex: 1;
    border: none;
    background: transparent;
    color: var(--text-dim);
    text-align: left;
    padding: 4px 0;
    border-radius: 0;
    overflow: hidden;
    cursor: pointer;
  }
  .row:hover {
    background: var(--bg-raised);
  }
  .row:hover .open,
  .row.current .open {
    color: var(--text);
  }
  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .remove {
    flex: 0 0 auto;
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
    opacity: 0;
  }
  .row:hover .remove {
    opacity: 1;
  }
  .remove:hover {
    color: var(--text);
    background: var(--bg-overlay);
  }
  .suggest-head {
    margin: 10px 10px 2px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-faint);
  }
  /* Suggested rows: the lighter "add me" affordance. A dashed-feel plus glyph,
     fainter text, so a computed suggestion reads as distinct from a saved
     topic. */
  .row.suggest {
    border: none;
    background: transparent;
    color: var(--text-faint);
    text-align: left;
    cursor: pointer;
    border-radius: 0;
  }
  .row.suggest:hover {
    background: var(--bg-raised);
    color: var(--text);
  }
  .plus {
    flex: 0 0 auto;
    color: var(--chrome-strong);
    font-weight: 700;
    width: 1em;
    text-align: center;
  }
  .size {
    flex: 0 0 auto;
    color: var(--text-faint);
    font-size: 11px;
  }
</style>
