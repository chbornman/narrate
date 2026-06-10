<script lang="ts">
  /**
   * Search (UI §5): one input, results as provenance rows, an overlay that
   * remembers its return point (Escape leaves to the invoking surface — I1).
   * Empty query: blank canvas. Zero results: one dimmed line. Quiet.
   *
   * Filter chips render the executed Filter values (RETRIEVAL §4: chips are
   * part of the M1 query input); removing one re-runs the query. The M3
   * parser will ADD chips from natural language — the rendering is already
   * here.
   */
  import { ui } from "../state/app.svelte";
  import { ZERO_RESULTS_LINE } from "../search/render";
  import type { Filter } from "../types/search";
  import SearchResultRow from "./SearchResultRow.svelte";
  import * as sel from "../logic/selection";

  let inputEl: HTMLInputElement | undefined = $state();
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;

  $effect(() => {
    inputEl?.focus();
  });

  function onInput() {
    clearTimeout(debounceTimer);
    // ≤ 50 ms debounce inside the <100 ms budget (UI §5.1).
    debounceTimer = setTimeout(() => void ui.runSearch(), 50);
  }

  function chipLabel(f: Filter): string {
    switch (f.type) {
      case "date":
        return f.relative !== undefined
          ? `date: ${f.relative.season ?? f.relative.unit}`
          : `date: ${f.absolute?.start ?? "…"} – ${f.absolute?.end ?? "…"}`;
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
      case "project":
        return `project: ${f.name}`;
      case "volume":
        return f.value;
      case "has_strokes":
        return f.value ? "has strokes" : "no strokes";
      case "source":
        return f.values.join("/");
    }
  }

  function onRowSelect(idx: number, e: MouseEvent) {
    if (ui.note.open) ui.cancelNote();
    const hashes = ui.resultHashes;
    let next: sel.SelState;
    if (e.shiftKey) next = sel.rangeTo(ui.searchSel, hashes, idx);
    else if (e.metaKey || e.ctrlKey) next = sel.toggle(ui.searchSel, hashes, idx);
    else next = sel.click(ui.searchSel, hashes, idx);
    ui.searchFocus = idx;
    ui.searchSel = next;
    void ui.reportScope();
  }
</script>

<div class="overlay" role="dialog" aria-label="Search">
  <div class="bar">
    <span class="glyph" aria-hidden="true">🔍</span>
    <input
      bind:this={inputEl}
      bind:value={ui.query}
      oninput={onInput}
      placeholder=""
      spellcheck="false"
      autocomplete="off"
      data-search-input
    />
  </div>

  {#if ui.chips.length > 0}
    <div class="chips">
      {#each ui.chips as chip, i (i)}
        <span class="chip">
          {chipLabel(chip)}
          <button aria-label="Remove filter" onclick={() => void ui.removeChip(i)}>×</button>
        </span>
      {/each}
    </div>
  {/if}

  <div class="results" role="listbox" aria-label="Results">
    {#if ui.results !== null}
      {#if ui.results.images.length === 0 && ui.results.session_hits.length === 0}
        <p class="zero">{ZERO_RESULTS_LINE}</p>
      {:else}
        {#each ui.results.images as result, i (result.image_hash)}
          <SearchResultRow
            {result}
            focused={ui.searchFocus === i}
            selected={sel.isSelected(ui.searchSel, result.image_hash)}
            onopen={() => void ui.openLook(result.image_hash, true)}
            onselect={(e) => onRowSelect(i, e)}
          />
        {/each}
        {#if ui.results.session_hits.length > 0}
          <div class="session-hits">
            {#each ui.results.session_hits as hit (hit.quote.event_id)}
              <p class="session-quote">“{hit.quote.text}”</p>
            {/each}
          </div>
        {/if}
      {/if}
    {/if}
    <!-- empty query: blank canvas — no trending, no recents, no tips (§5.2) -->
  </div>
</div>

<style>
  .overlay {
    position: absolute;
    inset: 0;
    background: var(--bg);
    z-index: 20;
    display: flex;
    flex-direction: column;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 18px 24px 6px;
  }
  .glyph {
    color: var(--text-faint);
    font-size: 14px;
  }
  .bar input {
    flex: 1;
    background: transparent;
    border: none;
    border-bottom: 1px solid var(--chrome);
    border-radius: 0;
    font-size: 16px;
    padding: 6px 2px;
  }
  .chips {
    display: flex;
    gap: 6px;
    padding: 6px 24px 0 48px;
    flex-wrap: wrap;
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
    font-size: 12px;
  }
  .chip button {
    border: none;
    background: transparent;
    color: var(--text-faint);
    padding: 0 4px;
  }
  .chip button:hover {
    color: var(--text);
  }
  .results {
    flex: 1;
    overflow-y: auto;
    padding: 12px 24px 60px;
  }
  .zero {
    text-align: center;
    color: var(--text-faint);
    margin-top: 18vh;
  }
  .session-hits {
    margin-top: 18px;
    border-top: 1px solid var(--chrome);
    padding-top: 10px;
  }
  .session-quote {
    color: var(--text-dim);
    user-select: text;
    -webkit-user-select: text;
  }
</style>
