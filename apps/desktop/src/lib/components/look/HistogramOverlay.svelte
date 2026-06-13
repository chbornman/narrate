<script lang="ts">
  /**
   * The Look histogram overlay (H) — a REVIEWING aid, not an editing tool:
   * a small RGB + luminance histogram docked in the canvas so a reviewer
   * can check exposure and clipping against the faithfully-rendered image.
   *
   * Pure/thin split (the zoom.ts ⇄ LookStage convention): all the counting
   * lives in logic/histogram.ts (unit-tested against known pixels); this
   * component is the DOM glue — it draws the displayed source into an
   * offscreen canvas (downsampled to a bounded working size), reads the
   * pixels back with getImageData, bins ONCE per image change (never per
   * frame, off the render path), and paints the curves into a visible
   * canvas.
   *
   * Data source: the SAME bytes the stage shows. The Look image is served
   * over the photoproof:// custom scheme (same-origin), so an <img> loaded
   * here is canvas-readable (no tainting). `src` is the source currently
   * painted on the stage — the preview, or the full-res/develop artifact
   * once it swaps in — so the histogram tracks the faithful render and
   * recomputes when the develop lands.
   *
   * Lights-out: the parent only mounts this when chrome is shown, and we
   * also guard on chromeHidden so Tab hides it unconditionally (it is
   * canvas chrome, not a panel, so it is not in the root's panel snapshot).
   */
  import { binRgba, normalize, workingSize } from "../../logic/histogram";

  interface Props {
    /** The source URL currently painted on the stage (preview or full-res).
     * Recompute keys off this: a new photo or a develop swap changes it. */
    src: string;
  }
  let { src }: Props = $props();

  // The visible plot canvas. ~256px wide so each of the 256 bins maps to a
  // device-pixel column at DPR 1; a small fixed height reads as a glance
  // aid, not a panel.
  const PLOT_W = 256;
  const PLOT_H = 120;

  let plotEl: HTMLCanvasElement | undefined = $state();

  // The offscreen worker canvas the source is downsampled into before
  // getImageData. Created once and reused (a fresh canvas per image change
  // would churn GPU-backed buffers for no benefit).
  let worker: HTMLCanvasElement | undefined;

  /**
   * Recompute the histogram from `src`. Loads the source (same-origin
   * custom scheme → canvas-readable), draws it downsampled, bins, paints.
   * The whole pass runs once per `src` change and is canceled cleanly if
   * `src` changes again mid-load (the stale `img.onload` no-ops).
   */
  $effect(() => {
    const url = src;
    if (url === "" || plotEl === undefined) return;

    let stale = false;
    const img = new Image();
    // The custom scheme is same-origin, so the canvas stays untainted and
    // getImageData is allowed; crossOrigin is set defensively regardless.
    img.crossOrigin = "anonymous";
    img.decoding = "async";
    img.onload = () => {
      if (stale) return;
      paintFrom(img);
    };
    img.onerror = () => {
      // A source that will not decode here (rare: the stage proved it loads)
      // just leaves the previous curve standing rather than blanking it.
    };
    img.src = url;

    return () => {
      stale = true; // src changed before this load finished: drop it
    };
  });

  /** Draw `img` into the worker canvas at the bounded working size, read it
   * back, bin it, and paint the curves. The single getImageData here is the
   * only main-thread cost, and it runs once per image (not per frame). */
  function paintFrom(img: HTMLImageElement) {
    const sw = img.naturalWidth;
    const sh = img.naturalHeight;
    if (sw === 0 || sh === 0 || plotEl === undefined) return;

    const { w, h } = workingSize(sw, sh);
    if (worker === undefined) worker = document.createElement("canvas");
    worker.width = w;
    worker.height = h;
    const wctx = worker.getContext("2d", { willReadFrequently: true });
    if (wctx === null) return;
    // Downsample the whole image into the small working canvas: the browser
    // does the averaging, so a 50MP develop becomes ~1MP of representative
    // pixels before we ever touch them in JS.
    wctx.drawImage(img, 0, 0, w, h);

    const pixels = wctx.getImageData(0, 0, w, h).data;
    const hist = binRgba(pixels); // pure: counts into 256 bins per channel
    paintCurves(hist);
  }

  /** Paint the four normalized curves into the visible plot. Channels draw
   * in additive blend (the standard photo-histogram look: overlaps brighten
   * toward white); luminance is a faint envelope under them so the combined
   * value curve reads without a separate axis. Log vertical scaling
   * (logic/histogram normalize default) keeps the sparse highlight/shadow
   * tails legible so end-of-range clipping is visible at a glance. */
  function paintCurves(hist: ReturnType<typeof binRgba>) {
    const ctx = plotEl?.getContext("2d");
    if (ctx === undefined || ctx === null) return;

    ctx.clearRect(0, 0, PLOT_W, PLOT_H);

    // Luminance first, underneath: a dim grey fill envelope.
    const lum = normalize(hist.lum);
    ctx.globalCompositeOperation = "source-over";
    fillCurve(ctx, lum, "rgba(170, 170, 170, 0.35)");

    // R, G, B in additive ("lighter") blend so overlaps go toward white,
    // the conventional RGB-histogram reading.
    ctx.globalCompositeOperation = "lighter";
    fillCurve(ctx, normalize(hist.r), "rgba(255, 64, 64, 0.7)");
    fillCurve(ctx, normalize(hist.g), "rgba(64, 255, 64, 0.7)");
    fillCurve(ctx, normalize(hist.b), "rgba(64, 96, 255, 0.7)");
    ctx.globalCompositeOperation = "source-over";
  }

  /** Fill one channel's curve as a filled area from the baseline. `heights`
   * is 256 values in [0,1]; bin i maps to column i (canvas is 256 wide). */
  function fillCurve(
    ctx: CanvasRenderingContext2D,
    heights: Float32Array,
    fill: string,
  ) {
    ctx.beginPath();
    ctx.moveTo(0, PLOT_H);
    for (let i = 0; i < heights.length; i++) {
      // 1px inset top so a full spike is not clipped by the canvas edge.
      const y = PLOT_H - heights[i] * (PLOT_H - 1);
      ctx.lineTo(i, y);
    }
    ctx.lineTo(PLOT_W - 1, PLOT_H);
    ctx.closePath();
    ctx.fillStyle = fill;
    ctx.fill();
  }
</script>

<div class="histogram" aria-hidden="true">
  <canvas bind:this={plotEl} width={PLOT_W} height={PLOT_H}></canvas>
</div>

<style>
  /* Docked top-right: the bottom corners already hold the zoom readout
     (bottom-left) and the "developing..." status (bottom-right), and the
     top-left is the natural eye-path start for the photo itself. */
  .histogram {
    position: absolute;
    top: 12px;
    right: 14px;
    width: 256px;
    padding: 6px;
    border-radius: 8px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    /* A reviewing aid that never steals a gesture from the stage. */
    pointer-events: none;
    /* Semi-transparent so the photo reads through at the edge. */
    opacity: 0.9;
  }
  .histogram canvas {
    display: block;
    width: 256px;
    height: 120px;
    /* The plot sits on a near-black field so the additive curves pop. */
    background: rgba(0, 0, 0, 0.55);
    border-radius: 4px;
  }
</style>
