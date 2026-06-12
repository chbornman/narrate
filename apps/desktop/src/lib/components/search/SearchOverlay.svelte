<script lang="ts">
  /**
   * Search (UI §5; ENTRY PRESENTATION amended by the dogfood-round-2
   * backlog ruling "Search entry as overlay, results as canvas"): `/`
   * floats ONE input over the DIMMED invoking surface — the context stays
   * visible through the scrim, an honest overlay per DECISIONS I1 (search
   * remembers its return point; Escape leaves to the invoking surface).
   * The moment results EXIST they expand to the full canvas: the same
   * contact sheet as before — provenance rows, selection, write scope,
   * and Enter→Look untouched (UI §5 stands; only entry presentation
   * changed).
   *
   * The stage is DERIVED from result state, never stored: an empty/short
   * query (results === null) and ZERO results both keep the floating
   * input — while a query is coming up empty you keep your context to
   * refine against; results arriving is the only thing that earns the
   * canvas. §5.2's "blank canvas" under an empty query becomes the dimmed
   * surface itself — still no trending, no recents, no tips. The
   * zero-result line (§5.2) renders as the single dimmed line inside the
   * floating panel.
   *
   * Filter chips render the executed Filter values (RETRIEVAL §4: chips are
   * part of the M1 query input); removing one re-runs the query. The M3
   * parser will ADD chips from natural language — the rendering is already
   * here.
   */
  // Lucide (BACKLOG "Adopt Lucide icons"): the §5 mockup's magnifier
  // glyph is illustrative, not normative — the bar uses the stroke set.
  import Search from "@lucide/svelte/icons/search";
  import X from "@lucide/svelte/icons/x";
  import { ui } from "../../state/app.svelte";
  import { ZERO_RESULTS_LINE } from "../../search/render";
  import type { Filter } from "../../types/search";
  import SearchResultRow from "./SearchResultRow.svelte";
  import * as sel from "../../logic/selection";

  // Keystroke debounce, sized inside the <100 ms search budget (UI §5.1)
  // — the rest of the budget belongs to the backend round-trip.
  const SEARCH_DEBOUNCE_MS = 50;

  let inputEl: HTMLInputElement | undefined = $state();
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;

  /** entry = floating input over the dimmed surface; canvas = full-bleed
   * contact sheet. Purely derived, so editing a query back to nothing
   * honestly collapses the canvas to the overlay again. */
  const stage: "entry" | "canvas" = $derived(
    ui.results !== null &&
      (ui.results.images.length > 0 || ui.results.session_hits.length > 0)
      ? "canvas"
      : "entry",
  );

  $effect(() => {
    inputEl?.focus();
  });

  function onInput() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => void ui.runSearch(), SEARCH_DEBOUNCE_MS);
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

  function onRowSelect(idx: number, e: MouseEvent) {
    if (ui.shell.note.open) ui.cancelNote();
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

<div class="overlay" data-stage={stage} role="dialog" aria-label="Search">
  {#if stage === "entry"}
    <!-- The dim is purely visual: the invoking surface stays visible
         (dimmed) behind it, but it is NOT a dismissal affordance — UI §5.1
         enumerates Escape as the return path, and Sheet's scrim contract
         (dismissing scrims are ENUMERATED; new ones are spec changes)
         means click-out here would need a DECISIONS/spec ruling first. It
         still swallows pointer events so the layer beneath stays inert
         while search is up. -->
    <div class="dim" role="presentation"></div>
  {/if}

  <!-- The panel (input + chips) is one persistent element across stages so
       the focused input never remounts mid-keystroke as results arrive. -->
  <div class="panel">
    <div class="bar">
      <span class="glyph" aria-hidden="true"><Search size={16} /></span>
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
            <button aria-label="Remove filter" onclick={() => void ui.removeChip(i)}
              ><X size={12} /></button
            >
          </span>
        {/each}
      </div>
    {/if}

    {#if stage === "entry" && ui.results !== null}
      <!-- Zero results: the single dimmed line (§5.2), inside the panel —
           the canvas is never spent on an empty answer. -->
      <p class="zero">{ZERO_RESULTS_LINE}</p>
    {/if}
    <!-- empty query: nothing below the input — the dimmed surface itself is
         the blank canvas (§5.2); no trending, no recents, no tips -->
  </div>

  {#if stage === "canvas"}
    <!-- Results exist → full canvas. The contact sheet is unchanged:
         provenance rows, Grid-like selection as a write scope, Enter→Look
         with result order as the nav set (UI §5.1/§5.3). -->
    <div class="results" role="listbox" aria-label="Results">
      {#each ui.results?.images ?? [] as result, i (result.image_hash)}
        <SearchResultRow
          {result}
          focused={ui.searchFocus === i}
          selected={sel.isSelected(ui.searchSel, result.image_hash)}
          onopen={() => void ui.openLook(result.image_hash, true)}
          onselect={(e) => onRowSelect(i, e)}
        />
      {/each}
      {#if (ui.results?.session_hits.length ?? 0) > 0}
        <div class="session-hits">
          {#each ui.results?.session_hits ?? [] as hit (hit.quote.event_id)}
            <p class="session-quote">“{hit.quote.text}”</p>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .overlay {
    position: absolute;
    inset: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
  }
  /* canvas: the opaque full-surface contact sheet (pre-amendment look) */
  .overlay[data-stage="canvas"] {
    background: var(--bg);
  }
  /* entry: the overlay frame paints nothing itself — the dim layer and the
   * floating panel are its only visible/interactive children, so the
   * surface behind genuinely shows through (honest overlay, I1) */
  .overlay[data-stage="entry"] {
    pointer-events: none;
  }
  .dim {
    position: absolute;
    inset: 0;
    background: var(--scrim);
    pointer-events: auto;
  }
  .panel {
    display: flex;
    flex-direction: column;
  }
  .overlay[data-stage="entry"] .panel {
    pointer-events: auto;
    position: relative; /* paints above the .dim sibling */
    margin: 16vh auto 0;
    width: min(640px, 86vw);
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 8px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.45);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 18px 24px 6px;
  }
  .overlay[data-stage="entry"] .bar {
    padding: 14px 18px 10px;
  }
  .glyph {
    color: var(--text-faint);
    display: inline-flex; /* svg baseline → flex centering */
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
  .overlay[data-stage="entry"] .chips {
    padding: 0 18px 10px 42px;
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
    display: inline-flex; /* center the Lucide X in the chip row */
    align-items: center;
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
    margin: 6px 0 16px;
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
