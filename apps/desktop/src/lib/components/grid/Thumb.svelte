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
  import { microUrl, srcHash, thumbUrl, tierForCell } from "../../ipc/urls";
  import { infoLine } from "../../logic/cellinfo";
  import { signalHint } from "../../logic/ranking";
  import type { CellInfoLevel } from "../../state/grid.svelte";
  import { ui } from "../../state/app.svelte";
  import StackChevron from "./StackChevron.svelte";

  let {
    hash,
    highPriority = true,
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
    intensity = 0,
    measurementGeneration = 0,
    onpreviewload,
    onpointerselect,
    onopen,
    onstacktoggle,
    oncontextmenu,
  }: {
    hash: string;
    /** True only when the cell intersects the real viewport. Mounted
     * one-screen overscan is useful rendering runway, but its image fetch is
     * speculative and must not compete with pixels the user can see. */
    highPriority?: boolean;
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
    /** Normalized engagement intensity 0..1 (DESIGN-ATTENTION-HEATMAP.md):
     * when the heat tint is on, the cell shows a warm glow + corner heat-bar
     * scaled by this. 0 (the default, and the tint-off case) renders nothing,
     * so the shimmer/badges are undisturbed. */
    intensity?: number;
    /** Monotone viewport-measurement generation. A new value makes an
     * already-decoded recycled image report readiness to the current journey. */
    measurementGeneration?: number;
    onpreviewload?: (hash: string, generation: number) => void;
    onpointerselect: (e: MouseEvent) => void;
    onopen: () => void;
    onstacktoggle: () => void;
    oncontextmenu: (e: MouseEvent) => void;
  } = $props();

  const info = $derived(infoLine(cellInfo, { fileName, rating, hasJournal }));

  // Heat-tint (DESIGN-ATTENTION-HEATMAP.md): a warm glow + a corner heat-bar
  // scaled by normalized engagement intensity. Clamp 0..1 defensively; a tiny
  // floor keeps a barely-warm cell from rendering a sub-pixel sliver. Off-tint
  // (and cold cells) pass 0, so nothing renders and the quiet grid stands.
  const heat = $derived(Math.max(0, Math.min(1, intensity)));
  const showHeat = $derived(heat > 0.01);

  // Per-signal provenance hint (search-as-scope Phase 3, "show, don't just
  // tune"): ONLY while the ⚙ "Ranking signals" popover is open, name which
  // fusion signals voted for THIS image (their short codes, e.g. "S1 S2 S4").
  // Quiet and tuning-scoped: the resultDebug map is empty unless the popover
  // asked for debug, so this is "" (and renders nothing) in the common case.
  const signals = $derived.by(() => {
    if (!ui.rankingPopoverOpen) return "";
    const dbg = ui.resultDebug.get(hash);
    return dbg === undefined ? "" : signalHint(dbg.per_signal);
  });

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
  /** Micro-tier miss fallback (keyed by hash like `retry`): `previewReady`
   * only promises the THUMB artifact exists — the micro tier is a
   * generator_version-3 regen that may not have run yet — so a micro 404
   * must not burn the retry budget waiting on a regen that may never come.
   * The first micro error drops this hash to the thumb tier immediately
   * (the graph loader's documented fallback, ipc/urls.ts); a
   * previews-changed ping clears it so a freshly-written micro is used. */
  let microMiss = $state({ hash: "" });

  const attempt = $derived(retry.hash === hash ? retry.n : 0);
  const pingSeq = $derived(applied.hash === hash ? applied.seq : 0);
  const loaded = $derived(loadedHash === hash);
  /** Request gate: the artifact is known to exist (backend flag), or its
   * previews-changed ping arrived after listing. Until then the <img>
   * never mounts — placeholder only, zero protocol traffic. */
  const ready = $derived(previewReady || pingSeq > 0);

  /** Decode tier by DISPLAY size (AUDIT F3, ipc/urls.ts tierForCell):
   * micro (96 px) for the two smallest zoom steps, thumb (512 px) above.
   * Safe on a zoom-step tier flip for a loaded cell: the URL keeps the
   * same hash, so `loaded` (and the srcHash guards below) stay true and
   * the old tier's bitmap keeps painting until the new src decodes — no
   * flash of empty at the boundary. */
  const useMicro = $derived(tierForCell(size) === "micro" && microMiss.hash !== hash);

  const src = $derived.by(() => {
    const base = useMicro ? microUrl(hash) : thumbUrl(hash);
    const parts = [];
    if (attempt > 0) parts.push(`r=${attempt}`);
    if (pingSeq > 0) parts.push(`p=${pingSeq}`);
    return parts.length === 0 ? base : `${base}?${parts.join("&")}`;
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

  // Report decoded pixels, including an image that was already warm when a
  // new viewport journey began. The generation lets Grid reject late recycled
  // slot events without persisting hashes in the metrics sink.
  $effect(() => {
    const generation = measurementGeneration;
    if (generation > 0 && highPriority && loaded && onpreviewload !== undefined) {
      onpreviewload(hash, generation);
    }
  });

  // previews-changed: this image's artifact just landed (the backend
  // writes it BEFORE emitting). Reload immediately with a fresh retry
  // budget — a capped 404 loop otherwise blanked the cell until restart
  // (founder dogfood, June 2026).
  //
  // An EMPTY `hashes` set is the GLOBAL signal the manual cache clear emits:
  // it applies to EVERY thumb, even one already `loaded`. WHY ignore `loaded`
  // here: the protocol serves content-addressed URLs with an `immutable`
  // cache header, so after a clear deletes the bytes the webview keeps
  // painting its cached copy for the same stable URL — only a NOVEL URL
  // (a bumped `?p=` seq) forces a re-request. Bumping `applied.seq` rewrites
  // the src so the grid re-fetches: a 1:1-only clear re-validates (thumbs
  // survive), a full clear lands a truthful "?" that then heals per hash as
  // the re-pended preview pass regenerates each artifact.
  $effect(() => {
    const ping = previewPing;
    if (ping.seq === 0) return;
    const global = ping.hashes.size === 0;
    if (!global && (loaded || !ping.hashes.has(hash))) return;
    clearTimeout(retryTimer);
    retry = { hash, n: 0 };
    // The regen that just landed may have written the micro artifact —
    // retry the size-appropriate tier, not the sticky thumb fallback.
    if (microMiss.hash === hash) microMiss = { hash: "" };
    applied = { hash, seq: ping.seq };
  });

  function handleError() {
    // Micro tier 404: fall back to the guaranteed thumb tier at once (see
    // microMiss above) — the retry budget stays for genuine thumb misses.
    if (useMicro) {
      microMiss = { hash };
      return;
    }
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
        loading={highPriority ? "eager" : "lazy"}
        fetchpriority={highPriority ? "high" : "low"}
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
    <!-- Signal-provenance hint (Phase 3): only while the ⚙ popover tunes, a
         quiet per-cell breakdown of which signals contributed to this match. -->
    {#if signals !== ""}<span class="signal-hint">{signals}</span>{/if}
    <!-- Heat-tint (DESIGN-ATTENTION-HEATMAP.md): a warm glow over the image
         plus a left-edge corner heat-bar, both scaled by intensity. Pointer-
         inert and below the badges so it never blocks a click or hides a dot. -->
    {#if showHeat}
      <span class="heat-glow" style:opacity={heat} aria-hidden="true"></span>
      <span class="heat-bar" style:--heat={heat} aria-hidden="true"></span>
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
  /* Heat-tint (DESIGN-ATTENTION-HEATMAP.md): a warm radial glow scaled by
     intensity (the inline opacity), warmest at the corner where the bar sits.
     Pointer-inert, achromatic-warm so it reads as "hot" without a hard tint. */
  .heat-glow {
    position: absolute;
    inset: 0;
    pointer-events: none;
    background: radial-gradient(
      circle at 8% 92%,
      rgba(255, 120, 40, 0.55) 0%,
      rgba(255, 140, 60, 0.22) 38%,
      transparent 70%
    );
    mix-blend-mode: screen;
  }
  /* Corner heat-bar: a short vertical wedge at the bottom-left whose HEIGHT
     scales with intensity (--heat, 0..1). A discrete read of the glow's
     analog warmth — the LrC-style flag without a clickable badge. */
  .heat-bar {
    position: absolute;
    left: 4px;
    bottom: 4px;
    width: 3px;
    height: calc(6px + var(--heat, 0) * 60%);
    max-height: calc(100% - 8px);
    border-radius: 2px;
    background: linear-gradient(
      to top,
      rgba(255, 90, 30, 0.95),
      rgba(255, 170, 70, 0.85)
    );
    pointer-events: none;
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
  /* Phase 3 signal-provenance hint: a quiet bottom-left pill, only while the
     ⚙ popover tunes — never a standing badge (UI §3.5 keeps cells quiet). */
  .signal-hint {
    position: absolute;
    left: 4px;
    bottom: 4px;
    font-size: 9px;
    letter-spacing: 0.04em;
    line-height: 1;
    padding: 2px 4px;
    border-radius: 3px;
    background: var(--bg-overlay);
    color: var(--text-faint);
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
