/**
 * Content-addressed preview URLs over the photoproof:// custom scheme
 * (UI §3.3, DECISIONS P16): the webview fetches bytes directly from the
 * protocol handler; immutable cache headers let the HTTP cache do the rest.
 * No blob/object URLs, no base64, nothing over IPC.
 *
 * These are hand-built — convertFileSrc is for the built-in asset protocol,
 * not for register_asynchronous_uri_scheme_protocol custom schemes.
 */

export const thumbUrl = (hash: string): string =>
  `photoproof://localhost/thumb/${hash}`;

/**
 * The MICRO preview tier (96 px): the Visualizer graph's tiny node thumbnail.
 * Nodes draw at ~20-132 px, so the 512 px thumb was wildly oversized and
 * decoding hundreds of them stalled the graph's initial open. A derived
 * on-disk-only artifact (preview.rs MICRO_EDGE); it 404s until the
 * generator_version-3 regen has written it, so the graph loader falls back to
 * thumbUrl on error.
 */
export const microUrl = (hash: string): string =>
  `photoproof://localhost/micro/${hash}`;

/**
 * Grid decode tier by DISPLAY size (AUDIT 2026-07-07 F3): the two smallest
 * THUMB_STEPS (96/160 targets) used to decode the 512 px thumb into tiny
 * cells — ~28x the needed pixels held decoded across the mounted window
 * (~150 MB vs ~5 MB). The 96 px micro tier covers them; larger steps keep
 * the thumb. The cutoff sits at the midpoint of the 160/240 steps because
 * the integer-column snap (gridlayout.ts) stretches a 160-target cell up
 * toward ~200 px at ordinary column counts — the tier tracks the ACTUAL
 * rendered size, so a slider step keeps one tier in any real grid. Softness
 * from upscaling micro at the 160 step is the audit's accepted trade: at
 * those sizes the grid reads as an overview, and decode memory wins.
 */
export const MICRO_TIER_MAX_CELL_PX = 200;

export type PreviewTier = "micro" | "thumb";

export const tierForCell = (cellPx: number): PreviewTier =>
  cellPx <= MICRO_TIER_MAX_CELL_PX ? "micro" : "thumb";

/** The grid cell's preview URL for its display size (tierForCell). */
export const previewTierUrl = (hash: string, cellPx: number): string =>
  tierForCell(cellPx) === "micro" ? microUrl(hash) : thumbUrl(hash);

/**
 * The hash a preview URL names — the last path segment, with any
 * cache-busting query stripped ("" for non-URL strings, e.g. an empty
 * `currentSrc`).
 *
 * Why it exists (BACKLOG: recycled <img> pixel flash): the grid
 * virtualizer recycles <img> elements by pool slot, so a Thumb's `hash`
 * prop changes under a LIVE element. Setting `src` only QUEUES the swap
 * (the HTML "update the image data" microtask) — until it runs,
 * `img.complete`/`img.naturalWidth` still describe the PREVIOUS
 * occupant's bitmap, and a load event already in flight for the old src
 * can fire after the prop changed. Every loaded-marking path must prove
 * the element actually holds THIS hash's bitmap (via currentSrc) before
 * unhiding it, or the old pixels flash for a frame on fast scroll.
 */
export const srcHash = (url: string): string => {
  const path = url.split("?")[0];
  return path.slice(path.lastIndexOf("/") + 1);
};

export const displayUrl = (hash: string): string =>
  `photoproof://localhost/display/${hash}`;

/** The ORIGINAL file — Look's progressive full-resolution route. Served
 * only for webview-decodable stored formats (protocol.rs allowlist:
 * jpeg/png/webp); RAW falls through to the embedded rung, TIFF/HEIC and
 * offline answer 404 and Look keeps the preview silently. */
export const originalUrl = (hash: string): string =>
  `photoproof://localhost/original/${hash}`;

/** The RAW's embedded full-resolution JPEG at NATIVE size — the ladder
 * rung between the display preview and (M1.5) decoded 1:1. Extraction is
 * on-demand and applies the preview's exact §9.3.1 orientation policy, so
 * strokes stay put at deep zoom; non-RAW/offline/small-preview sources
 * answer 404 and Look falls back per logic/fullres.ts. */
export const embeddedUrl = (hash: string): string =>
  `photoproof://localhost/embedded/${hash}`;

/** The on-demand full-decode artifact at NATIVE sensor resolution (OD-1):
 * the neutral RAW develop Look's 100%-zoom rung serves, the deepest rung.
 * 404s while the develop is in flight ("developing..."); the frontend
 * enqueues the develop via the requestFullDecode command and retries until
 * the pump's drain writes it. */
export const fullDecodeUrl = (hash: string): string =>
  `photoproof://localhost/full-decode/${hash}`;
