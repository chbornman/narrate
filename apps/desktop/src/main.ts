import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";
import { theme } from "./lib/theme/theme-store.svelte";
import { surround } from "./lib/theme/surround-store.svelte";

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
