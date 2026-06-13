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
