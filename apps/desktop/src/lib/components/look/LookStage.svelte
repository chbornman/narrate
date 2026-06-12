<script lang="ts">
  /**
   * The Look stage: image + THE transform (logic/zoom.ts) — STAGE B.
   * The slice's zoomSession ({mode, scale, centerFrac}) is canonical; the
   * live transform DERIVES from it against this image's dims, so zoom
   * persistence across ←/→ (featureset §2) and re-anchoring on panel
   * resize are carryOver by construction — raw tx/ty never survive a
   * dimension change (Appendix B's drift class).
   *
   * Pointer pipeline (component-local, Actions only — no enablement logic
   * lives here): wheel zoom-to-cursor on CONTAINER-relative points (the
   * legacy offsetX/offsetY anchor bug class), double-click = zoom toggle
   * at cursor, drag pans when zoomed — the ONLY pan since June 12 2026:
   * Space became 100% the microphone key (defs/global.ts mic-press), so
   * the whole Space hold-to-pan / clean-tap-close pipeline was deleted.
   * The one raw key fact left here is the hold-E eraser release (the
   * registry is keydown-only; engagement is the pencil-eraser row).
   */
  import { ui } from "../../state/app.svelte";
  import { displayUrl, embeddedUrl, originalUrl } from "../../ipc/urls";
  import * as zoom from "../../logic/zoom";
  import {
    FIRST_SOURCE,
    loadProvesPixels,
    needsOriginal,
    nextSource,
    type FullresSource,
  } from "../../logic/fullres";
  import PencilOverlay from "./PencilOverlay.svelte";

  let stageEl: HTMLDivElement | undefined = $state();
  let cw = $state(0);
  let ch = $state(0);
  let natW = $state(0);
  let natH = $state(0);

  const hash = $derived(ui.look.currentHash);

  // New image (←/→ or R): dims are unknown until load — the transform
  // waits for them rather than reusing the previous image's. The full-res
  // source re-proves its load per image (cache makes a revisit instant).
  $effect(() => {
    void hash;
    natW = 0;
    natH = 0;
    fullresLoadedHash = null;
  });

  const container = $derived<zoom.Dims>({ w: cw, h: ch });
  const image = $derived<zoom.Dims>({ w: natW, h: natH });
  const ready = $derived(cw > 0 && ch > 0 && natW > 0 && natH > 0);

  const mode = $derived(ui.look.zoomSession?.mode ?? "fit");
  const t = $derived(
    ready ? zoom.carryOver(ui.look.zoomSession, container, image) : null,
  );

  // ---- progressive full resolution (logic/fullres.ts owns predicate+ladder) ----
  //
  // Past what the preview can supply, climb the source ladder: /original
  // first (webview-decodable stored formats), then /embedded (the RAW's
  // native-size embedded JPEG, dogfood round 2); each protocol refusal is
  // a 404 that advances the rung, and an exhausted ladder leaves the
  // preview standing silently (TIFF/HEIC: the M1.5 backfill). The request
  // is STICKY per hash (zooming back out never re-fetches or flickers);
  // the preview stays painted until a source has PROVABLY loaded (nonzero
  // natural dims — WKWebView fires a lying 0×0 "load" for a 404 after an
  // onerror-driven src swap; see loadProvesPixels), then they swap in
  // place. The full-res image renders into the preview's layout box
  // (explicit natW×natH), so the live transform — derived from the
  // canonical zoom session — carries over exactly, by construction, and
  // strokes drawn over the preview keep their substrate geometry.

  /** Hash whose full-res has been requested (sticky for the session). */
  let fullresHash = $state<string | null>(null);
  /** The ladder rung currently requested for fullresHash. */
  let fullresSource = $state<FullresSource>(FIRST_SOURCE);
  /** Hash whose full-res <img> has finished loading (the swap gate). */
  let fullresLoadedHash = $state<string | null>(null);
  /** Hashes whose whole ladder the protocol refused: never re-asked. */
  let fullresFailed = $state<ReadonlySet<string>>(new Set());

  const wantsFullres = $derived(
    hash !== null &&
      ready &&
      t !== null &&
      !fullresFailed.has(hash) &&
      needsOriginal({
        scale: t.scale,
        preview: image,
        devicePixelRatio: window.devicePixelRatio || 1,
      }),
  );
  $effect(() => {
    if (wantsFullres && fullresHash !== hash) {
      fullresHash = hash;
      fullresSource = FIRST_SOURCE;
    }
  });
  const fullresShown = $derived(
    hash !== null && fullresHash === hash && fullresLoadedHash === hash,
  );

  function onFullresError() {
    if (fullresHash === null) return;
    const next = nextSource(fullresSource);
    if (next !== null) {
      fullresSource = next; // e.g. a RAW: /original refused, try /embedded
      return;
    }
    const failed = new Set(fullresFailed);
    failed.add(fullresHash);
    fullresFailed = failed; // ladder exhausted: the preview stands (M1.5)
    fullresHash = null;
  }

  /** The swap happens HERE and only here — after a load that PROVED pixels
   * (logic/fullres.ts loadProvesPixels). WKWebView fires a lying `load`
   * (natural dims 0×0) instead of `error` when a src swapped inside its
   * own onerror then 404s — the RAW ladder's exact shape — and trusting it
   * replaced the painted preview with the broken-image glyph. A dimension-
   * less "load" is a refusal and walks the same ladder as onerror. */
  function onFullresLoad(e: Event) {
    const img = e.currentTarget as HTMLImageElement;
    if (loadProvesPixels({ w: img.naturalWidth, h: img.naturalHeight })) {
      fullresLoadedHash = hash;
    } else {
      onFullresError();
    }
  }

  // ---- session writes (the only zoom mutations) ------------------------------

  function applyZoom(next: zoom.ZoomTransform, m: zoom.ZoomMode) {
    ui.look.zoomSession = zoom.toSession(next, container, image, m);
    flashReadout(`${Math.round(next.scale * 100)}%`);
  }

  function setFit() {
    ui.look.zoomSession = null; // null session = fit (the entry default)
    flashReadout("Fit");
  }

  /** Z / double-click: Fit ⇄ 100% anchored at a container point. */
  function toggleZoom(p: zoom.Point) {
    if (t === null) return;
    if (mode === "fit") applyZoom(zoom.zoomAtPoint(t, container, image, p, 1), "actual");
    else setFit();
  }

  function center(): zoom.Point {
    return { x: cw / 2, y: ch / 2 };
  }

  /** CONTAINER-relative event point — never offsetX/offsetY: over the
   * transformed <img> those are image-local and re-introduce the drift. */
  function stagePoint(e: { clientX: number; clientY: number }): zoom.Point {
    const r = stageEl?.getBoundingClientRect();
    return r === undefined ? center() : { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  // Zoom commands from the action router (Z anchors at the pointer; the
  // chord rows carry no point, so the anchor is a stage fact).
  let handledSeq = 0;
  $effect(() => {
    const cmd = ui.look.zoomCmd;
    if (cmd.seq === 0 || cmd.seq === handledSeq) return;
    handledSeq = cmd.seq;
    if (cmd.op === "fit") {
      setFit();
    } else if (t === null) {
      return; // no image yet: nothing to anchor against
    } else if (cmd.op === "toggle") {
      toggleZoom(lastPointer ?? center());
    } else if (cmd.op === "actual") {
      applyZoom(zoom.zoomAtPoint(t, container, image, center(), 1), "actual");
    } else if (cmd.op === "step") {
      const next = zoom.stepScale(t.scale, cmd.delta ?? 1);
      applyZoom(zoom.zoomAtPoint(t, container, image, center(), next), "free");
    }
  });

  // ---- wheel: continuous zoom-to-cursor (featureset §2) ----------------------

  // deltaMode 1 (line-scrolling legacy mice): browsers' conventional line
  // height in px. zoom.ts's wheel rate is calibrated against PIXEL deltas,
  // so this scale factor sets the zoom feel for line-mode devices.
  const WHEEL_LINE_PX = 16;

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (t === null) return;
    const deltaY = e.deltaMode === 1 ? e.deltaY * WHEEL_LINE_PX : e.deltaY;
    const next = zoom.wheelScale(t.scale, deltaY);
    applyZoom(zoom.zoomAtPoint(t, container, image, stagePoint(e), next), "free");
  }

  // ---- drag-pan when zoomed ---------------------------------------------------

  let lastPointer = $state<zoom.Point | null>(null);
  let dragging = false;
  let lastX = 0;
  let lastY = 0;

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return; // right-click = the backdrop menu seat
    if (mode === "fit") return; // drag pans only when zoomed
    dragging = true;
    lastX = e.clientX;
    lastY = e.clientY;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    lastPointer = stagePoint(e); // Z / double-click anchor
    if (!dragging || t === null) return;
    const dx = e.clientX - lastX;
    const dy = e.clientY - lastY;
    lastX = e.clientX;
    lastY = e.clientY;
    ui.look.zoomSession = zoom.toSession(zoom.panBy(t, dx, dy), container, image, mode);
  }

  function onPointerUp() {
    dragging = false;
  }

  function onPointerLeave() {
    dragging = false;
    lastPointer = null;
  }

  function onWindowKeyup(e: KeyboardEvent) {
    // Hold-E eraser release (P5.1): engagement is the registry's
    // pencil-eraser row; the release is a raw key fact (the registry is
    // keydown-only — the same split the Space mic uses in App.svelte).
    // Released unconditionally so a hold can never wedge.
    if (e.key === "e" || e.key === "E") ui.look.eraserHeld = false;
  }

  // ---- transient zoom readout [nice] (obeys lights-out) ----------------------

  // Long enough to read at a glance, gone before it reads as chrome —
  // the same transient-duration family as COPY_FLASH_MS / TOAST_DISMISS_MS.
  const READOUT_FLASH_MS = 900;
  let readout = $state<string | null>(null);
  let readoutTimer: ReturnType<typeof setTimeout> | undefined;
  function flashReadout(text: string) {
    readout = text;
    clearTimeout(readoutTimer);
    readoutTimer = setTimeout(() => (readout = null), READOUT_FLASH_MS);
  }
