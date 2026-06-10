/// <reference types="vite/client" />

interface ImportMetaEnv {
  /**
   * Compile-time define (vite.config.ts): true in dev / explicit debug
   * builds, false in release bundles. The debug panel and its IPC calls are
   * dead-code-eliminated when false (spec/UI.md §10.1).
   */
  readonly PHOTOPROOF_DEBUG: boolean;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
