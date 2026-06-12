<script lang="ts">
  /**
   * Settings (UI §2.4): one modest window, exactly four sections, nothing
   * else in v1 — Watched folders · Microphone · Models · Export.
   * Explicitly absent: appearance, keyboard remapping, per-folder options,
   * cache tuning, telemetry, accounts.
   *
   * M1 renders the degraded RUNTIME contract: Microphone stays hidden until
   * ASR is installed; Models shows the explainer.
   */
  // Lucide (BACKLOG "Adopt Lucide icons"): X for the window close; Unplug
  // for the offline-volume mark (Lucide ships no eject).
  import Unplug from "@lucide/svelte/icons/unplug";
  import X from "@lucide/svelte/icons/x";
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open } from "@tauri-apps/plugin-dialog";
  import * as ipc from "../ipc/commands";
  import { isMac } from "../logic/platform";
  import type { AppSettings, RootDto, RuntimeStatus } from "../types/dto";

  /** Same chrome split as Titlebar.svelte (UI §2.3): on macOS this window
   * is built with native decorations + Overlay traffic lights
   * (open_settings_window in commands/app.rs), so the custom close button
   * is dropped and the drag strip insets past the lights. */
  const mac = isMac();

  /** P7.4 decision 4: an installed embedder row shows running/idle state
   * text. The in-process ort sessions load AFTER install (seconds for DFN5B),
   * so "installed" alone cannot tell a loaded embedder from one still building
   * or whose native load failed — the readiness booleans in the same
   * RuntimeStatus payload carry that. We map the three pinned embedder ids to
   * their role's readiness flag; non-embedder rows return "" (no suffix).
   * Trivially additive (lane spec) — no new DTO surface. The booleans only
   * distinguish ready from not-ready; the build-vs-fail detail lives in the
   * debug panel (debug_lines), out of scope for this row. */
  const TEXT_EMBEDDER_IDS = ["embeddinggemma-300m-q8", "qwen3-embedding-0.6b-int8"];
  const CLIP_EMBEDDER_IDS = ["ViT-H-14-378-quickgelu__dfn5b"];
  function embedderStatus(modelId: string, rt: RuntimeStatus): string {
    if (TEXT_EMBEDDER_IDS.includes(modelId)) {
      return rt.textEmbedderReady ? "running" : "idle (loading)";
    }
    if (CLIP_EMBEDDER_IDS.includes(modelId)) {
      return rt.clipReady ? "running" : "idle (loading)";
    }
    return "";
  }

  let roots = $state<RootDto[]>([]);
  let runtime = $state<RuntimeStatus | null>(null);
  let settings = $state<AppSettings | null>(null);
  let removeWarnFor = $state<string | null>(null);
  let rebuildConfirm = $state(false);
  let exportNote = $state("");
  let busy = $state(false);

  const win = getCurrentWindow();

  onMount(() => {
    void win.setTitle("Settings");
    void refresh();
    // Download progress rides the `runtime-status` channel (the runtime
    // pump emits coalesced snapshots to every webview). The load-time
    // fetch above is a snapshot only — without this subscription the
    // Models section froze at its initial percentages until some action
    // returned fresh status (founder dogfood, June 2026).
    const unlisten = listen<RuntimeStatus>("runtime-status", (e) => {
      runtime = e.payload;
    });
    return () => {
      void unlisten.then((u) => u());
    };
  });

  async function refresh() {
    roots = await ipc.listRoots();
    runtime = await ipc.runtimeStatus();
    settings = await ipc.settingsGet();
  }

  async function addFolder() {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir !== "string") return;
    await ipc.addRoot(dir);
    await refresh();
  }

  async function confirmRemove(rootId: string) {
    await ipc.removeRoot(rootId);
    removeWarnFor = null;
    await refresh();
  }

  /** "Stacked pairs show" (featureset §5 dogfood amendment): persisted by
   * the backend, which emits `settings-changed` — the main window's grid
   * re-pairs live. */
  async function setStackDisplay(display: "jpeg" | "raw") {
    settings = await ipc.setStackDisplay(display);
  }

  async function runExport() {
    const dir = await open({ directory: true, multiple: false, title: "Export destination" });
    if (typeof dir !== "string") return;
    busy = true;
    try {
      const report = await ipc.exportJournal(dir);
      exportNote = `Exported ${report.images} sidecars, ${report.sessions} sessions.`;
      await refresh();
    } finally {
      busy = false;
    }
  }

  async function acceptLicense(modelId: string) {
    runtime = await ipc.runtimeAcceptLicense(modelId);
  }

  async function downloadModel(modelId: string) {
    runtime = await ipc.runtimeDownloadModel(modelId);
  }

  async function removeModel(modelId: string) {
    runtime = await ipc.runtimeRemoveModel(modelId);
  }

  /** §8.1: Failed re-enters Spawning with a fresh budget. */
  async function restartRuntime() {
    runtime = await ipc.runtimeRestart();
  }

  /** §6.1.4: cached + re-detect on demand. */
  async function redetect() {
    runtime = await ipc.runtimeRedetect();
  }

  async function runRebuild() {
    busy = true;
    try {
      const report = await ipc.rebuildIndex();
      exportNote = `Rebuilt from ${report.filesParsed} sidecar files (${report.failures} failures).`;
      rebuildConfirm = false;
    } finally {
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") void win.close();
  }
</script>

<svelte:window onkeydown={onKeydown} />

<main>
  <div class="drag" class:mac data-tauri-drag-region>
    <span data-tauri-drag-region>Settings</span>
    {#if !mac}
      <button class="close" aria-label="Close" onclick={() => void win.close()}
        ><X size={14} /></button
      >
    {/if}
  </div>

  <!-- 1. Watched folders -->
  <section>
    <h2>Watched folders</h2>
    {#each roots as root (root.rootId)}
      <div class="row">
        <span class="name">{root.displayName}</span>
        <span class="state"
          >{#if root.online}online{:else}offline <Unplug size={11} />{/if}</span
        >
        <button class="quiet" onclick={() => (removeWarnFor = root.rootId)}>Remove</button>
      </div>
      {#if removeWarnFor === root.rootId}
        <!-- inline, one sentence — not a modal (UI §2.4) -->
        <div class="inline-warn">
          <span
            >Journals and sidecars are untouched; the images leave the index.</span
          >
          <button onclick={() => void confirmRemove(root.rootId)}>Remove</button>
          <button class="quiet" onclick={() => (removeWarnFor = null)}>Keep</button>
        </div>
      {/if}
    {/each}
    <button onclick={() => void addFolder()}>Add folder…</button>
    <!-- library behavior row (U6 stands: no new section) -->
    <div class="row pref">
      <span class="name">Stacked pairs show</span>
      <select
        aria-label="Stacked pairs show"
        value={settings?.stackDisplay ?? "jpeg"}
        onchange={(e) =>
          void setStackDisplay(e.currentTarget.value === "raw" ? "raw" : "jpeg")}
      >
        <option value="jpeg">JPEG (default)</option>
        <option value="raw">RAW</option>
      </select>
    </div>
  </section>

  <!-- 2. Microphone — hidden until ASR is installed (UI §2.4 / RUNTIME) -->
  {#if runtime?.asrReady}
    <section>
      <h2>Microphone</h2>
      <!-- M2b packet: device picker, level meter, mic-enabled checkbox. -->
    </section>
  {/if}

  <!-- 3. Models (renders RUNTIME's contract: tier, per-model rows with
       resumable progress + license display, restart-runtime — §2.4) -->
  <section>
    <h2>Models</h2>
    {#if runtime !== null}
      <p class="dim">
        Hardware tier: {runtime.tierEffective}
        {#if runtime.tierEffective !== runtime.tierDetected}
          (detected {runtime.tierDetected}, overridden)
        {/if}
        {#if runtime.tierOverriddenAbove}
          — set above detected hardware; models may not fit.
        {/if}
      </p>
      {#if runtime.tierEffective === 0}
        <p class="dim">
          Without models, journaling is fully functional: typed notes, the
          pencil, ratings, and keyword search all work. Voice capture and
          semantic search light up if models are added later.
        </p>
      {/if}
      {#each runtime.models.filter((m) => m.state !== "not-offered") as m (m.id)}
        <div class="row">
          <span class="name">{m.id}</span>
          <span class="state">
            {#if m.state === "downloading"}
              downloading — {Math.floor((m.downloadedBytes / Math.max(m.totalBytes, 1)) * 100)}%
              <!-- Auto-retry of an interrupted transfer: still
                   "downloading", never a terminal "failed" until the
                   retry schedule is exhausted. -->
              {#if m.retryHint !== null}
                <span class="dim">— {m.retryHint}</span>
              {/if}
            {:else if m.state === "unpinned"}
              <!-- B55 fail-closed: no verified pin yet (embedders until
                   spike session 2) — pending, not a failure. -->
              coming in a later build
            {:else}
              {m.state}
              {#if m.state === "installed" && runtime !== null && embedderStatus(m.id, runtime) !== ""}
                — {embedderStatus(m.id, runtime)}
              {/if}
            {/if}
          </span>
          {#if m.state === "not-downloaded" || m.state === "failed"}
            {#if m.acceptanceRequired && !m.accepted}
              <button class="quiet" onclick={() => void acceptLicense(m.id)}>
                Accept license
              </button>
            {:else}
              <button class="quiet" onclick={() => void downloadModel(m.id)}>Download</button>
            {/if}
          {/if}
          {#if m.state === "installed"}
            <button class="quiet" onclick={() => void removeModel(m.id)}>Remove</button>
          {/if}
        </div>
        <div class="row license">
          <a href={m.licenseUrl} target="_blank" rel="noreferrer">{m.licenseName}</a>
          {#if m.accepted}<span class="dim">accepted</span>{/if}
          {#if m.error !== null}<span class="dim">— {m.error}</span>{/if}
        </div>
      {/each}
      <div class="row">
        <button class="quiet" onclick={() => void restartRuntime()}>Restart runtime</button>
        <button class="quiet" onclick={() => void redetect()}>Re-detect hardware</button>
      </div>
    {/if}
  </section>

  <!-- 4. Export -->
  <section>
    <h2>Export</h2>
    <div class="row">
      <button disabled={busy} onclick={() => void runExport()}>Export library journal…</button>
      {#if settings?.lastExportTs}
        <span class="dim">last export {settings.lastExportTs}</span>
      {/if}
    </div>
    <div class="row">
      {#if rebuildConfirm}
        <!-- inline (not modal) confirm (UI §2.4) -->
        <span class="dim">Re-import sidecar truth and rebuild the index?</span>
        <button disabled={busy} onclick={() => void runRebuild()}>Rebuild</button>
        <button class="quiet" onclick={() => (rebuildConfirm = false)}>Cancel</button>
      {:else}
        <button class="quiet" onclick={() => (rebuildConfirm = true)}
          >Rebuild index from sidecars…</button
        >
      {/if}
    </div>
    {#if exportNote !== ""}<p class="dim">{exportNote}</p>{/if}
  </section>
</main>

<style>
  main {
    padding: 0 18px 18px;
    overflow-y: auto;
    height: 100%;
  }
  .drag {
    display: flex;
    justify-content: space-between;
    align-items: center;
    height: 28px;
    margin: 0 -18px;
    padding: 0 6px 0 12px;
    color: var(--text-dim);
    font-size: 12px;
  }
  /* macOS: clear the native traffic lights (same 78px footprint the main
     window's Titlebar reserves). */
  .drag.mac {
    padding-left: 78px;
  }
  .close {
    border: none;
    background: transparent;
    color: var(--text-faint);
    display: inline-flex; /* center the Lucide X svg */
    align-items: center;
    padding: 2px 6px;
  }
  /* The offline mark rides the text line — flex keeps it on the midline. */
  .state {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  section {
    margin-top: 18px;
    padding-top: 10px;
    border-top: 1px solid var(--chrome);
  }
  h2 {
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
    margin: 0 0 10px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 8px;
  }
  .name {
    flex: 1;
  }
  .state,
  .dim {
    color: var(--text-faint);
    font-size: 12px;
  }
  .quiet {
    background: transparent;
    border-color: transparent;
    color: var(--text-dim);
  }
  .inline-warn {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-dim);
    font-size: 12px;
    margin: -2px 0 10px;
  }
  .row.pref {
    margin-top: 12px;
  }
  .row.pref .name {
    flex: initial;
    color: var(--text-dim);
  }
  .row.license {
    margin: -4px 0 10px;
    font-size: 11px;
  }
  .row.license a {
    color: var(--text-faint);
  }
</style>
