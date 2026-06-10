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
  import { onMount } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open } from "@tauri-apps/plugin-dialog";
  import * as ipc from "../ipc/commands";
  import type { AppSettings, RootDto, RuntimeStatus } from "../types/dto";

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
  <div class="drag" data-tauri-drag-region>
    <span data-tauri-drag-region>Settings</span>
    <button class="close" aria-label="Close" onclick={() => void win.close()}>×</button>
  </div>

  <!-- 1. Watched folders -->
  <section>
    <h2>Watched folders</h2>
    {#each roots as root (root.rootId)}
      <div class="row">
        <span class="name">{root.displayName}</span>
        <span class="state">{root.online ? "online" : "offline ⏏"}</span>
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
  </section>

  <!-- 2. Microphone — hidden until ASR is installed (UI §2.4 / RUNTIME) -->
  {#if runtime?.asrReady}
    <section>
      <h2>Microphone</h2>
      <!-- M2b packet: device picker, level meter, mic-enabled checkbox. -->
    </section>
  {/if}

  <!-- 3. Models -->
  <section>
    <h2>Models</h2>
    {#if runtime !== null && runtime.models.length === 0}
      <p class="dim">
        No models installed. Hardware tier: {runtime.hardwareTier ?? "not detected"}.
      </p>
      <p class="dim">
        Without models, journaling is fully functional: typed notes, ratings,
        and keyword search all work. Voice capture and semantic search light
        up if models are added later.
      </p>
    {:else if runtime !== null}
      {#each runtime.models as m (m.name)}
        <div class="row"><span class="name">{m.name}</span><span class="state">{m.state}</span></div>
      {/each}
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
  .close {
    border: none;
    background: transparent;
    color: var(--text-faint);
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
</style>
