/**
 * Progressive full-resolution in Look (dogfood round 1). The convention
 * (Lightroom builds 1:1 previews on demand on top of standard previews;
 * Capture One zooms from proxies into the full decode): Look opens on the
 * 2560 px display preview and swaps in the ORIGINAL file once the zoom
 * demands more pixels than the preview can supply.
 *
 * THE SWAP-THRESHOLD PREDICATE, pure: the preview runs out when the
 * rendered size (zoom scale × the preview's fitted/natural dims, in DEVICE
 * pixels) exceeds the preview's own pixel dims — i.e. the preview is being
 * upsampled on screen. In zoom.ts terms (`scale` = screen px per preview
 * px) that reduces to `scale × devicePixelRatio > 1`.
 *
 * Everything else is owned elsewhere: the protocol serves /original only
 * for webview-decodable STORED formats (RAW/TIFF 404 → Look keeps the
 * preview silently, M1.5 backfill); LookStage keeps the preview painted
 * until the original has actually loaded, renders the original into the
 * preview's layout box, and derives both from the canonical zoom session —
 * the transform carries over exactly by construction.
 */
import type { Dims } from "./zoom";

/** Headroom so exact 1:1 (and float noise around it) stays on the preview. */
export const FULLRES_EPSILON = 1e-3;

export interface FullresInput {
  /** Live transform scale — screen px per PREVIEW px (logic/zoom.ts). */
  scale: number;
  /** The loaded preview's natural pixel dims. */
  preview: Dims;
  /** CSS px → device px; HiDPI displays exhaust the preview sooner. */
  devicePixelRatio?: number;
}

/** True when the zoom has outrun the preview's pixels. */
export function needsOriginal(input: FullresInput): boolean {
  if (input.preview.w <= 0 || input.preview.h <= 0) return false;
  const dpr = input.devicePixelRatio ?? 1;
  // rendered device px = scale·preview.w·dpr; exceeds preview.w ⇔ scale·dpr > 1
  return input.scale * dpr > 1 + FULLRES_EPSILON;
}
