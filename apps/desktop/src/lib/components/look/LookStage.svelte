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
   * at cursor, drag pans when zoomed, Space = hold-to-pan while zoomed
   * with the clean-tap-close resolved by logic/looknav.ts (at fit the
   * registry's look-close row consumes Space before it reaches here).
   * `atFit` is reported back for that row's enabled gate.
   */
  import { ui } from "../../state/app.svelte";
  import { displayUrl, originalUrl } from "../../ipc/urls";
  import * as zoom from "../../logic/zoom";
  import { needsOriginal } from "../../logic/fullres";
  import {
    SPACE_IDLE,
    spaceDown,
    spacePanned,
    spaceUp,
    type SpaceHoldState,
  } from "../../logic/looknav";

  let stageEl: HTMLDivElement | undefined = $state();
  let cw = $state(0);
  let ch = $state(0);
  let natW = $state(0);
  let natH = $state(0);

  const hash = $derived(ui.look.currentHash);

  // New image (←/→ or R): dims are unknown until load — the transform
  // waits for them rather than reusing the previous image's. The original
  // re-proves its load per image (cache makes a revisit instant).
  $effect(() => {
    void hash;
    natW = 0;
    natH = 0;
    originalLoadedHash = null;
  });

  const container = $derived<zoom.Dims>({ w: cw, h: ch });
  const image = $derived<zoom.Dims>({ w: natW, h: natH });
  const ready = $derived(cw > 0 && ch > 0 && natW > 0 && natH > 0);

  const mode = $derived(ui.look.zoomSession?.mode ?? "fit");
  const t = $derived(
    ready ? zoom.carryOver(ui.look.zoomSession, container, image) : null,
  );

  // Report fit state for the Space dual-role gate (look-close enabled).
  $effect(() => {
    ui.look.atFit = mode === "fit";
  });

  // ---- progressive full resolution (logic/fullres.ts owns the predicate) ----
  //
  // Past what the preview can supply, request the original (its protocol
  // route refuses RAW/TIFF/offline with 404 — the preview stands silently).
  // The request is STICKY per hash (zooming back out never re-fetches or
  // flickers); the preview stays painted until the original has LOADED,
  // then they swap in place. The original renders into the preview's
  // layout box (explicit natW×natH), so the live transform — derived from
  // the canonical zoom session — carries over exactly, by construction.

  /** Hash whose original has been requested (sticky for the session). */
  let originalHash = $state<string | null>(null);
  /** Hash whose original <img> has finished loading (the swap gate). */
  let originalLoadedHash = $state<string | null>(null);
  /** Hashes whose original the protocol refused (404): never re-asked. */
  let originalFailed = $state<ReadonlySet<string>>(new Set());

  const wantsOriginal = $derived(
    hash !== null &&
      ready &&
      t !== null &&
      !originalFailed.has(hash) &&
      needsOriginal({
        scale: t.scale,
        preview: image,
        devicePixelRatio: window.devicePixelRatio || 1,
      }),
  );
  $effect(() => {
    if (wantsOriginal) originalHash = hash;
  });
  const originalShown = $derived(
    hash !== null && originalHash === hash && originalLoadedHash === hash,
  );

  function onOriginalError() {
    if (originalHash === null) return;
    const failed = new Set(originalFailed);
    failed.add(originalHash);
    originalFailed = failed; // e.g. a RAW source: the preview stands (M1.5)
    originalHash = null;
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

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (t === null) return;
    const deltaY = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY; // lines → px
    const next = zoom.wheelScale(t.scale, deltaY);
    applyZoom(zoom.zoomAtPoint(t, container, image, stagePoint(e), next), "free");
  }

  // ---- drag-pan when zoomed + Space hold-to-pan ------------------------------

  let lastPointer = $state<zoom.Point | null>(null);
  let dragging = false;
  let lastX = 0;
  let lastY = 0;
  let spaceHold: SpaceHoldState = SPACE_IDLE;

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
    spaceHold = spacePanned(spaceHold); // a pan breaks the clean tap
  }

  function onPointerUp() {
    dragging = false;
  }

  function onPointerLeave() {
    dragging = false;
    lastPointer = null;
  }

  // Space pipeline: raw key events (not Actions — the registry's look-close
  // row consumed the at-fit tap before this listener sees anything useful;
  // looknav.ts resolves the zoomed tap-vs-hold). Typing keeps typing.
  function isTextInput(el: Element | null): boolean {
    return (
      el instanceof HTMLInputElement ||
      el instanceof HTMLTextAreaElement ||
      (el instanceof HTMLElement && el.isContentEditable)
    );
  }

  // Raw-pipeline ownership, mirroring match.ts's suppression rules (this
  // is pipeline routing, not enablement — that stays on the defs): while
  // an overlay/menu/the rail owns the keys, Space neither pans nor closes.
  function stageOwnsRawKeys(): boolean {
    return (
      !ui.searchOpen &&
      !ui.shell.cheatsheetOpen &&
      ui.shell.contextMenu === null &&
      !ui.shell.railFocused &&
      !isTextInput(document.activeElement)
    );
  }

  function onWindowKeydown(e: KeyboardEvent) {
    if (e.key !== " " || !stageOwnsRawKeys()) return;
    spaceHold = spaceDown(spaceHold, { atFit: mode === "fit", repeat: e.repeat });
    // While the hold is engaged, Space must not scroll or click a focused
    // control (repeats included).
    if (spaceHold.held) e.preventDefault();
  }

  function onWindowKeyup(e: KeyboardEvent) {
    if (e.key !== " ") return;
    // Always resolve the machine (a hold must never wedge), but a clean
    // tap acts only if the stage still owns the keys at release.
    const { state, outcome } = spaceUp(spaceHold);
    spaceHold = state;
    if (outcome === "close" && stageOwnsRawKeys())
      void ui.perform({ kind: "look-close" });
  }

  // ---- transient zoom readout [nice] (obeys lights-out) ----------------------

  let readout = $state<string | null>(null);
  let readoutTimer: ReturnType<typeof setTimeout> | undefined;
  function flashReadout(text: string) {
    readout = text;
    clearTimeout(readoutTimer);
    readoutTimer = setTimeout(() => (readout = null), 900);
  }
