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
 * until a full-res source has actually loaded — PROVABLY: a load event
 * counts only when it carries nonzero natural dims (`loadProvesPixels`
 * below; WKWebView can fire a lying 0×0 "load" for a refused source after
 * an onerror-driven src swap) — renders it into the
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

/** A rung of the progressive source ladder past the display preview.
 * `full-decode` is the DEEPEST rung (OD-1): the neutral RAW develop at native
 * sensor resolution, served once the on-demand develop has run. */
export type FullresSource = "original" | "embedded" | "full-decode";

/** First rung for a freshly requested image. */
export const FIRST_SOURCE: FullresSource = "original";

/** Next rung after `source` refused (a protocol 404): /original → the
 * embedded-native JPEG (the RAW path) → /full-decode (the on-demand neutral
 * develop at native resolution). A refused /full-decode exhausts the ladder
 * (`null` — the preview stands).
 *
 * NOTE: a /full-decode 404 is special — it can mean "developing..." (the
 * develop is in flight) rather than a permanent refusal. LookStage owns that
 * distinction: it requests the develop and RETRIES /full-decode while a
 * develop is pending, rather than advancing past it; only a develop the
 * backend declines (non-RAW, unsupported CFA) exhausts the rung. */
export function nextSource(source: FullresSource): FullresSource | null {
  if (source === "original") return "embedded";
  if (source === "embedded") return "full-decode";
  return null;
}

/** Whether `source` is the on-demand full-decode rung — the one LookStage
 * triggers a develop for and retries while "developing...", rather than
 * advancing past on a 404. */
export function isFullDecodeRung(source: FullresSource): boolean {
  return source === "full-decode";
}

/** THE SWAP GATE: a full-res <img> `load` event proves a usable source
 * only when the element reports nonzero natural dims.
 *
 * WHY (founder bug, June 2026, macOS): WKWebView fires a LOAD event — with
 * naturalWidth/Height 0 — instead of `error` when an <img>'s src is
 * swapped INSIDE its own onerror handler and the new URL then 404s. That
 * is exactly the ladder's original→embedded hop on a RAW: /original
 * refuses (allowlist), onerror advances the rung, and when /embedded also
 * refuses the "load" lied. Trusting it supplanted the painted preview with
 * the webview's broken-image glyph AND wedged the ladder (no error event ⇒
 * no rung advance, no failed-set entry — the embedded rung looked served
 * while serving nothing). Verified empirically against a wry-style custom
 * scheme handler answering an empty-body 404 after an onerror-driven src
 * swap; a fresh (non-swapped) 404 fires `error` as expected.
 *
 * The gate is exact, not heuristic: every route serves raster formats only
 * (webp/jpeg/png), and a raster that decoded has nonzero dims — so zero
 * dims is always a refusal in load's clothing and takes the SAME path as
 * onerror (next rung, or the preview stands). This is what keeps the
 * pinned contract honest: the preview stays painted until a full-res
 * source has ACTUALLY loaded. */
export function loadProvesPixels(natural: Dims): boolean {
  return natural.w > 0 && natural.h > 0;
}
