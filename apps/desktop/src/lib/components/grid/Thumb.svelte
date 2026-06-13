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
    previewReady = true,
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
    infoStrip,
    onpointerselect,
    onopen,
    onstacktoggle,
    oncontextmenu,
  }: {
    hash: string;
    /** A thumb artifact exists (GridItem.previewReady): only then is the
     * protocol URL requested at all — mid-scan on a network volume, eager
     * requests are thousands of doomed 404 round-trips (founder, SMB,
     * June 2026). A previews-changed ping flips a not-ready cell live. */
    previewReady?: boolean;
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
    /** The square IMAGE box side (px). */
    size: number;
    /** Fixed info-strip height (px) reserved ABOVE the image; 0 when off.
     * The cell GROWS by this so the image is never overlaid (cellinfo.ts). */
    infoStrip: number;
    onpointerselect: (e: MouseEvent) => void;
    onopen: () => void;
    onstacktoggle: () => void;
    oncontextmenu: (e: MouseEvent) => void;
  } = $props();

  const info = $derived(infoLine(cellInfo, { fileName, rating, hasJournal }));

  // Per-cell shimmer phase: a stable negative animation-delay (0..1 of the
  // cycle) derived from the hash desyncs neighbours so a wall of placeholders
  // never sweeps in lockstep. Pure CSS once applied — no per-frame JS, and the
  // delay is constant per cell so it costs nothing. Negative delay starts the
  // animation already mid-cycle, so there is no initial pause before the sweep.
  const shimmerDelay = $derived.by(() => {
    let h = 0;
    for (let i = 0; i < hash.length; i++) h = (h * 31 + hash.charCodeAt(i)) | 0;
    // 1.6s cycle (see thumb-shimmer); spread the phase across it.
    return `-${((Math.abs(h) % 160) / 100).toFixed(2)}s`;
  });

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
  // Backoff cadence behind MAX_RETRIES: one more second per attempt,
  // capped — together they bound the whole retry budget (~2.5 min)
  // before a cell settles on the placeholder for good.
  const RETRY_BACKOFF_STEP_MS = 1000;
  const RETRY_BACKOFF_MAX_MS = 5000;
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
  /** Request gate: the artifact is known to exist (backend flag), or its
   * previews-changed ping arrived after listing. Until then the <img>
   * never mounts — placeholder only, zero protocol traffic. */
  const ready = $derived(previewReady || pingSeq > 0);

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
      Math.min(RETRY_BACKOFF_STEP_MS * (n + 1), RETRY_BACKOFF_MAX_MS),
    );
  }
</script>

<div
  class="thumb"
  class:selected
  class:active
  style:width="{size}px"
  style:height="{size + infoStrip}px"
  onclick={onpointerselect}
  ondblclick={onopen}
  {oncontextmenu}
  onkeydown={() => {
    /* keyboard selection is global (UI §11); cells are not tab stops */
  }}
  role="gridcell"
  tabindex="-1"
