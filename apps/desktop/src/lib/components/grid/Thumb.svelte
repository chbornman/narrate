<script lang="ts">
  /**
   * One grid cell — STAGE A. Exactly two badges may appear (UI §3.5): the
   * has-journal dot (dulled red, bottom-right) and the offline-volume
   * badge (top-right); both are display-only and HOVER-QUIET (featureset
   * §1: pointer-inert, no hover affordance — the LrC clickable-badge
   * regret). The T cell-info levels add dimmed text via logic/cellinfo.ts.
   * ACTIVE ring is visually DISTINCT from the selected ring (featureset
   * §1 — tokens --focus-cell vs --selection). Single click zone per cell
   * (§8); the StackChevron is the ONE extra control, not a badge.
   *
   * The <img> elements are recycled by the virtualizer (keyed by pool
   * slot, not item) — this component only swaps `src`.
   */
  import { thumbUrl } from "../../ipc/urls";
  import { infoLine } from "../../logic/cellinfo";
  import type { CellInfoLevel } from "../../state/grid.svelte";
  import StackChevron from "./StackChevron.svelte";

  let {
    hash,
    hasJournal,
    offline,
    stack,
    cellInfo,
    fileName,
    rating,
    selected,
    active,
    size,
    onpointerselect,
    onopen,
    onstacktoggle,
    oncontextmenu,
  }: {
    hash: string;
    hasJournal: boolean;
    offline: boolean;
    stack: "solo" | "collapsed" | "expanded";
    cellInfo: CellInfoLevel;
    fileName: string;
    rating: number | null;
    selected: boolean;
    active: boolean;
    size: number;
    onpointerselect: (e: MouseEvent) => void;
    onopen: () => void;
    onstacktoggle: () => void;
    oncontextmenu: (e: MouseEvent) => void;
  } = $props();

  const info = $derived(infoLine(cellInfo, { fileName, rating, hasJournal }));

  // During ingest a thumb's preview may not exist yet (the protocol 404s).
  // Retry with backoff via a cache-busting param; until the first successful
  // load the neutral placeholder shows instead of the webview's broken-image
  // icon. Capped: images whose previews are deferred (e.g. RAW full-decode
  // backfill) settle on the placeholder.
  //
  // Load state is KEYED BY HASH, never reset by effects: cached images can
  // complete before the first effect runs, so an effect-driven reset races
  // onload and permanently hides the img. The complete-check below covers
  // loads that finish before handlers observe them.
  const MAX_RETRIES = 30;
  let el: HTMLImageElement | undefined = $state();
  let loadedHash = $state<string | null>(null);
  let retry = $state({ hash: "", n: 0 });
  let retryTimer: ReturnType<typeof setTimeout> | undefined;

  const attempt = $derived(retry.hash === hash ? retry.n : 0);
  const loaded = $derived(loadedHash === hash);

  $effect(() => {
    void hash;
    const img = el;
    if (img && img.complete && img.naturalWidth > 0) {
      loadedHash = hash;
    }
    return () => clearTimeout(retryTimer);
  });

  function handleError() {
    const n = retry.hash === hash ? retry.n : 0;
    if (n >= MAX_RETRIES) return;
    const forHash = hash;
    clearTimeout(retryTimer);
    retryTimer = setTimeout(
      () => {
        if (hash === forHash) {
          retry = { hash: forHash, n: n + 1 };
        }
      },
      Math.min(1000 * (n + 1), 5000),
    );
  }
</script>

<div
  class="thumb"
  class:selected
  class:active
  style:width="{size}px"
  style:height="{size}px"
  onclick={onpointerselect}
  ondblclick={onopen}
  {oncontextmenu}
  onkeydown={() => {
    /* keyboard selection is global (UI §11); cells are not tab stops */
  }}
  role="gridcell"
  tabindex="-1"
>
  <img
    bind:this={el}
    src={attempt > 0 ? `${thumbUrl(hash)}?r=${attempt}` : thumbUrl(hash)}
    alt=""
    draggable="false"
    loading="eager"
    decoding="async"
    class:loaded
    onload={() => {
      loadedHash = hash;
    }}
    onerror={handleError}
  />
  {#if info.name !== null}
    <span class="info">
      <span class="info-name">{info.name}</span>
      {#if info.state !== null}<span class="info-state">{info.state}</span>{/if}
    </span>
  {/if}
  <!-- badges paint over the info strip so the dot never disappears -->
  {#if hasJournal}<span class="journal-dot"></span>{/if}
  {#if offline}<span class="offline-badge">⏏</span>{/if}
  {#if stack !== "solo"}
    <!-- the expand/collapse CONTROL, not a badge (featureset §5).
         count: D1 pairs strictly one JPEG with one RAW — always 2. -->
    <StackChevron collapsed={stack === "collapsed"} count={2} onactivate={onstacktoggle} />
  {/if}
</div>

<style>
  .thumb {
    position: relative;
    background: var(--bg-raised); /* neutral placeholder, no layout shift */
    border: 1px solid transparent;
    border-radius: 2px;
    overflow: hidden;
    cursor: default;
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
    opacity: 0; /* placeholder until first successful load; no broken icon */
  }
  .thumb img.loaded {
    opacity: 1;
  }
  .thumb.selected {
    border-color: var(--selection);
    transform: translateY(-1px);
  }
  /* Active (focused) ring — distinct from selected (featureset §1). */
  .thumb.active {
    outline: 1px solid var(--focus-cell);
    outline-offset: 1px;
  }
  /* Badges: display-only, hover-quiet — pointer-inert by construction. */
  .journal-dot {
    position: absolute;
    right: 6px;
    bottom: 6px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--journal-dot);
    pointer-events: none;
  }
  .offline-badge {
    position: absolute;
    right: 5px;
    top: 3px;
    color: var(--text-dim);
    font-size: 11px;
    pointer-events: none;
  }
  /* T cell-info strip (logic/cellinfo.ts) — dimmed, pointer-inert. */
  .info {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    flex-direction: column;
    padding: 2px 5px;
    background: var(--bg-overlay);
    opacity: 0.92;
    pointer-events: none;
  }
  .info-name {
    color: var(--text-dim);
    font-size: 10px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .info-state {
    color: var(--text-faint);
    font-size: 10px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