</script>

<svelte:window onkeydown={onWindowKeydown} onkeyup={onWindowKeyup} />

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
      class:supplanted={originalShown}
      style:transform={t !== null
        ? `translate(${t.tx}px, ${t.ty}px) scale(${t.scale})`
        : "none"}
      onload={(e) => {
        const img = e.currentTarget as HTMLImageElement;
        natW = img.naturalWidth;
        natH = img.naturalHeight;
      }}
    />
    {#if originalHash === hash}
      <!-- the original, laid out in the PREVIEW's pixel box under the same
           transform: invisible until loaded, then swapped in place -->
      <img
        class="original"
        class:shown={originalShown}
        src={originalUrl(hash)}
        alt=""
        draggable="false"
        decoding="async"
        style:width="{natW}px"
        style:height="{natH}px"
        style:transform={t !== null
          ? `translate(${t.tx}px, ${t.ty}px) scale(${t.scale})`
          : "none"}
        onload={() => {
          originalLoadedHash = hash;
        }}
        onerror={onOriginalError}
      />
    {/if}
  {/if}
  {#if readout !== null && !ui.shell.chromeHidden}
    <span class="readout">{readout}</span>
  {/if}
  <!-- overlay slot reserved: the M2a stroke canvas mounts here -->
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
  /* The progressive-original pair: the preview hides only once the
   * original is actually painted (no flash, no blocking). */
  .stage > img.supplanted {
    visibility: hidden;
  }
  .stage > img.original {
    visibility: hidden;
  }
  .stage > img.original.shown {
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