>
  {#if info.name !== null && infoStrip > 0}
    <!-- Info strip at the TOP, IN-FLOW (fixed px): the cell grows downward so
         the image below stays fully visible, never overlaid (founder). -->
    <span class="info" style:height="{infoStrip}px">
      <span class="info-name">{info.name}</span>
      {#if info.state !== null}<span class="info-state">{info.state}</span>{/if}
    </span>
  {/if}
  <!-- The square IMAGE box holds the <img> and all badges, which anchor to
       the image (not the taller cell). -->
  <div class="image" style:height="{size}px">
    {#if !loaded}
      <!-- Building shimmer (BACKLOG "Import progressively" b): a quiet sweep
           over the placeholder while the preview artifact is still being
           built/loaded. Shown for BOTH the pre-ready state (artifact not yet
           known to exist) and the in-flight load, so the card reads as
           "working", not stalled. Removed the instant `loaded` flips, so it
           never animates over a real bitmap. Pure CSS (one GPU transform on a
           ::after band) — hundreds of cells shimmer with no per-frame JS.
           prefers-reduced-motion drops the sweep for a static placeholder. -->
      <div class="shimmer" style:--shimmer-delay={shimmerDelay} aria-hidden="true"></div>
    {/if}
    {#if ready}
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
    {/if}
    <!-- badges anchor to the IMAGE box, top/bottom edges of the thumbnail -->
    {#if hasJournal}<span class="journal-dot"></span>{/if}
    {#if offline}<span class="offline-badge"><Unplug size={11} /></span>{/if}
    {#if stack !== "solo"}
      <!-- the expand/collapse CONTROL, not a badge (featureset §5).
           count: D1 pairs strictly one JPEG with one RAW — always 2. -->
      <StackChevron collapsed={stack === "collapsed"} count={2} onactivate={onstacktoggle} />
    {/if}
  </div>
</div>

<style>
  .thumb {
    /* Column: the info strip stacks ABOVE the square image box, in flow, so
       the cell grows downward and the image is never overlaid (founder). */
    display: flex;
    flex-direction: column;
    background: var(--bg-raised); /* neutral placeholder, no layout shift */
    border: 1px solid transparent;
    border-radius: 2px;
    overflow: hidden;
    cursor: default;
  }
  /* The square IMAGE box: badges anchor here (to the thumbnail), not to the
     taller cell. */
  .image {
    position: relative;
    width: 100%;
    flex: none;
  }
  .image img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
    opacity: 0; /* placeholder until first successful load; no broken icon */
  }
  .image img.loaded {
    opacity: 1;
  }
  /* Building shimmer: the placeholder reads as "working", not stalled, while
     the preview artifact is built/loaded. The layer fills the image box; a
     single lighter band (::after) sweeps across via a GPU transform. Only one
     animated property (translate) so many cells can run at once without jank.
     Achromatic, faint — fits the quiet philosophy. The base is the existing
     --bg-raised placeholder token; the band is a low-alpha light glaze. */
  .shimmer {
    position: absolute;
    inset: 0;
    overflow: hidden;
    background: var(--bg-raised);
    pointer-events: none;
  }
  .shimmer::after {
    content: "";
    position: absolute;
    inset: 0;
    /* faint lighter band; transparent edges so it reads as a soft sweep, not a
       hard bar. A low-alpha light glaze (same translucency idiom as --scrim /
       --marquee) lifts --bg-raised toward --chrome's value without a hard tint,
       so it stays achromatic and quiet across every surround level. */
    background: linear-gradient(
      100deg,
      transparent 30%,
      rgba(255, 255, 255, 0.045) 50%,
      transparent 70%
    );
    /* start fully off the left edge; the keyframe sweeps it across and off */
    transform: translateX(-100%);
    will-change: transform;
    /* negative delay (per-cell, from hash) starts each cell at a different
       point in the cycle so neighbours never sweep in lockstep. */
    animation: thumb-shimmer 1.6s ease-in-out infinite;
    animation-delay: var(--shimmer-delay, 0s);
  }
  @keyframes thumb-shimmer {
    /* hold off-screen for most of the cycle (the "pause"), then one calm pass:
       a long quiet gap keeps a wall of cells from feeling busy or flashy. */
    0%,
    40% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(100%);
    }
  }
  /* Reduced motion (correctness, not optional): no sweep at all. The band
     parks just off-screen so the placeholder is a calm static --bg-raised. */
  @media (prefers-reduced-motion: reduce) {
    .shimmer::after {
      animation: none;
      transform: translateX(-100%);
    }
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
  /* T cell-info strip (logic/cellinfo.ts) — IN-FLOW at the TOP of the cell,
     fixed px, dimmed, pointer-inert. The cell grows by this strip so the
     image below stays fully visible. */
  .info {
    flex: none;
    display: flex;
    flex-direction: column;
    justify-content: center;
    overflow: hidden;
    padding: 1px 5px;
    box-sizing: border-box;
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