</script>

<svelte:window onkeyup={onWindowKeyup} />

<div
  class="stage"
  class:zoomed={mode !== "fit"}
  bind:this={stageEl}
  bind:clientWidth={cw}
  bind:clientHeight={ch}
  onwheel={onWheel}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointerleave={onPointerLeave}
  ondblclick={(e) => toggleZoom(stagePoint(e))}
  role="img"
  aria-label="Photograph"
>
  {#if hash !== null}
    <img
      src={displayUrl(hash)}
      alt=""
      draggable="false"
      class:ready
      class:supplanted={fullresShown}
      style:transform={t !== null
        ? `translate(${t.tx}px, ${t.ty}px) scale(${t.scale})`
        : "none"}
      onload={(e) => {
        const img = e.currentTarget as HTMLImageElement;
        natW = img.naturalWidth;
        natH = img.naturalHeight;
      }}
    />
    {#if fullresHash === hash}
      <!-- the full-res source (original or embedded-native), laid out in
           the PREVIEW's pixel box under the same transform: invisible
           until loaded, then swapped in place -->
      <img
        class="fullres"
        class:shown={fullresShown}
        src={fullresSource === "original" ? originalUrl(hash) : embeddedUrl(hash)}
        alt=""
        draggable="false"
        decoding="async"
        style:width="{natW}px"
        style:height="{natH}px"
        style:transform={t !== null
          ? `translate(${t.tx}px, ${t.ty}px) scale(${t.scale})`
          : "none"}
        onload={onFullresLoad}
        onerror={onFullresError}
      />
    {/if}
  {/if}
  {#if hash !== null && ready && t !== null}
    <!-- the tracing paper (P5.1): folded strokes + the live stroke; its
         pointer-events gate keeps drag-pan/wheel on the stage -->
    <PencilOverlay {t} {container} {image} {hash} />
  {/if}
  {#if readout !== null && !ui.shell.chromeHidden}
    <span class="readout">{readout}</span>
  {/if}
</div>

<style>
  .stage {
    position: absolute;
    inset: 0;
    overflow: hidden;
  }
  .stage.zoomed {
    cursor: grab;
  }
  .stage > img {
    position: absolute;
    top: 0;
    left: 0;
    transform-origin: 0 0;
    will-change: transform;
    user-select: none;
    -webkit-user-select: none;
  }
  .stage > img:not(.ready) {
    visibility: hidden; /* dims unknown: never paint a misplaced frame */
  }
  /* The progressive pair: the preview hides only once the full-res
   * source is actually painted (no flash, no blocking). */
  .stage > img.supplanted {
    visibility: hidden;
  }
  .stage > img.fullres {
    visibility: hidden;
  }
  .stage > img.fullres.shown {
    visibility: visible;
  }
  .readout {
    position: absolute;
    left: 14px;
    bottom: 12px;
    padding: 2px 10px;
    border-radius: 12px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    color: var(--text-dim);
    font-size: 12px;
    pointer-events: none;
  }
</style>
