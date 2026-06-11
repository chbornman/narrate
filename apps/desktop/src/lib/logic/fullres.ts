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
 * THE SOURCE LADDER (dogfood round 2 adds the embedded-native rung):
 * past the threshold, sources are tried in order — /original (the file
 * itself, webview-decodable stored formats only), then /embedded (the
 * full-resolution JPEG most cameras pack into a RAW container, served at
 * NATIVE size with the cached preview's exact display orientation — the
 * backend applies the same §9.3.1 policy, so strokes stay put). When the
 * ladder is exhausted (TIFF/HEIC, small/no embedded preview, offline),
 * the 2560 preview stands silently; true decoded 1:1 stays M1.5.
 *
 * Everything else is owned elsewhere: the protocol owns both allowlists
 * and refuses with uniform 404s; LookStage keeps the preview painted
 * until a full-res source has actually loaded, renders it into the
 * preview's layout box, and derives both from the canonical zoom session —
 * the transform carries over exactly by construction. "Actual" (100%)
 * zoom stays PREVIEW-relative even when a full-res source renders (U12:
 * the session carries exactly); native-pixel 1:1 semantics are the M1.5
 * decoded-loupe question.
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

/** A rung of the progressive source ladder past the display preview. */
export type FullresSource = "original" | "embedded";

/** First rung for a freshly requested image. */
export const FIRST_SOURCE: FullresSource = "original";

/** Next rung after `source` refused (a protocol 404): /original → the
 * embedded-native JPEG (the RAW path); a refused /embedded exhausts the
 * ladder (`null` — the preview stands, never re-asked this session). */
export function nextSource(source: FullresSource): FullresSource | null {
  return source === "original" ? "embedded" : null;
}
