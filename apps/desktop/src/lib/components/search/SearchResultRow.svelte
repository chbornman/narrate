<script lang="ts">
  /**
   * One search result (UI §5.1): thumbnail + provenance. The quote is the
   * user's own words, verbatim, match-highlighted — rendered as text runs
   * (never raw HTML through IPC; RETRIEVAL §4). Non-quote provenance renders
   * its honest label (RETRIEVAL §6). No score, signal name, or ranking
   * explanation ever (UI §5.3).
   */
  import { thumbUrl } from "../../ipc/urls";
  import {
    FILTER_ONLY_LABEL,
    provenanceDisplay,
    VISUAL_MATCH_LABEL,
  } from "../../search/render";
  import type { ImageResult } from "../../types/search";

  let {
    result,
    focused,
    selected,
    onopen,
    onselect,
  }: {
    result: ImageResult;
    focused: boolean;
    selected: boolean;
    onopen: () => void;
    onselect: (e: MouseEvent) => void;
  } = $props();

  const display = $derived(provenanceDisplay(result.provenance));
</script>

<div
  class="row"
  class:focused
  class:selected
  role="option"
  aria-selected={selected}
  tabindex="-1"
  onclick={onselect}
  ondblclick={onopen}
  onkeydown={() => {
    /* keyboard is global (registry: search-open-result); rows are not tab stops */
  }}
>
  <img class="thumb" src={thumbUrl(result.preview)} alt="" draggable="false" />
  <div class="prov">
    {#if display.kind === "quote"}
      <p class="quote">
        “{#each display.runs as run, i (i)}{#if run.marked}<mark>{run.text}</mark>{:else}{run.text}{/if}{/each}”
      </p>
      <span class="date">— {display.dateLabel}</span>
    {:else if display.kind === "stroke"}
      <p class="label">marked with the pencil</p>
      <span class="date">— {display.dateLabel}</span>
    {:else if display.kind === "visual-match"}
      <p class="label">{VISUAL_MATCH_LABEL}</p>
    {:else}
      <p class="label">{FILTER_ONLY_LABEL}</p>
    {/if}
  </div>
</div>

<style>
  .row {
    display: flex;
    gap: 14px;
    align-items: center;
    padding: 8px;
    border-radius: 4px;
    border: 1px solid transparent;
  }
  .row.focused {
    background: var(--bg-raised);
  }
  .row.selected {
    border-color: var(--focus);
  }
  .thumb {
    width: 96px;
    height: 96px;
    object-fit: contain;
    background: var(--bg-raised);
    border-radius: 2px;
    flex: 0 0 auto;
  }
  .prov {
    min-width: 0;
  }
  .quote {
    margin: 0;
    color: var(--text);
    user-select: text;
    -webkit-user-select: text;
  }
  .label {
    margin: 0;
    color: var(--text-dim);
    font-style: italic;
  }
  .date {
    color: var(--text-faint);
    font-size: 12px;
  }
</style>
