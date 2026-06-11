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
