/**
 * The one platform probe for chrome decisions (UI §2.3): macOS keeps
 * native decorations with overlay traffic lights (tauri.macos.conf.json
 * sets `titleBarStyle: Overlay`), so Svelte chrome must hide its own
 * window controls and reserve the lights' left footprint; Windows/Linux
 * stay undecorated and render the custom controls. Also the single
 * source for the ⌘-vs-Ctrl glyph split (KeyHint, tooltip).
 *
 * A function rather than a module-scope constant so vitest can stub
 * navigator per test (jsdom reports neither a Mac platform nor a Mac
 * userAgent by default). navigator.platform is deprecated-but-universal
 * in WKWebView/WebKitGTK; userAgent is the fallback should a webview
 * ever blank it. No plugin-os dependency — this stays a zero-IPC check.
 */
export function isMac(): boolean {
  if (typeof navigator === "undefined") return false;
  return /Mac/.test(navigator.platform) || /Macintosh|Mac OS X/.test(navigator.userAgent);
}
