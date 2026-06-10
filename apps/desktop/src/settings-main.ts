import { mount } from "svelte";
import "./app.css";
import SettingsApp from "./lib/settings/SettingsApp.svelte";

const app = mount(SettingsApp, {
  target: document.getElementById("app")!,
});

export default app;
