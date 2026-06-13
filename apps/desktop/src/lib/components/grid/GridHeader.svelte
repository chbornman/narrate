<script lang="ts">
  /**
   * Grid header (extracted from the old App.svelte): folder name, the
   * always-visible search bar (M3 search-as-scope), sort ▾ (opens the ONE
   * context-menu machinery), thumb-size slider synced with Ctrl+wheel and
   * -/=. Tooltips resolve from the registry (tooltip.ts).
   *
   * THE BAR (M3): the query is a grid scope, not a destination — the search
   * input lives HERE, always present (the header owns "what is this grid").
   * As-you-type runs the LEXICAL keyword lane (50 ms debounce, the budget
   * floor); Enter commits the SEMANTIC full-hybrid lane. On focus-with-text
   * a thin detail row shows the `within:` residue (the folder/collection the
   * query is scoped over, with one-key clear), the active chips, and a LANE
   * STATUS indicator (M3 Phase 2) that names the live lane — "lexical · Enter
   * for semantic" while typing, "semantic" after a commit, back to lexical on
   * the next edit. The indicator derives from ui.searchLane (the one explicit
   * lane flag) so it never disagrees with the lane that fed the grid. The old
   * SearchOverlay's entry/canvas two-stage model is gone: results render in
   * place as ordinary grid cells.
   */
  // Lucide (BACKLOG "Adopt Lucide icons").
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import Flame from "@lucide/svelte/icons/flame";
  import Search from "@lucide/svelte/icons/search";
  import Settings2 from "@lucide/svelte/icons/settings-2";
  // Topic-graph lens entry (DESIGN-SEMANTIC-GRAPH.md): a network glyph reads as
  // "spatial/related view" — the force-directed map of the current scope.
  import Share2 from "@lucide/svelte/icons/share-2";
  import X from "@lucide/svelte/icons/x";
  import { ui } from "../../state/app.svelte";
  // Search-bar keystroke debounce now lives in the centralized UI tuning
  // module (lib/tuning.ts), the frontend half of the tuning registry.
  import { SEARCH_DEBOUNCE_MS } from "../../tuning";
  import { THUMB_STEPS } from "../../logic/sort";
  import { laneStatus } from "../../logic/searchmode";
  import { tooltip } from "../../primitives/tooltip";
  import type { Filter } from "../../types/search";
  import RankingSignals from "./RankingSignals.svelte";
  import TopicBakeBar from "../topics/TopicBakeBar.svelte";

  // The ⚙ "Ranking signals" popover anchor (Phase 3). The popover itself reads
  // ui.rankingPopoverOpen; this only holds the element it floats from.
  let signalBtn: HTMLButtonElement | undefined = $state();

  let sortBtn: HTMLButtonElement | undefined = $state();
  let inputEl: HTMLInputElement | undefined = $state();
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;

  // `/` and Cmd+F focus the bar by bumping ui.focusBarRequest; this effect
  // re-runs on every bump (even when the value is otherwise unchanged) and
  // pulls DOM focus to the input.
  $effect(() => {
    void ui.focusBarRequest; // tracked dependency
    if (ui.focusBarRequest > 0) inputEl?.focus();
  });

  function openSort() {
    if (ui.shell.contextMenu?.seat === "sort") {
      ui.shell.closeContextMenu();
      return;
    }
    const r = sortBtn?.getBoundingClientRect();
    ui.shell.openContextMenu("sort", r ? { x: r.left, y: r.bottom + 2 } : null);
  }

  // As-you-type: the LEXICAL lane (forced keyword, <100 ms budget). The
  // per-keystroke debounce keeps the grid re-scoping live without a query
  // per keystroke; the backend's interrupt() cancels any in-flight one.
  function onInput() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => void ui.runQueryScope("lexical"), SEARCH_DEBOUNCE_MS);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      // Commit: the SEMANTIC full-hybrid lane (auto-selects relevance sort).
      e.preventDefault();
      clearTimeout(debounceTimer);
      void ui.runQueryScope("semantic");
      return;
    }
    if (e.key === "Backspace" && ui.query === "" && ui.chips.length > 0) {
      // Empty input + chips: drop the last chip and re-scope live (the old
      // overlay's remove-last-chip rule, now owned by the focused input).
      e.preventDefault();
      void ui.perform({ kind: "remove-last-chip" });
    }
    // Escape is NOT handled here: it routes through the global escape ladder
    // (clear-query-scope, then blur-search-bar) so it cannot fight the
    // layer ownership (logic/escape.ts).
  }

  // The detail row appears while the bar is focused AND there is text or chips
  // to refine against — a quiet, queryless header otherwise. It ALSO appears in
  // a `similar` scope (B69 "more like this"): that scope clears the bar input,
  // so its residue ("similar to <name>") is the only thing announcing the view,
  // and it must stay visible without bar focus so the one-key clear is always
  // reachable.
  const showDetail = $derived(
    ui.gridScope.kind === "similar" ||
      ui.gridScope.kind === "topic" ||
      (ui.barFocused && (ui.query.trim().length > 0 || ui.chips.length > 0)),
  );

  /** "More like this" residue copy (B69): the filename the grid is showing
   * neighbors of, captured into the scope at dispatch. Null unless a similar
   * scope is active. No em-dash in user copy (gate: check:emdash). */
  const similarLabel = $derived(
    ui.gridScope.kind === "similar" ? ui.gridScope.filename : null,
  );

  /** Topic-scope residue copy (DESIGN-TOPICS-COLLECTIONS.md): the topic phrase
   * the grid is showing ranked images for. Like the similar residue, it stays
   * visible without bar focus so the one-key clear is always reachable. Null
   * unless a topic scope is active. No em-dash in user copy (gate). */
  const topicLabel = $derived(
    ui.gridScope.kind === "topic" ? ui.gridScope.phrase : null,
  );

  // The lane status indicator (M3 Phase 2): the detail row names the lane the
  // grid is CURRENTLY scoped by. While typing -> "lexical · Enter for semantic";
  // after Enter -> "semantic"; re-typing drops back to lexical. Derived from
  // the ONE explicit lane flag (ui.searchLane) via the pure laneStatus map, so
  // the indicator can never disagree with the lane that fed the grid. Before a
  // scope forms (sub-threshold text, lane "none") the copy is empty and only
  // the standing hint shows — the bar still tells you Enter upgrades.
  const laneHint = $derived(
    laneStatus(ui.searchLane) || "lexical · Enter for semantic",
  );

  /** The folder/collection a query is scoped over (the `within:` residue),
   * or null when no query is active. */
  const withinLabel = $derived.by(() => {
    if (ui.gridScope.kind !== "query") return null;
    const within = ui.gridScope.within;
    if (within.kind === "collection")
      return ui.collections.find((c) => c.id === within.id)?.name ?? "collection";
    // A query is never scoped over another query (queryWithin enforces it),
    // so the remaining case is a folder: the leaf name, or the root's
    // display name at the root.
    if (within.kind !== "folder") return null;
    if (within.folder !== "") return within.folder.split("/").pop() ?? within.folder;
    return ui.roots.find((r) => r.rootId === within.rootId)?.displayName ?? "library";
  });

  function chipLabel(f: Filter): string {
    switch (f.type) {
      case "date":
        return f.relative !== undefined
          ? `date: ${f.relative.season ?? f.relative.unit}`
          : `date: ${f.absolute?.start ?? "…"} - ${f.absolute?.end ?? "…"}`;
      case "camera":
        return `camera: ${f.value}`;
      case "lens":
        return `lens: ${f.value}`;
      case "folder":
        return `folder: ${f.value}`;
      case "root":
        return `root: ${f.value}`;
      case "rating":
        return `rating ${f.op === "gte" ? "≥" : f.op === "lte" ? "≤" : "="} ${f.value}`;
      case "collection":
        return `collection: ${f.name}`;
      case "volume":
        return f.value;
      case "has_strokes":
        return f.value ? "has strokes" : "no strokes";
      case "source":
        return f.values.join("/");
    }
  }
