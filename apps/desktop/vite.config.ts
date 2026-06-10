import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { resolve } from "node:path";

// The debug panel (spec/UI.md §10) is excluded from release bundles at build
// time via this compile-time define. Dev server: always on. Production build:
// off unless PHOTOPROOF_DEBUG=1 (a dev *bundle* pairs that with
// `cargo tauri build --features debug-panel`). scripts/assert-release-clean.sh
// asserts the default production bundle carries no debug-panel module.
export default defineConfig(({ command }) => {
  const debugPanel = command === "serve" || process.env.PHOTOPROOF_DEBUG === "1";
  return {
    plugins: [svelte()],
    define: {
      "import.meta.env.PHOTOPROOF_DEBUG": JSON.stringify(debugPanel),
    },
    build: {
      target: "safari15",
      rollupOptions: {
        input: {
          main: resolve(__dirname, "index.html"),
          settings: resolve(__dirname, "settings.html"),
        },
      },
    },
    // Tauri dev server contract (tauri.conf.json devUrl).
    clearScreen: false,
    server: {
      port: 1420,
      strictPort: true,
    },
  };
});
