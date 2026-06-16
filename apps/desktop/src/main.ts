import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";
import { theme } from "./lib/theme/theme-store.svelte";
import { surround } from "./lib/theme/surround-store.svelte";
import { logWebviewCaps } from "./lib/logic/webviewcaps";

// One-time WKWebView capability probe (BACKLOG: "WKWebView capability check").
// Records OffscreenCanvas / Workers / createImageBitmap / WebGL(2) / WebGPU
// support for THIS host webview to the console so a live `tauri dev` run pins
// the verdicts that GATE the visualizer-off-main-thread and off-main-thread
// thumbnail-decode work (see docs/SPIKE-WKWEBVIEW.md). Pure, defensive, cheap.
logWebviewCaps();

// Paint the persisted theme BEFORE the first component mounts so there's no
// dark→light flash, and arm the OS watcher for `system`. Theme is orthogonal
// to the per-image [data-surround] backdrop; both webviews call this.
theme.init();
// Arm the surround store's cross-window listener so a surround mode/level change
// made in the Settings window reaches this window (mirrors theme.init()).
surround.init();

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