</script>

<header class="grid-header" class:detail={showDetail}>
  <div class="row">
    <span class="folder-name">▍{ui.folderName}</span>

    <div class="bar">
      <span class="glyph" aria-hidden="true"><Search size={14} /></span>
      <input
        bind:this={inputEl}
        bind:value={ui.query}
        oninput={onInput}
        onkeydown={onKeydown}
        onfocus={() => (ui.barFocused = true)}
        onblur={() => (ui.barFocused = false)}
        placeholder="Search"
        spellcheck="false"
        autocomplete="off"
        aria-label="Search"
        data-search-input
      />
    </div>

    <!-- ~ fuzzy quiet-toggle (Phase 4): dim until armed. When on, the
         as-you-type lexical search widens with typo-tolerant camera/lens/
         filename matches, appended BELOW the exact hits. Off by default;
         lexical-lane only, so it never taxes the keystroke budget. No
         em-dashes in the tooltip (gate: check:emdash). -->
    <button
      class="quiet fuzzy"
      class:active={ui.fuzzyMode}
      onclick={() => void ui.setFuzzyMode(!ui.fuzzyMode)}
      aria-label="Fuzzy search"
      aria-pressed={ui.fuzzyMode}
      {@attach tooltip({ text: "Fuzzy search: typo-tolerant camera, lens and filename matching" })}
    >
      ~
    </button>

    <!-- Topic-graph lens (DESIGN-SEMANTIC-GRAPH.md): open the force-directed
         map of the current scope. A quiet glyph next to the ranking controls;
         no em-dashes in the tooltip (gate: check:emdash). -->
    <button
      class="quiet"
      class:active={ui.viewMode === "visualizer"}
      onclick={() => void ui.perform({ kind: "toggle-graph" })}
      aria-label="Visualizer"
      aria-pressed={ui.viewMode === "visualizer"}
      {@attach tooltip({ actionId: "toggle-graph" })}
    >
      <Share2 size={14} />
    </button>

    <!-- ⚙ "Ranking signals" (Phase 3): on-demand, never on the keystroke path.
         Toggling makes the B75 fusion weights visible; semantic-lane only. -->
    <button
      bind:this={signalBtn}
      class="quiet"
      class:active={ui.rankingPopoverOpen}
      onclick={() => void ui.setRankingPopover(!ui.rankingPopoverOpen)}
      aria-label="Ranking signals"
      aria-pressed={ui.rankingPopoverOpen}
      {@attach tooltip({ text: "Ranking signals: see and tune how results are scored" })}
    >
      <Settings2 size={14} />
    </button>

    <!-- Attention heat-tint toggle (DESIGN-ATTENTION-HEATMAP.md): a quiet
         reviewing aid like the histogram, off by default. When on, grid cells
         get a warm glow scaled by engagement intensity. No em-dashes in the
         copy (gate: check:emdash). -->
    <button
      class="quiet heat"
      class:active={ui.heatOn}
      onclick={() => ui.toggleHeat()}
      aria-label="Attention heat"
      aria-pressed={ui.heatOn}
      {@attach tooltip({ actionId: "toggle-heat" })}
    >
      <Flame size={14} />
    </button>
    <!-- "All-time" recency switch (founder decision): shown only while the heat
         tint is on. Off = recency-weighted ("what am I working on now"); on =
         flat all-time ("what mattered most ever"). -->
    {#if ui.heatOn}
      <button
        class="quiet all-time"
        class:active={ui.heatAllTime}
        onclick={() => ui.toggleAllTime()}
        aria-label="All-time attention"
        aria-pressed={ui.heatAllTime}
        {@attach tooltip({ actionId: "toggle-attention-all-time" })}
      >
        {ui.heatAllTime ? "all-time" : "recent"}
      </button>
    {/if}

    <button
      bind:this={sortBtn}
      class="quiet"
      onclick={openSort}
      {@attach tooltip({ actionId: "open-sort-menu" })}
    >
      sort <ChevronDown size={12} />
    </button>
    <input
      class="thumb-slider"
      type="range"
      min="0"
      max={THUMB_STEPS.length - 1}
      step="1"
      value={ui.grid.thumbStep}
      aria-label="Thumbnail size"
      oninput={(e) => ui.grid.setThumbStep(Number(e.currentTarget.value))}
      {@attach tooltip({ actionId: "thumb-size" })}
    />
  </div>

  {#if showDetail}
    <!-- The thin detail row: the `within:` residue (one-key clear) + chips.
         "within:" is the M3 query-residue indicator. -->
    <div class="detail-row">
      {#if similarLabel !== null}
        <!-- "More like this" residue (B69): names the image whose visual
             neighbors fill the grid; one-key clear returns to the source
             (the same clearQueryScope funnel the query residue uses). -->
        <button
          class="residue"
          onclick={() => void ui.clearQueryScope()}
          aria-label="Clear similar, return to source"
        >
          similar to {similarLabel} <X size={11} />
        </button>
      {:else if topicLabel !== null}
        <!-- Topic-scope residue (DESIGN-TOPICS-COLLECTIONS.md): names the topic
             phrase whose ranked images fill the grid; one-key clear returns to
             the source (the same clearQueryScope funnel). -->
        <button
          class="residue"
          onclick={() => void ui.clearQueryScope()}
          aria-label="Clear topic, return to source"
        >
          topic: {topicLabel} <X size={11} />
        </button>
      {:else if withinLabel !== null}
        <button
          class="residue"
          onclick={() => void ui.clearQueryScope()}
          aria-label="Clear search, return to source"
        >
          within: {withinLabel} <X size={11} />
        </button>
      {/if}
      {#each ui.chips as chip, i (i)}
        <span class="chip">
          {chipLabel(chip)}
          <button aria-label="Remove filter" onclick={() => void ui.removeChip(i)}
            ><X size={11} /></button
          >
        </span>
      {/each}
      {#if topicLabel !== null}
        <!-- Topic scope: the bake bar (threshold -> live count -> "Make a
             collection") sits in the detail row, sharing the same pure
             threshold math + bake command as the graph slider. No lane hint
             here (a topic has no lexical/semantic lane). -->
        <TopicBakeBar phrase={topicLabel} scored={ui.topicScored} />
      {:else}
        <!-- The lane status indicator (M3 Phase 2): names the live lane. The
             `committed` class lifts it out of the faint hint tone once a
             semantic commit lands, so "semantic" reads as state, not a prompt. -->
        <span class="lane-hint" class:committed={ui.searchLane === "semantic"}>
          {laneHint}
        </span>
      {/if}
    </div>
  {/if}
</header>

{#if ui.rankingPopoverOpen}
  <!-- The ⚙ popover floats from the gear button (Phase 3). Closed by default;
       its dismiss routes through ui.setRankingPopover so debug + state clean
       up together. Escape is handled by the global ladder (logic/escape.ts). -->
  <RankingSignals anchor={signalBtn ?? null} />
{/if}

<style>
  .grid-header {
    display: flex;
    flex-direction: column;
    justify-content: center;
    height: 30px;
    padding: 0 12px;
  }
  /* The header grows to seat the detail row when the bar is engaged. */
  .grid-header.detail {
    height: 52px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .folder-name {
    color: var(--text-dim);
    white-space: nowrap;
  }
  .bar {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: 460px;
    margin: 0 auto;
    padding: 2px 8px;
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    border-radius: 6px;
  }
  .bar:focus-within {
    border-color: var(--text-faint);
  }
  .glyph {
    color: var(--text-faint);
    display: inline-flex;
  }
  .bar input {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: none;
    padding: 2px 0;
    font-size: 13px;
  }
  .quiet {
    border: none;
    background: transparent;
    color: var(--text-faint);
    display: inline-flex; /* keep the chevron svg on the text's midline */
    align-items: center;
    gap: 2px;
  }
  .quiet:hover {
    color: var(--text);
  }
  /* The ⚙ reads as ON while its popover is open (the tuning is live). */
  .quiet.active {
    color: var(--text);
  }
  /* The ~ glyph: a monospace tilde that sits on the icons' midline. Dim until
     armed (inherits .quiet's faint tone), bright via .active when on. */
  .quiet.fuzzy {
    font-family:
      ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 15px;
    line-height: 1;
    padding: 0 2px;
  }
  .thumb-slider {
    width: 90px;
    accent-color: var(--text-faint);
    background: transparent;
    border: none;
    padding: 0;
  }
  .detail-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 0 0;
    flex-wrap: wrap;
    font-size: 12px;
  }
  .residue {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: transparent;
    border: 1px solid var(--chrome);
    border-radius: 10px;
    padding: 1px 8px;
    color: var(--text-dim);
  }
  .residue:hover {
    color: var(--text);
    border-color: var(--text-faint);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    border-radius: 10px;
    padding: 1px 4px 1px 9px;
    color: var(--text-dim);
  }
  .chip button {
    border: none;
    background: transparent;
    color: var(--text-faint);
    padding: 0 2px;
    display: inline-flex;
    align-items: center;
  }
  .chip button:hover {
    color: var(--text);
  }
  .lane-hint {
    margin-left: auto;
    color: var(--text-faint);
    white-space: nowrap;
  }
  /* A committed semantic lane reads as the grid's current STATE, not a hint —
     lift it to the dim tone so "semantic" is legible at a glance. */
  .lane-hint.committed {
    color: var(--text-dim);
  }
</style>
