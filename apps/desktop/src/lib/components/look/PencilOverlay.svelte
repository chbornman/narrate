<script lang="ts">
  /**
   * The tracing-paper canvas (P5.1 — CAPTURE §8, UI §4.4): folded strokes
   * of the VIEWED image + the live in-flight stroke, drawn through the
   * stage's CURRENT transform so marks zoom with the image. ZERO chrome:
   * the cursor dot and the indicator's mode segment are the entire
   * announcement.
   *
   * Thin glue only — geometry lives in logic/stroke.ts, the drawing
   * machine in logic/pencil.ts. Pointer capture engages only in pencil
   * mode with Space up (`pointer-events: none` otherwise, so plain-drag /
   * Space-drag panning and wheel zoom keep flowing to the stage).
   * Pointer-cancel discards silently; Ctrl+Z mid-draw arrives over the
   * slice's penCancelSeq channel.
   */
  import { ui } from "../../state/app.svelte";
  import * as ipc from "../../ipc/commands";
  import type { Dims, ZoomTransform } from "../../logic/zoom";
  import * as stroke from "../../logic/stroke";
  import {
    livePoints,
    livePressures,
    penDown,
    penMove,
    penUp,
    type PenState,
  } from "../../logic/pencil";
  import { BASE_W_DEFAULT } from "../../logic/stroke";
  import type { StrokeWirePoint } from "../../types/dto";

  let {
    t,
    container,
    image,
    hash,
  }: {
    t: ZoomTransform | null;
    container: Dims;
    image: Dims;
    hash: string;
  } = $props();

  let canvasEl: HTMLCanvasElement | undefined = $state();

  // ---- folded strokes + display orientation (refetched on any mutation) -----

  interface OverlayStroke {
    id: string;
    orientation: number;
    baseW: number;
    points: StrokeWirePoint[];
  }

  let strokes = $state<OverlayStroke[]>([]);
  let displayOrientation = $state(1);
  let loadSeq = 0;

  $effect(() => {
    const h = hash;
    void ui.look.strokesVersion; // any stroke mutation refetches the fold
    const seq = ++loadSeq;
    void (async () => {
      try {
        const [rows, meta] = await Promise.all([
          ipc.imageJournal(h),
          ipc.imageMetadata(h),
        ]);
        if (seq !== loadSeq) return;
        displayOrientation = meta.orientation;
        strokes = rows
          .filter((r) => r.kind === "stroke" && !r.retracted && r.stroke != null)
          .map((r) => ({
            id: r.id,
            orientation: r.stroke!.orientation,
            baseW: r.stroke!.baseW,
            points: r.stroke!.points,
          }));
      } catch {
        if (seq !== loadSeq) return;
        strokes = []; // backend unavailable (tests/dev): clean paper
        displayOrientation = 1;
      }
    })();
  });

  // ---- the drawing machine (logic/pencil.ts) ---------------------------------

  let pen: PenState | null = null;
  let liveSeq = $state(0); // redraw trigger for in-flight samples

  function canvasPoint(e: PointerEvent): { x: number; y: number } {
    const r = canvasEl?.getBoundingClientRect();
    return r === undefined
      ? { x: e.clientX, y: e.clientY }
      : { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  function sampleOf(e: PointerEvent) {
    const p = canvasPoint(e);
    return {
      x: p.x,
      y: p.y,
      pressure: e.pressure,
      pointerType: e.pointerType,
      timeMs: e.timeStamp,
    };
  }

  function discardPen() {
    pen = null;
    ui.look.penDown = false;
    liveSeq += 1;
  }

  /** The stylus eraser end counts (UI §4.4): eraser button = bit 32 in
   * `buttons` (button 5 on the down transition). */
  function eraserIntent(e: PointerEvent): boolean {
    return ui.look.eraserHeld || e.button === 5 || (e.buttons & 32) !== 0;
  }

  function onPointerDown(e: PointerEvent) {
    if (!ui.look.pencilMode || t === null) return;
    e.stopPropagation(); // the stage must not also start a drag-pan
    if (eraserIntent(e)) {
      eraseAt(e);
      return;
    }
    if (e.button !== 0) return; // right-click stays the backdrop menu seat
    canvasEl?.setPointerCapture(e.pointerId);
    pen = penDown(sampleOf(e), { t, image });
    ui.look.penDown = true;
    liveSeq += 1;
  }

  function onPointerMove(e: PointerEvent) {
    if (pen === null || t === null) return;
    // Coalesced samples where available; the pipeline MUST produce
    // acceptable strokes from plain pointermove cadence alone (§8.3 —
    // getCoalescedEvents is absent in WKWebView before macOS 15.2).
    const events =
      typeof e.getCoalescedEvents === "function" ? e.getCoalescedEvents() : [];
    for (const ev of events.length > 0 ? events : [e])
      penMove(pen, sampleOf(ev), { t, image });
    liveSeq += 1;
  }

  function onPointerUp(e: PointerEvent) {
    if (pen === null) return;
    if (t === null) {
      discardPen(); // transform lost mid-stroke: nothing truthful to commit
      return;
    }
    // B41: the pointer-up position/time is the stroke's final stored
    // sample (dedupe-exempt) — ts − t_last becomes the exact pen span.
    const payload = penUp(pen, displayOrientation, sampleOf(e), { t, image });
    discardPen();
    if (payload !== null) void ui.commitStroke(payload); // pulse rides the backend
  }

  // Palm rejection / window loss: discard, nothing logged (§8.4).
  function onPointerCancel() {
    discardPen();
  }

  // Ctrl+Z during pen-down: cancel in-progress, local and unlogged.
  let handledCancelSeq = 0;
  $effect(() => {
    const seq = ui.look.penCancelSeq;
    if (seq === handledCancelSeq) return;
    handledCancelSeq = seq;
    if (pen !== null) discardPen();
  });

  // ←/→ mid-stroke: the image changed under the pen — discard (a commit
  // would bind old geometry to the new image). Unmount cleans up too.
  $effect(() => {
    void hash;
    return () => {
      if (pen !== null) discardPen();
    };
  });

  // ---- eraser (§8.6: whole-stroke retract via the existing tombstone path) ---

  function eraseAt(e: PointerEvent) {
    if (t === null) return;
    const tap = stroke.normalize(stroke.screenToImage(canvasPoint(e), t), image);
    const target = stroke.pickEraserTarget(
      tap,
      strokes,
      image,
      t.scale,
      displayOrientation,
    );
    if (target !== null) void ui.eraseStroke(target);
  }

  // ---- raw key facts: hold-E release, Space yields the pointer ----------------

  let spaceHeld = $state(false);

  function isTextInput(el: Element | null): boolean {
    return (
      el instanceof HTMLInputElement ||
      el instanceof HTMLTextAreaElement ||
      (el instanceof HTMLElement && el.isContentEditable)
    );
  }

  function onWindowKeydown(e: KeyboardEvent) {
    if (e.key === " " && !isTextInput(document.activeElement)) spaceHeld = true;
  }

  function onWindowKeyup(e: KeyboardEvent) {
    if (e.key === " ") spaceHeld = false;
    // (Hold-E release lives in LookStage's keyup — mounted for the whole
    // Look visit, including before this canvas has image dims.)
  }

  function onWindowBlur() {
    spaceHeld = false;
    ui.look.eraserHeld = false;
    if (pen !== null) discardPen(); // window loss = pointer-cancel semantics
  }

  // ---- journal-row flash (UI §8.2) --------------------------------------------

  let flashId = $state<string | null>(null);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const flash = ui.look.strokeFlash;
    if (flash === null) return;
    void flash.seq;
    flashId = flash.id;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => (flashId = null), 700);
  });

  // ---- cursors (zero chrome: the dot IS the mode announcement) ----------------
  //
  // CSS cursors cannot reference tokens; the SVG data-URIs are built from
  // the COMPUTED token values (no hex lives here — I5 discipline).

  let penCursor = $state("crosshair");
  let eraserCursor = $state("crosshair");

  $effect(() => {
    const styles = getComputedStyle(document.documentElement);
    const red = styles.getPropertyValue("--red-pencil").trim();
    const text = styles.getPropertyValue("--text").trim();
    if (red === "" || text === "") return; // tokens unresolved — keep crosshair, never a raw color
    const svg = (body: string) =>
      `url("data:image/svg+xml,${encodeURIComponent(
        `<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'>${body}</svg>`,
      )}") 8 8, crosshair`;
    penCursor = svg(`<circle cx='8' cy='8' r='3' fill='${red}'/>`);
    eraserCursor = svg(
      `<circle cx='8' cy='8' r='5.5' fill='none' stroke='${text}' stroke-width='1.5'/>`,
    );
  });

  // ---- rendering ---------------------------------------------------------------

  const active = $derived(ui.look.pencilMode && !spaceHeld);

  let pencilRed = $state(""); // resolved from the token below; empty until then
  $effect(() => {
    if (canvasEl === undefined) return;
    pencilRed = getComputedStyle(canvasEl).getPropertyValue("--red-pencil").trim();
  });

  function drawPath(
    ctx: CanvasRenderingContext2D,
    pts: { x: number; y: number }[],
    widths: number[],
  ) {
    if (pts.length === 0) return;
    if (pts.length === 1) {
      ctx.beginPath();
      ctx.arc(pts[0].x, pts[0].y, Math.max(widths[0], 1) / 2, 0, Math.PI * 2);
      ctx.fill();
      return;
    }
    // Render-only centripetal Catmull-Rom (§8.3); width interpolates per
    // segment (§8.2 width model), round caps keep the joins continuous.
    const segs = stroke.catmullRomBeziers(pts);
    for (const [i, s] of segs.entries()) {
      ctx.beginPath();
      ctx.lineWidth = Math.max((widths[i] + widths[i + 1]) / 2, 0.5);
      ctx.moveTo(s.p0.x, s.p0.y);
      ctx.bezierCurveTo(s.c1.x, s.c1.y, s.c2.x, s.c2.y, s.p1.x, s.p1.y);
      ctx.stroke();
    }
  }

  $effect(() => {
    // Reactive draw dependencies: transform, geometry, live stroke, flash.
    void liveSeq;
    void flashId;
    const c = canvasEl;
    if (c === undefined) return;
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(container.w * dpr));
    const h = Math.max(1, Math.round(container.h * dpr));
    if (c.width !== w) c.width = w;
    if (c.height !== h) c.height = h;
    const ctx = c.getContext("2d");
    if (ctx === null) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, container.w, container.h);
    if (!ui.look.overlayVisible || t === null) return;
    if (pencilRed === "") return; // token unresolved — skip the frame rather than paint off-token
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.strokeStyle = pencilRed;
    ctx.fillStyle = pencilRed;
    for (const s of strokes) {
      const spec = stroke.strokeScreenSpec(
        s.points,
        s.baseW,
        image,
        t,
        s.orientation,
        displayOrientation,
      );
      if (s.id === flashId) {
        // The journal-row flash: a soft wide echo under the stroke.
        ctx.globalAlpha = 0.35;
        drawPath(
          ctx,
          spec.pts,
          spec.widths.map((wd) => wd * 3 + 6),
        );
        ctx.globalAlpha = 1;
      }
      drawPath(ctx, spec.pts, spec.widths);
    }
    if (pen !== null) {
      const pts = livePoints(pen, t);
      const widths = livePressures(pen).map((p) =>
        stroke.screenWidth(p, BASE_W_DEFAULT, image, t.scale),
      );
      drawPath(ctx, pts, widths);
    }
  });
</script>

<svelte:window
  onkeydown={onWindowKeydown}
  onkeyup={onWindowKeyup}
  onblur={onWindowBlur}
/>

<canvas
  bind:this={canvasEl}
  class="pencil-overlay"
  style:pointer-events={active ? "auto" : "none"}
  style:cursor={ui.look.eraserHeld ? eraserCursor : penCursor}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerCancel}
  ondblclick={(e) => e.stopPropagation()}
  aria-hidden="true"
></canvas>

<style>
  /* Clipped to the stage; ZERO pixels of chrome (UI §4.4). */
  .pencil-overlay {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
</style>
