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
