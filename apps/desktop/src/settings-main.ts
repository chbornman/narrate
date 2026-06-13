import { mount } from "svelte";
import "./app.css";
import SettingsApp from "./lib/settings/SettingsApp.svelte";
import { theme } from "./lib/theme/theme-store.svelte";

// The Settings window is its own webview, so it paints the theme itself (the
// same shared store the segmented control writes to). Without this the
// Settings window would ignore the preference and the control would have no
// live chrome to reflect.
theme.init();

const app = mount(SettingsApp, {
  target: document.getElementById("app")!,
});

export default app;
