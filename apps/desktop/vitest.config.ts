import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { svelteTesting } from "@testing-library/svelte/vite";

export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  define: {
    // Tests run with the debug panel compiled OUT, like a release bundle.
    "import.meta.env.PHOTOPROOF_DEBUG": "false",
  },
  test: {
    environment: "jsdom",
    include: ["tests/**/*.test.ts"],
    // This jsdom config has no localStorage; the shim keeps the prefs/panel
    // persistence paths testable.
    setupFiles: ["tests/setup.ts"],
  },
});
