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
  // Lucide Unplug = the offline-volume badge (BACKLOG "Adopt Lucide
  // icons"; Lucide ships no eject — "disconnected" is the meaning).
  import Unplug from "@lucide/svelte/icons/unplug";
  import { srcHash, thumbUrl } from "../../ipc/urls";
  import { infoLine } from "../../logic/cellinfo";
  import type { CellInfoLevel } from "../../state/grid.svelte";
  import StackChevron from "./StackChevron.svelte";

  let {
    hash,
    previewPing,
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
    /** `previews-changed` ping (grid slice): when `hashes` carries this
     * cell's image, its artifact just landed — reload now. */
    previewPing: { seq: number; hashes: ReadonlySet<string> };
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
  //
  // RECYCLED-IMG GUARD (BACKLOG: pixel flash): the virtualizer recycles
  // this <img> by pool slot, so `hash` changes under a live element whose
  // bitmap (and possibly an in-flight load event) still belongs to the
  // PREVIOUS occupant — setting `src` only queues the swap. The hash
  // keying drops `loaded` (opacity 0) on recycle, but BOTH marking paths
  // must also prove via currentSrc that the element holds THIS hash's
  // bitmap, or they re-mark against stale pixels and the old image
  // flashes for a frame on fast scroll (srcHash doc in ipc/urls.ts).
  const MAX_RETRIES = 30;
  let el: HTMLImageElement | undefined = $state();
  let loadedHash = $state<string | null>(null);
  let retry = $state({ hash: "", n: 0 });
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  /** Last applied previews-changed ping — its seq joins the cache-buster
   * so the reload URL is NOVEL (never a previously-404'd one). */
  let applied = $state({ hash: "", seq: 0 });

  const attempt = $derived(retry.hash === hash ? retry.n : 0);
  const pingSeq = $derived(applied.hash === hash ? applied.seq : 0);
  const loaded = $derived(loadedHash === hash);

  const src = $derived.by(() => {
    const parts = [];
    if (attempt > 0) parts.push(`r=${attempt}`);
    if (pingSeq > 0) parts.push(`p=${pingSeq}`);
    return parts.length === 0 ? thumbUrl(hash) : `${thumbUrl(hash)}?${parts.join("&")}`;
  });

  $effect(() => {
    void hash;
    const img = el;
    // complete + naturalWidth alone are NOT enough: on a recycled slot
    // they still describe the previous occupant until the src swap's
    // microtask runs — only a matching currentSrc proves the bitmap is ours.
    if (img && img.complete && img.naturalWidth > 0 && srcHash(img.currentSrc) === hash) {
      loadedHash = hash;
    }
    return () => clearTimeout(retryTimer);
  });

  // previews-changed: this image's artifact just landed (the backend
  // writes it BEFORE emitting). Reload immediately with a fresh retry
  // budget — a capped 404 loop otherwise blanked the cell until restart
  // (founder dogfood, June 2026).
  $effect(() => {
    const ping = previewPing;
    if (ping.seq === 0 || loaded || !ping.hashes.has(hash)) return;
    clearTimeout(retryTimer);
    retry = { hash, n: 0 };
    applied = { hash, seq: ping.seq };
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
    {src}
    alt=""
    draggable="false"
    loading="eager"
    decoding="async"
    class:loaded
    onload={() => {
      // A load event can arrive for the PREVIOUS occupant's src after the
      // slot was recycled — mark loaded only when the element's bitmap is
      // this hash's (recycled-img guard above).
      if (el !== undefined && srcHash(el.currentSrc) === hash) loadedHash = hash;
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
  {#if offline}<span class="offline-badge"><Unplug size={11} /></span>{/if}
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
    display: flex; /* size the badge box to the svg, no baseline gap */
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
