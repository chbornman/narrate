<script lang="ts">
  /**
   * The shared threshold -> count -> bake control (DESIGN-TOPICS-COLLECTIONS.md).
   * ONE small component the Topics-tab ranked grid and (logically) the graph
   * slider both reduce to: a 0..1 threshold slider, a live "N photos" count of
   * the images at or above it, and a "Make a collection" action that commits
   * that selection into an evented collection via ui.bakeTopicCollection (the
   * ONE-WAY bake; provenance recorded by the backend).
   *
   * The threshold -> selected-set math is the pure logic/topicbake module, so the
   * count here and the graph's glow set and the bake's membership agree exactly.
   * Theme-aware (tokens only). No em-dashes in user-visible copy (gate).
   */
  import { ui } from "../../state/app.svelte";
  import {
    countAboveThreshold,
    DEFAULT_BAKE_THRESHOLD,
    type ScoredImage,
  } from "../../logic/topicbake";

  let {
    phrase,
    scored,
  }: {
    /** The topic phrase being baked (the bake command keys off it + the threshold). */
    phrase: string;
    /** The ranked affinity scores to threshold over (hash + score, descending).
     * The Topics tab passes ui.topicScored (the scores the grid was fed). */
    scored: readonly ScoredImage[];
  } = $props();

  // The live threshold. Starts at the design default (the strongly-on-topic
  // core); the user drags it to widen or tighten the selection.
  let threshold = $state(DEFAULT_BAKE_THRESHOLD);

  // The live count the readout shows ("142 photos") -- the pure threshold math,
  // so it agrees with the graph glow and the eventual bake membership.
  const count = $derived(countAboveThreshold(scored, threshold));

  // Naming the bake: a one-shot inline input defaulting to the phrase. Closed
  // until "Make a collection" is pressed (the rail's create idiom).
  let naming = $state(false);
  let name = $state("");
  let baking = $state(false);

  function beginBake() {
    name = phrase; // default the collection name to the topic phrase
    naming = true;
  }

  async function commitBake() {
    if (baking) return;
    baking = true;
    try {
      // The ONE-WAY bake: members are the in-scope images scoring >= threshold;
      // the backend records phrase + threshold + alpha as provenance and returns
      // an ordinary independent collection. ui surfaces it (snapshot refresh ->
      // the rail shows it) and flips to the Collections tab so it is visible.
      const created = await ui.bakeTopicCollection(phrase, threshold, name);
      naming = false;
      if (created !== null) ui.shell.stationPop(`Collection - ${created.name}`);
    } finally {
      baking = false;
    }
  }

  function onNameKeydown(e: KeyboardEvent) {
    e.stopPropagation(); // the input owns its keys (the rail-creator pattern)
    if (e.key === "Enter") void commitBake();
    else if (e.key === "Escape") naming = false;
  }
</script>

<div class="bake" aria-label="Bake topic into a collection">
  <label class="slider">
    <span class="lbl">threshold</span>
    <input
      type="range"
      min="0"
      max="1"
      step="0.01"
      bind:value={threshold}
      aria-label="Affinity threshold"
    />
    <span class="val">{threshold.toFixed(2)}</span>
  </label>
  <span class="count">{count.toLocaleString()} {count === 1 ? "photo" : "photos"}</span>
  {#if naming}
    <input
      class="name-input"
      bind:value={name}
      placeholder="Collection name"
      aria-label="New collection name"
      onkeydown={onNameKeydown}
    />
    <button class="go" disabled={baking || name.trim() === ""} onclick={() => void commitBake()}>
      {baking ? "Baking..." : "Bake"}
    </button>
  {:else}
    <button class="make" disabled={count === 0} onclick={beginBake}>Make a collection</button>
  {/if}
</div>

<style>
  .bake {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    font-size: 12px;
    color: var(--text-dim);
  }
  .slider {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .slider .lbl {
    color: var(--text-faint);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
  }
  .slider input[type="range"] {
    width: 110px;
  }
  .val {
    color: var(--text);
    font-variant-numeric: tabular-nums;
    min-width: 2.4em;
  }
  .count {
    color: var(--text);
    font-weight: 600;
  }
  .name-input {
    background: var(--bg-raised);
    border: 1px solid var(--chrome-strong);
    border-radius: 4px;
    color: var(--text);
    padding: 3px 8px;
    font-size: 12px;
  }
  button {
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    color: var(--text);
    padding: 4px 10px;
    border-radius: 4px;
    cursor: pointer;
    font-size: 12px;
  }
  button:hover:not(:disabled) {
    border-color: var(--chrome-strong);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
