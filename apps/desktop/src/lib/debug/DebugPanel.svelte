<script lang="ts">
  /**
   * The debug side panel (UI §10, DECISIONS I6) — dev builds only. This
   * module is excluded from release bundles by the PHOTOPROOF_DEBUG
   * compile-time define (dead dynamic import) and its commands exist only
   * under the `debug-panel` cargo feature.
   *
   * Read-only except the bordered [dev] actions: force flush, force rescan.
   */
  import { invoke } from "@tauri-apps/api/core";
  import { ui } from "../state/app.svelte";

  // Marker asserted absent from release bundles (scripts/assert-release-clean.sh).
  export const DEBUG_PANEL_MARKER = "PP_DEBUG_PANEL_MARKER";

  type Tab = "events" | "capture" | "ingest" | "sidecars" | "runtime" | "search";
  let tab = $state<Tab>("events");
  let payload = $state<unknown>(null);
  let busy = $state(false);

  async function refresh() {
    busy = true;
    try {
      switch (tab) {
        case "events":
          payload = await invoke("debug_tail_events", { limit: 100 });
          break;
        case "capture":
          payload = await invoke("debug_capture");
          break;
        case "ingest":
          payload = await invoke("debug_ingest");
          break;
        case "sidecars":
          payload = await invoke("debug_sidecars");
          break;
        case "runtime":
          payload = await invoke("debug_runtime");
          break;
        case "search":
          payload = await invoke("debug_search");
          break;
      }
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    void tab;
    void refresh();
  });

  async function forceFlush() {
    await invoke("debug_force_flush");
    await refresh();
  }

  async function forceRescan() {
    if (ui.grid.rootId === null) return;
    await invoke("debug_force_rescan", { rootId: ui.grid.rootId });
    await refresh();
  }
</script>

<aside class="debug-panel" aria-label="Debug panel">
  <header>
    {#each ["events", "capture", "ingest", "sidecars", "runtime", "search"] as t (t)}
      <button class:active={tab === t} onclick={() => (tab = t as Tab)}>{t}</button>
    {/each}
    <span class="spacer"></span>
    <button class="dev" onclick={() => void forceFlush()}>[dev] flush</button>
    <button class="dev" onclick={() => void forceRescan()}>[dev] rescan</button>
    <button class="dev" onclick={() => void refresh()} disabled={busy}>↻</button>
  </header>
  <pre>{JSON.stringify(payload, null, 2)}</pre>
</aside>

<style>
  .debug-panel {
    position: fixed;
    top: 28px;
    right: 0;
    bottom: 0;
    width: 480px;
    background: var(--bg-overlay);
    border-left: 1px solid var(--chrome);
    z-index: 70;
    display: flex;
    flex-direction: column;
    font-family: var(--mono);
    font-size: 11px;
  }
  header {
    display: flex;
    gap: 2px;
    padding: 4px;
    border-bottom: 1px solid var(--chrome);
    flex-wrap: wrap;
  }
  header button {
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-family: var(--mono);
    font-size: 11px;
    padding: 2px 6px;
  }
  header button.active {
    color: var(--text);
    background: var(--bg-raised);
  }
  header .dev {
    border: 1px solid var(--chrome-strong);
    border-radius: 3px;
  }
  .spacer {
    flex: 1;
  }
  pre {
    flex: 1;
    overflow: auto;
    margin: 0;
    padding: 8px;
    user-select: text;
    -webkit-user-select: text;
    white-space: pre-wrap;
    word-break: break-all;
  }
</style>
