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
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open, save } from "@tauri-apps/plugin-dialog";
  import * as ipc from "../ipc/commands";
  import { progressDetail, updateRate, type RateState } from "../logic/downloadrate";
  import {
    modelExecutionStatus,
    modelRuntimeError,
    modelRuntimeStatus,
  } from "../logic/modelruntime";
  import { isMac } from "../logic/platform";
  import AckButton from "../primitives/AckButton.svelte";
  import {
    initialSettingsBootState,
    SettingsBootController,
    type SettingsBootState,
  } from "./boot";
  import { theme } from "../theme/theme-store.svelte";
  import { THEME_LABELS, THEME_MODES, type ThemeMode } from "../theme/theme";
  import { surround } from "../theme/surround-store.svelte";
  import {
    SURROUND_LABELS,
    SURROUND_LEVELS,
    SURROUND_MODE_LABELS,
    SURROUND_MODES,
    type SurroundLevel,
    type SurroundMode,
  } from "../theme/surround";
  import type {
    ApplicationHealth,
    ApplicationStateChanged,
    AppSettings,
    OperationReceipt,
    PreviewCacheStatsDto,
    RootDto,
    RuntimeStatus,
    UpdateStatus,
  } from "../types/dto";

  /** Bytes per GB (binary, matching the backend's 20 * 1024^3 default). The
   * 1:1 cache budget is edited in GB but stored/measured in bytes. */
  const BYTES_PER_GB = 1024 * 1024 * 1024;

  /** Human-readable size for the cache readout: GB once past ~1 GB, MB below
   * (the 1:1 cache is large, but an empty/fresh cache should read "0 MB"
   * rather than "0.00 GB"). */
  function formatBytes(bytes: number): string {
    if (bytes >= BYTES_PER_GB) return `${(bytes / BYTES_PER_GB).toFixed(2)} GB`;
    return `${(bytes / (1024 * 1024)).toFixed(0)} MB`;
  }

  function formatLatency(milliseconds: number | null): string {
    if (milliseconds === null) return "-";
    return milliseconds < 10
      ? `${milliseconds.toFixed(2)} ms`
      : `${milliseconds.toFixed(0)} ms`;
  }

  function formatHealthTime(milliseconds: number | null): string {
    if (milliseconds === null) return "";
    return new Date(milliseconds).toLocaleString();
  }

  /** Same chrome split as Titlebar.svelte (UI §2.3): on macOS this window
   * is built with native decorations + Overlay traffic lights
   * (open_settings_window in commands/app.rs), so the custom close button
   * is dropped and the drag strip insets past the lights. */
  const mac = isMac();

  let roots = $state<RootDto[]>([]);
  let runtime = $state<RuntimeStatus | null>(null);
  /** D5: per-model smoothed throughput for the progress detail line, fed
   * one sample per status snapshot. Entries exist only while their row is
   * downloading, so a finished/cancelled model starts fresh next time. */
  let rates = $state<Record<string, RateState>>({});

  /** Every path that lands a fresh RuntimeStatus goes through here so the
   * rate tracker sees each snapshot exactly once, whether it arrived on
   * the runtime-status channel or as a command's return value. */
  function setRuntime(rt: RuntimeStatus | null) {
    // Runtime status can be null before the backend has detected hardware
    // (and the settings template already renders a "detecting" state off a
    // null `runtime`). Guard so the rate tracker never dereferences it.
    if (!rt) {
      rates = {};
      runtime = null;
      return;
    }
    const now = Date.now();
    const next: Record<string, RateState> = {};
    for (const m of rt.models) {
      if (m.state === "downloading") {
        next[m.id] = updateRate(rates[m.id] ?? null, now, m.downloadedBytes);
      }
    }
    rates = next;
    runtime = rt;
  }
  let settings = $state<AppSettings | null>(null);
  let removeWarnFor = $state<string | null>(null);
  let modelRemoveWarnFor = $state<string | null>(null);
  let showOtherModels = $state(false);
  let addFolderPolicy = $state<
    "default" | "process-now" | "preview-only" | "process-later"
  >("default");
  let rebuildConfirm = $state(false);
  // Previews (DESIGN-PREVIEW-POLICY.md): the 1:1 cache readout + budget knob.
  let cacheStats = $state<PreviewCacheStatsDto | null>(null);
  // The budget input edits GB locally; we commit (persist + re-evict) on blur
  // so a multi-keystroke edit is one backend call, not one per digit.
  let budgetGb = $state(20);
  let exportNote = $state("");
  let backupReceipt = $state<OperationReceipt | null>(null);
  let backupConfirmPath = $state<string | null>(null);
  let restoreConfirmPath = $state<string | null>(null);
  let busy = $state(false);
  let boot = $state<SettingsBootState>({ ...initialSettingsBootState });
  let health = $state<ApplicationHealth | null>(null);
  let healthError = $state<string | null>(null);
  let healthCopyNote = $state("");
  let updates = $state<UpdateStatus | null>(null);
  let updateError = $state<string | null>(null);
  let updateBusy = $state(false);
  let updateConfirmVersion = $state<string | null>(null);
  let actionState = $state<{
    label: string | null;
    pending: boolean;
    error: string | null;
  }>({ label: null, pending: false, error: null });
  let retryAction: (() => Promise<void>) | null = null;

  /** One mutation lane for the Settings window. Every product-state action
   * enters here, so rejected `void` handlers become a visible failed state,
   * concurrent clicks cannot overlap, and the exact failed operation can be
   * retried without reloading the window. */
  async function performAction(
    label: string,
    operation: () => Promise<void>,
  ): Promise<void> {
    if (actionState.pending) return;
    retryAction = () => performAction(label, operation);
    actionState = { label, pending: true, error: null };
    try {
      await operation();
      actionState = { label: null, pending: false, error: null };
      retryAction = null;
    } catch (error) {
      actionState = {
        label,
        pending: false,
        error: error instanceof Error ? error.message : String(error),
      };
    }
  }

  function retryFailedAction(): void {
    const retry = retryAction;
    if (retry !== null) void retry();
  }

  function dismissActionError(): void {
    actionState = { label: null, pending: false, error: null };
    retryAction = null;
  }

  const win = getCurrentWindow();
  const bootController = new SettingsBootController(
    {
      roots: ipc.listRoots,
      runtime: ipc.runtimeStatus,
      settings: ipc.settingsGet,
      cache: ipc.previewCacheStats,
    },
    {
      roots: (value) => (roots = value),
      runtime: setRuntime,
      settings: (value) => {
        settings = value ?? null;
        budgetGb = Math.round(
          (value?.previewCacheBudgetBytes ?? 20 * BYTES_PER_GB) / BYTES_PER_GB,
        );
      },
      cache: (value) => (cacheStats = value),
      state: (value) => (boot = value),
    },
  );
  let disposed = false;
  const eventUnlisteners = new Map<string, UnlistenFn>();
  let listenerInstall: Promise<void> | null = null;
  let applicationRevision = 0;
  let eventChannelsHealthy = false;
  const applicationRevisions = {
    settings: 0,
    roots: 0,
    collections: 0,
    topics: 0,
    runtime: 0,
    previewCache: 0,
  };

  async function catchUpApplicationState(): Promise<void> {
    const snapshot = await ipc.applicationStateSnapshot();
    if (snapshot === null) return;
    if (snapshot.revisions.roots > applicationRevisions.roots) {
      applicationRevisions.roots = snapshot.revisions.roots;
      bootController.liveRoots(snapshot.roots);
    }
    if (snapshot.revisions.runtime > applicationRevisions.runtime) {
      applicationRevisions.runtime = snapshot.revisions.runtime;
      bootController.liveRuntime(snapshot.runtime);
    }
    if (snapshot.revisions.settings > applicationRevisions.settings) {
      applicationRevisions.settings = snapshot.revisions.settings;
      bootController.liveSettings(snapshot.settings);
    }
    if (snapshot.revisions.previewCache > applicationRevisions.previewCache) {
      applicationRevisions.previewCache = snapshot.revisions.previewCache;
      bootController.liveCache(snapshot.previewCache);
    }
    // Settings does not render collections, but retaining its revision avoids
    // treating a collections-only change as a future gap.
    applicationRevisions.collections = Math.max(
      applicationRevisions.collections,
      snapshot.revisions.collections,
    );
    applicationRevisions.topics = Math.max(
      applicationRevisions.topics,
      snapshot.revisions.topics,
    );
    applicationRevision = Math.max(applicationRevision, snapshot.revision);
  }

  async function onApplicationStateChanged(
    change: ApplicationStateChanged,
  ): Promise<void> {
    if (change.revision <= applicationRevision) return;
    if (!eventChannelsHealthy) {
      await catchUpApplicationState();
      return;
    }
    if (change.revision !== applicationRevision + 1) {
      await catchUpApplicationState();
      return;
    }
    applicationRevision = change.revision;
    for (const domain of change.domains) {
      if (domain === "preview-cache") {
        applicationRevisions.previewCache = change.revision;
      } else {
        applicationRevisions[domain] = change.revision;
      }
    }
  }

  /** Install every live state channel before/alongside the cold reads. Partial
   * success is retained and Retry installs only the missing channels. This is
   * boot health: a rejected subscription must not leave a healthy-looking
   * Settings window that silently grows stale. */
  function installEventListeners(): Promise<void> {
    if (listenerInstall !== null) return listenerInstall;
    const run = async () => {
      const specs: Array<{
        event: string;
        install: () => Promise<UnlistenFn>;
      }> = [
        {
          event: "runtime-status",
          install: () =>
            listen<RuntimeStatus>("runtime-status", (e) =>
              bootController.liveRuntime(e.payload),
            ),
        },
        {
          event: "roots-changed",
          install: () =>
            listen<RootDto[]>("roots-changed", (e) =>
              bootController.liveRoots(e.payload),
            ),
        },
        {
          event: "settings-changed",
          install: () =>
            listen<AppSettings>("settings-changed", (e) =>
              bootController.liveSettings(e.payload),
            ),
        },
        {
          event: "preview-cache-changed",
          install: () =>
            listen<PreviewCacheStatsDto>("preview-cache-changed", (e) =>
              bootController.liveCache(e.payload),
            ),
        },
        {
          event: "application-state-changed",
          install: () =>
            listen<ApplicationStateChanged>(
              "application-state-changed",
              (e) => {
                void onApplicationStateChanged(e.payload).catch((error) => {
                  eventChannelsHealthy = false;
                  bootController.listenersFailed(error);
                });
              },
            ),
        },
      ];
      const missing = specs.filter(({ event }) => !eventUnlisteners.has(event));
      const results = await Promise.allSettled(
        missing.map(async ({ event, install }) => {
          const unlisten = await install();
          if (disposed) {
            unlisten();
            return;
          }
          eventUnlisteners.set(event, unlisten);
        }),
      );
      const failures = results.flatMap((result, index) =>
        result.status === "rejected"
          ? [`${missing[index]?.event ?? "unknown"}: ${
              result.reason instanceof Error
                ? result.reason.message
                : String(result.reason)
            }`]
          : [],
      );
      if (failures.length === 0 && eventUnlisteners.size === specs.length) {
        try {
          await catchUpApplicationState();
          eventChannelsHealthy = true;
          bootController.listenersReady();
        } catch (error) {
          eventChannelsHealthy = false;
          bootController.listenersFailed(error);
        }
      } else {
        eventChannelsHealthy = false;
        bootController.listenersFailed(
          new Error(failures.join("; ") || "live update subscription unavailable"),
        );
      }
    };
    listenerInstall = run().finally(() => {
      listenerInstall = null;
    });
    return listenerInstall;
  }

  onMount(() => {
    const bootStarted = performance.now();
    void win.setTitle("Settings").catch((error) => {
      console.warn("could not set Settings window title", error);
    });
    void Promise.allSettled([
      refresh(),
      refreshHealth(),
      refreshUpdateStatus(),
      refreshBackupReceipt(),
    ]).then((results) => {
      requestAnimationFrame(() => {
        ipc.recordPerformance(
          "settings",
          "first-paint",
          performance.now() - bootStarted,
          results.every((result) => result.status === "fulfilled"),
        );
      });
    });
    return () => {
      disposed = true;
      for (const unlisten of eventUnlisteners.values()) unlisten();
      eventUnlisteners.clear();
      void ipc.flushPerformance();
    };
  });

  async function refresh(): Promise<void> {
    // Start listener installation first so a mutation between subscription
    // and the cold responses is delivered and wins generation arbitration.
    await Promise.all([installEventListeners(), bootController.refresh()]);
  }

  async function refreshHealth(): Promise<void> {
    try {
      health = await ipc.applicationHealth();
      healthError = null;
    } catch (error) {
      healthError = error instanceof Error ? error.message : String(error);
    }
  }

  async function refreshBackupReceipt(): Promise<void> {
    backupReceipt = await ipc.backupOperationStatus();
  }

  async function copyHealthReport(): Promise<void> {
    const snapshot = await ipc.applicationHealth();
    await navigator.clipboard.writeText(JSON.stringify(snapshot, null, 2));
    health = snapshot;
    healthError = null;
    healthCopyNote = "Copied";
  }

  /** Interpret the backend's closed health-action vocabulary through the same
   * serialized mutation lane as every other Settings action. Refreshing the
   * authoritative snapshot after completion proves whether recovery landed. */
  async function runHealthAction(issue: ApplicationHealth["issues"][number]) {
    await performAction(issue.action.label, async () => {
      const targetId = issue.action.targetId;
      switch (issue.action.kind) {
        case "retry-root":
          if (targetId === null) throw new Error("Folder recovery target is missing.");
          await ipc.rescanRoot(targetId);
          break;
        case "retry-roots":
          await ipc.recoverRoots();
          break;
        case "retry-runtime":
          bootController.liveRuntime(await ipc.runtimeRestart());
          break;
        case "retry-repair":
          await ipc.retryIntegrityRepair();
          break;
        case "redetect-runtime":
          bootController.liveRuntime(await ipc.runtimeRedetect());
          break;
        case "verify-model":
          if (targetId === null) throw new Error("Model verification target is missing.");
          bootController.liveRuntime(await ipc.runtimeVerifyModel(targetId));
          break;
        case "rebuild-previews":
          if (targetId !== null) {
            await ipc.rebuildPreviews(targetId);
          } else {
            for (const root of roots) await ipc.rebuildPreviews(root.rootId);
          }
          break;
        case "reveal-logs":
          await ipc.revealLogs();
          break;
        case "restore-controls":
          if (
            targetId !== "settings" &&
            targetId !== "config" &&
            targetId !== "tuning"
          ) {
            throw new Error("Control recovery target is missing.");
          }
          await ipc.restoreControlDefaults(targetId);
          break;
      }
      health = await ipc.applicationHealth();
      healthError = null;
    });
  }

  async function refreshUpdateStatus(): Promise<void> {
    try {
      updates = await ipc.updateStatus();
      updateError = null;
    } catch (error) {
      updateError = error instanceof Error ? error.message : String(error);
    }
  }

  async function checkForUpdate(): Promise<void> {
    updateBusy = true;
    updateConfirmVersion = null;
    try {
      updates = await ipc.updateCheck();
      updateError = null;
    } catch (error) {
      updateError = error instanceof Error ? error.message : String(error);
      await refreshUpdateStatus();
    } finally {
      updateBusy = false;
    }
  }

  async function installUpdate(version: string): Promise<void> {
    updateBusy = true;
    updateError = null;
    try {
      await ipc.updateInstall(version);
    } catch (error) {
      updateError = error instanceof Error ? error.message : String(error);
      updateBusy = false;
      await refreshUpdateStatus();
    }
  }

  /** Settings → Previews: commit the edited GB budget (clamped to a sane
   * minimum so a 0 cannot wipe the cache by typo). Persists in bytes and
   * re-evicts immediately, then refreshes the readout. */
  async function commitBudget() {
    await performAction("Saving preview cache budget", async () => {
      const gb = Math.max(1, Math.floor(budgetGb || 0));
      budgetGb = gb;
      bootController.liveSettings(
        await ipc.setPreviewCacheBudget(gb * BYTES_PER_GB),
      );
      bootController.liveCache(await ipc.previewCacheStats());
    });
  }

  /** Settings → Previews "Clear 1:1 cache" / "Clear all previews": SAFE (every
   * removed artifact re-derives on next view). Refresh the readout after. */
  async function clearCache(kind: "full" | "all") {
    await performAction(
      kind === "full" ? "Clearing 1:1 previews" : "Rebuilding previews",
      async () => {
        await ipc.clearPreviewCache(kind);
        bootController.liveCache(await ipc.previewCacheStats());
      },
    );
  }

  async function addFolder() {
    await performAction("Adding folder", async () => {
      const dir = await open({ directory: true, multiple: false });
      if (typeof dir !== "string") return;
      await ipc.addRoot(
        dir,
        addFolderPolicy === "default" ? undefined : addFolderPolicy,
      );
      addFolderPolicy = "default";
      await refresh();
    });
  }

  async function confirmRemove(rootId: string) {
    await performAction("Removing folder", async () => {
      await ipc.removeRoot(rootId);
      removeWarnFor = null;
      await refresh();
    });
  }

  /** "Stacked pairs show" (featureset §5 dogfood amendment): persisted by
   * the backend, which emits `settings-changed` — the main window's grid
   * re-pairs live. */
  async function setStackDisplay(display: "jpeg" | "raw") {
    await performAction("Saving stacked-pair display", async () => {
      bootController.liveSettings(await ipc.setStackDisplay(display));
    });
  }

  async function setProcessingPolicy(next: {
    intensity?: "eco" | "balanced" | "max";
    paused?: boolean;
    newRootPolicy?: "process-now" | "preview-only" | "process-later";
    deferTextEmbeddings?: boolean;
    deferImageEmbeddings?: boolean;
  }) {
    await performAction("Saving processing policy", async () => {
      const current = settings;
      if (!current) return;
      bootController.liveSettings(
        await ipc.setProcessingPolicy(
          next.intensity ?? current.processingIntensity ?? "balanced",
          next.paused ?? current.processingPaused ?? false,
          next.newRootPolicy ?? current.newRootPolicy ?? "process-now",
          next.deferTextEmbeddings ?? current.deferTextEmbeddings ?? false,
          next.deferImageEmbeddings ?? current.deferImageEmbeddings ?? false,
        ),
      );
    });
  }

  /** "Open in external editor" target (BACKLOG "Configurable external
   * editor, D4 revisit"): the backend trims and treats empty as the OS
   * default handler, then emits `settings-changed`. */
  async function setExternalEditor(editor: string) {
    await performAction("Saving external editor", async () => {
      bootController.liveSettings(await ipc.setExternalEditor(editor));
    });
  }

  async function runExport() {
    await performAction("Exporting library journal", async () => {
      const dir = await open({
        directory: true,
        multiple: false,
        title: "Export destination",
      });
      if (typeof dir !== "string") return;
      busy = true;
      try {
        const report = await ipc.exportJournal(dir);
        exportNote = `Exported ${report.images} sidecars, ${report.sessions} sessions.`;
        await refresh();
      } finally {
        busy = false;
      }
    });
  }

  async function chooseFullBackup() {
    const destination = await save({
      title: "Save complete Photoproof backup",
      defaultPath: `Photoproof Backup ${new Date().toISOString().slice(0, 10)}.ppbackup`,
    });
    if (typeof destination === "string") {
      restoreConfirmPath = null;
      backupConfirmPath = destination;
    }
  }

  async function runFullBackup() {
    const destination = backupConfirmPath;
    if (destination === null) return;
    await performAction("Preparing full backup and quit", async () => {
      // This command only arms the helper. The copy begins after the process
      // exits and the inherited safety pipe reaches EOF.
      await ipc.backupAndQuit(destination);
    });
  }

  async function chooseFullRestore() {
    const backup = await open({
      directory: true,
      multiple: false,
      title: "Choose a Photoproof .ppbackup folder",
    });
    if (typeof backup === "string") {
      backupConfirmPath = null;
      restoreConfirmPath = backup;
    }
  }

  async function runFullRestore() {
    const backup = restoreConfirmPath;
    if (backup === null) return;
    await performAction("Verifying full backup and restarting", async () => {
      await ipc.restoreAndRestart(backup);
    });
  }

  async function importSavedTopics() {
    await performAction("Importing saved topics", async () => {
      const path = await open({
        directory: false,
        multiple: false,
        title: "Choose topics.photoproof.json",
        filters: [{ name: "Photoproof topics", extensions: ["json"] }],
      });
      if (typeof path !== "string") return;
      const imported = await ipc.importTopics(path);
      exportNote =
        imported === 0
          ? "Saved topics were already up to date."
          : `Imported ${imported} saved topic and note records.`;
    });
  }

  async function acceptLicense(modelId: string) {
    await performAction("Accepting model license", async () => {
      bootController.liveRuntime(await ipc.runtimeAcceptLicense(modelId));
    });
  }

  async function downloadModel(modelId: string) {
    await performAction("Starting model download", async () => {
      bootController.liveRuntime(await ipc.runtimeDownloadModel(modelId));
    });
  }

  /** D3: stop a transfer; part files are kept so Download later resumes. */
  async function cancelDownload(modelId: string) {
    await performAction("Cancelling model download", async () => {
      bootController.liveRuntime(await ipc.runtimeCancelDownload(modelId));
    });
  }

  async function removeModel(modelId: string) {
    await performAction("Removing model", async () => {
      bootController.liveRuntime(await ipc.runtimeRemoveModel(modelId));
    });
  }

  async function confirmRemoveModel(modelId: string) {
    modelRemoveWarnFor = null;
    await removeModel(modelId);
  }

  async function verifyModel(modelId: string) {
    await performAction("Verifying model", async () => {
      bootController.liveRuntime(await ipc.runtimeVerifyModel(modelId));
    });
  }

  async function discardPartial(modelId: string) {
    await performAction("Discarding partial download", async () => {
      bootController.liveRuntime(await ipc.runtimeDiscardPartial(modelId));
    });
  }

  /** §8.1: Failed re-enters Spawning with a fresh budget. */
  async function restartRuntime() {
    await performAction("Restarting model runtime", async () => {
      bootController.liveRuntime(await ipc.runtimeRestart());
    });
  }

  /** §6.1.4: cached + re-detect on demand. */
  async function redetect() {
    await performAction("Detecting hardware", async () => {
      bootController.liveRuntime(await ipc.runtimeRedetect());
    });
  }

  async function runRebuild() {
    await performAction("Rebuilding the library", async () => {
      busy = true;
      try {
        const report = await ipc.rebuildIndex();
        exportNote = `Rebuilt from ${report.filesParsed} sidecar files (${report.failures} failures).`;
        rebuildConfirm = false;
      } finally {
        busy = false;
      }
    });
  }

  // Attention/engagement heatmap (DESIGN-ATTENTION-HEATMAP.md): the privacy-
  // hygiene reset. Inline confirm (not a modal, UI §2.4); the AckButton says
  // it landed even when the count is the only thing that changed.
  let clearAttentionConfirm = $state(false);
  async function clearAttention(): Promise<void> {
    await performAction("Clearing attention history", async () => {
      await ipc.clearDwell();
      clearAttentionConfirm = false;
    });
  }

  /** Appearance theme (BACKLOG "Full interface themes"): writes the shared
   * theme store, which persists the pref and repaints data-theme live in
   * every webview. `system` follows the OS; light/dark are explicit. */
  function setTheme(mode: ThemeMode) {
    theme.set(mode);
  }

  /** Background surround (D6) lives beside the theme: follow-theme derives the
   * image backdrop from the active light/dark theme; manual pins a level. Both
   * write the shared surround store, so the main window's data-surround follows
   * (and picking a level here flips the store to manual, exactly like the
   * backdrop right-click). */
  function setSurroundMode(mode: SurroundMode) {
    surround.setMode(mode);
  }

  function setSurroundLevel(level: SurroundLevel) {
    surround.pick(level);
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

  {#if boot.phase === "loading" && !boot.hasSnapshot}
    <div class="boot-state" role="status" aria-live="polite">
      {boot.attempt > 1 ? "Retrying settings..." : "Loading settings..."}
    </div>
  {:else if boot.phase === "fatal"}
    <div class="boot-state error" role="alert">
      <strong>Settings could not be loaded.</strong>
      <span>Your library has not been changed.</span>
      <button onclick={() => void refresh()}>Retry</button>
      {#each boot.issues as issue (issue.source)}
        <span class="issue">{issue.source}: {issue.message}</span>
      {/each}
    </div>
  {:else if boot.phase === "degraded"}
    <div class="boot-state warning" role="alert">
      <strong>Some settings are temporarily unavailable.</strong>
      <span>The sections that loaded are still usable.</span>
      <button class="quiet" onclick={() => void refresh()}>Retry unavailable sections</button>
      {#each boot.issues as issue (issue.source)}
        <span class="issue">{issue.source}: {issue.message}</span>
      {/each}
    </div>
  {:else if boot.phase === "loading"}
    <div class="boot-state" role="status" aria-live="polite">Refreshing settings...</div>
  {/if}

  {#if actionState.pending}
    <div class="action-state" role="status" aria-live="polite">
      <span>{actionState.label}...</span>
    </div>
  {:else if actionState.error !== null}
    <div class="action-state error" role="alert">
      <span>
        {actionState.label ?? "Settings action"} failed: {actionState.error}
      </span>
      <button type="button" onclick={retryFailedAction}>Retry</button>
      <button type="button" class="quiet" onclick={dismissActionError}>Dismiss</button>
    </div>
  {/if}

  <div
    class="settings-body"
    class:blocked={boot.phase === "fatal" || (boot.phase === "loading" && !boot.hasSnapshot)}
    aria-busy={actionState.pending}
    inert={actionState.pending ||
      boot.phase === "fatal" ||
      (boot.phase === "loading" && !boot.hasSnapshot)}
  >
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
    <div class="row pref">
      <button onclick={() => void addFolder()}>Add folder…</button>
      <select bind:value={addFolderPolicy} aria-label="Processing for next folder">
        <option value="default">Use saved default</option>
        <option value="process-now">Process now</option>
        <option value="preview-only">Previews only</option>
        <option value="process-later">Process later</option>
      </select>
    </div>
    <div class="row pref">
      <span class="name">Processing intensity</span>
      <select
        aria-label="Processing intensity"
        value={settings?.processingIntensity ?? "balanced"}
        onchange={(e) =>
          void setProcessingPolicy({
            intensity:
              e.currentTarget.value === "eco"
                ? "eco"
                : e.currentTarget.value === "max"
                  ? "max"
                  : "balanced",
          })}
      >
        <option value="eco">Eco</option>
        <option value="balanced">Balanced</option>
        <option value="max">Max</option>
      </select>
    </div>
    <div class="row pref">
      <span class="name">
        {settings?.processingPaused ? "Background processing paused" : "Background processing"}
      </span>
      <button
        class:active={settings?.processingPaused}
        onclick={() =>
          void setProcessingPolicy({ paused: !(settings?.processingPaused ?? false) })}
      >
        {settings?.processingPaused ? "Resume" : "Pause"}
      </button>
    </div>
    <div class="row pref">
      <span class="name">When adding a folder</span>
      <select
        aria-label="When adding a folder"
        value={settings?.newRootPolicy ?? "process-now"}
        onchange={(e) =>
          void setProcessingPolicy({
            newRootPolicy:
              e.currentTarget.value === "process-later"
                ? "process-later"
                : e.currentTarget.value === "preview-only"
                  ? "preview-only"
                : "process-now",
          })}
      >
        <option value="process-now">Process now</option>
        <option value="preview-only">Previews only</option>
        <option value="process-later">Process later</option>
      </select>
    </div>
    <div class="row pref">
      <span class="name">Text embeddings</span>
      <button
        class:active={settings?.deferTextEmbeddings}
        onclick={() =>
          void setProcessingPolicy({
            deferTextEmbeddings: !(settings?.deferTextEmbeddings ?? false),
          })}
      >
        {settings?.deferTextEmbeddings ? "Deferred" : "Enabled"}
      </button>
    </div>
    <div class="row pref">
      <span class="name">Image embeddings</span>
      <button
        class:active={settings?.deferImageEmbeddings}
        onclick={() =>
          void setProcessingPolicy({
            deferImageEmbeddings: !(settings?.deferImageEmbeddings ?? false),
          })}
      >
        {settings?.deferImageEmbeddings ? "Deferred" : "Enabled"}
      </button>
    </div>
    <p class="helper">
      Eco keeps one expensive lane active; Balanced allows two; Max uses up to
      four. Pause leaves explicit 1:1 photo development available. “Process
      later” registers and watches a new folder without walking its full tree.
      “Previews only” indexes metadata and previews while model passes remain
      pending. Embedding switches resume the same durable pass rows when enabled.
    </p>
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
    <!-- Configurable external editor (BACKLOG, D4 revisit): hand the
         original off for real editing (review here, edit there). -->
    <div class="row pref">
      <span class="name">Open in external editor</span>
      <input
        type="text"
        class="editor-input"
        aria-label="External editor"
        placeholder="System default"
        value={settings?.externalEditor ?? ""}
        onchange={(e) => void setExternalEditor(e.currentTarget.value)}
      />
    </div>
    <p class="helper">
      The app to open an image's original in. Leave blank to use the system
      default. On macOS use the application name (for example, Affinity Photo).
    </p>
  </section>

  <!-- 1b. Appearance (BACKLOG "Full interface themes"): the chrome theme.
       Segmented System / Light / Dark; the live chrome reflects it at once
       because this window paints data-theme from the same store. -->
  <section>
    <h2>Appearance</h2>
    <div class="row pref">
      <span class="name">Theme</span>
      <div class="segmented" role="radiogroup" aria-label="Theme">
        {#each THEME_MODES as mode (mode)}
          <button
            type="button"
            role="radio"
            aria-checked={theme.mode === mode}
            class:active={theme.mode === mode}
            onclick={() => setTheme(mode)}>{THEME_LABELS[mode]}</button
          >
        {/each}
      </div>
    </div>
    <p class="helper">
      System follows your operating system's light or dark setting.
    </p>
    <!-- Background surround (D6): the backdrop behind the photo. Follow theme
         derives it from the active light/dark theme; Manual pins a level. -->
    <div class="row pref">
      <span class="name">Background surround</span>
      <div class="segmented" role="radiogroup" aria-label="Background surround">
        {#each SURROUND_MODES as mode (mode)}
          <button
            type="button"
            role="radio"
            aria-checked={surround.mode === mode}
            class:active={surround.mode === mode}
            onclick={() => setSurroundMode(mode)}>{SURROUND_MODE_LABELS[mode]}</button
          >
        {/each}
      </div>
    </div>
    {#if surround.mode === "manual"}
      <div class="row pref">
        <span class="name">Surround level</span>
        <div class="segmented" role="radiogroup" aria-label="Surround level">
          {#each SURROUND_LEVELS as level (level)}
            <button
              type="button"
              role="radio"
              aria-checked={surround.manualLevel === level}
              class:active={surround.manualLevel === level}
              onclick={() => setSurroundLevel(level)}>{SURROUND_LABELS[level]}</button
            >
          {/each}
        </div>
      </div>
    {/if}
    <p class="helper">
      Follow theme keeps the photo backdrop matched to the light or dark theme.
      Manual pins a specific shade.
    </p>
  </section>

  <!-- 1c. Previews (DESIGN-PREVIEW-POLICY.md): the one knob for the on-disk
       cost of full-res 1:1 develops. Thumbnails and display previews are
       small and always kept; only the big 1:1 artifacts get a budget. No
       em-dashes in copy (gate: check:emdash). -->
  <section>
    <h2>Previews</h2>
    <div class="row pref">
      <span class="name">1:1 cache budget</span>
      <input
        type="number"
        class="budget-input"
        aria-label="1:1 preview cache budget in gigabytes"
        min="1"
        step="1"
        bind:value={budgetGb}
        onblur={() => void commitBudget()}
      />
      <span class="dim">GB</span>
    </div>
    <p class="helper">
      Full resolution previews build as you zoom in and are cached on disk.
      They are kept until the cache passes this size, then the
      least-recently-viewed ones are removed. Removing them is always safe:
      each one rebuilds the next time you view it, and your marks are never
      stored in a preview.
    </p>
    <p class="helper">
      Clear 1:1 cache reclaims just the large full-resolution files; they
      redevelop on next view. Rebuild all previews clears every cached preview
      and regenerates them in the background, so thumbnails repopulate as they
      finish.
    </p>
    {#if cacheStats !== null}
      <p class="dim">
        1:1 cache: {formatBytes(cacheStats.fullBytes)} in {cacheStats.fullFiles}
        {cacheStats.fullFiles === 1 ? "file" : "files"} (budget {formatBytes(
          cacheStats.budgetBytes,
        )}). All previews on disk: {formatBytes(cacheStats.totalBytes)}.
      </p>
    {/if}
    <div class="row">
      <AckButton
        quiet
        label="Clear 1:1 cache"
        doneLabel="Cleared"
        verb={() => clearCache("full")}
      />
      <AckButton
        quiet
        label="Rebuild all previews"
        doneLabel="Rebuilding"
        verb={() => clearCache("all")}
      />
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
        Hardware tier:
        {#if runtime.capabilityState === "detecting"}
          detecting
        {:else if runtime.capabilityState === "provisional"}
          {runtime.tierEffective} (provisional)
        {:else}
          {runtime.tierEffective}
        {/if}
        {#if runtime.tierEffective !== runtime.tierDetected}
          (detected {runtime.tierDetected}, overridden)
        {/if}
        {#if runtime.tierOverriddenAbove}
          - set above detected hardware; models may not fit.
        {/if}
      </p>
      {#if runtime.capabilityState === "failed"}
        <p class="dim">
          Hardware detection failed. The provisional safe configuration remains
          active. {runtime.capabilitySummary ?? ""}
        </p>
      {:else if runtime.capabilityState === "detecting"}
        <p class="dim">Checking adapters and available hardware backends...</p>
      {:else if runtime.capabilityAdapters.length > 0}
        <p class="dim">
          {runtime.capabilityAdapters
            .map((adapter) => `${adapter.name} (${adapter.backend})`)
            .join(", ")}
        </p>
      {/if}
      {#if runtime.capabilities}
        <p class="dim">
          ONNX Runtime:
          {runtime.capabilities.providers
            .filter((provider) => provider.compiled)
            .map(
              (provider) =>
                `${provider.provider} ${provider.runtimeAvailable === true ? "available" : provider.runtimeAvailable === false ? "unavailable" : "unknown"}`,
            )
            .join(", ")}
        </p>
      {/if}
      {#if runtime.tierEffective === 0}
        <p class="dim">
          Without models, journaling is fully functional: typed notes, the
          pencil, ratings, and keyword search all work. Voice capture and
          semantic search light up if models are added later.
        </p>
      {/if}
      <!-- Plan-says-run-but-binary-missing (the June 2026 silent-dark
           incident): one muted line per blocked process — without it the
           rows below read "installed" while voice/LLM stay dead with no
           explanation anywhere. -->
      {#if runtime.asrBlocked !== null}
        <p class="dim">Voice is unavailable: {runtime.asrBlocked}</p>
      {/if}
      {#if runtime.llmBlocked !== null}
        <p class="dim">Local LLM is unavailable: {runtime.llmBlocked}</p>
      {/if}
      <button class="quiet" onclick={() => (showOtherModels = !showOtherModels)}>
        {showOtherModels ? "Hide other models" : "Show other models"}
      </button>
      {#each runtime.models.filter(
        (m) =>
          m.defaultOffer ||
          showOtherModels ||
          m.state === "installed" ||
          m.operation !== null,
      ) as m (m.id)}
        <div class="row">
          <span class="name">
            {m.id}
            <span class="dim">
              {m.defaultOffer
                ? "recommended"
                : m.advancedAvailable
                  ? "other compatible model"
                  : "unavailable on this machine"}
            </span>
          </span>
          <span class="state">
            {#if m.state === "downloading" || m.operation === "downloading"}
              downloading - {Math.floor((m.downloadedBytes / Math.max(m.totalBytes, 1)) * 100)}%
              <!-- D5: bytes + smoothed throughput beside the percent, from
                   the same snapshots the percent already rides. -->
              <span class="dim"
                >{progressDetail(
                  m.downloadedBytes,
                  m.totalBytes,
                  rates[m.id]?.bytesPerSec ?? null,
                )}</span
              >
              <!-- Auto-retry of an interrupted transfer: still
                   "downloading", never a terminal "failed" until the
                   retry schedule is exhausted. -->
              {#if m.retryHint !== null}
                <span class="dim">- {m.retryHint}</span>
              {/if}
            {:else if m.operation !== null}
              {m.operation}
            {:else if m.state === "unpinned"}
              <!-- B55 fail-closed: no verified pin yet (embedders until
                   spike session 2) — pending, not a failure. -->
              coming in a later build
            {:else if m.state === "not-offered"}
              unavailable - {m.compatibilityReason}
            {:else}
              {m.state}
              {#if m.state === "installed" && modelRuntimeStatus(m) !== ""}
                - {modelRuntimeStatus(m)}
                {#if modelExecutionStatus(m) !== ""}
                  <span class="dim">- {modelExecutionStatus(m)}</span>
                {/if}
                {#if modelRuntimeError(m) !== ""}
                  <span class="dim">- {modelRuntimeError(m)}</span>
                {/if}
              {/if}
            {/if}
          </span>
          {#if m.state === "not-downloaded" || m.state === "failed" || m.state === "cancelled"}
            {#if m.acceptanceRequired && !m.accepted}
              <button class="quiet" onclick={() => void acceptLicense(m.id)}>
                Accept license
              </button>
            {:else}
              <button class="quiet" onclick={() => void downloadModel(m.id)}>Download</button>
            {/if}
          {/if}
          {#if m.state === "queued" || m.state === "downloading" || m.state === "verifying" || m.state === "installing"}
            <!-- D3: cancel keeps the part files, so a later Download
                 resumes from where this stopped. -->
            <button class="quiet" onclick={() => void cancelDownload(m.id)}>Cancel</button>
          {/if}
          {#if m.state === "installed"}
            <button class="quiet" onclick={() => void verifyModel(m.id)}>Verify</button>
            <button class="quiet" onclick={() => (modelRemoveWarnFor = m.id)}>Remove</button>
            {#if modelRemoveWarnFor === m.id}
              <div class="inline-warn">
                <span>The model is unloaded before its files are removed.</span>
                <button onclick={() => void confirmRemoveModel(m.id)}>Remove</button>
                <button class="quiet" onclick={() => (modelRemoveWarnFor = null)}>Keep</button>
              </div>
            {/if}
          {/if}
          {#if m.state !== "installed" && m.operation === null && m.downloadedBytes > 0}
            <button class="quiet" onclick={() => void verifyModel(m.id)}>Verify</button>
            <button class="quiet" onclick={() => void discardPartial(m.id)}>
              Discard partial
            </button>
          {/if}
        </div>
        <div class="row license">
          <a href={m.licenseUrl} target="_blank" rel="noreferrer">{m.licenseName}</a>
          {#if m.accepted}<span class="dim">accepted</span>{/if}
          {#if m.state !== "installed" && m.downloadedBytes > 0}
            <span class="dim">- {formatBytes(m.downloadedBytes)} on disk</span>
          {/if}
          {#if m.error !== null}<span class="dim">- {m.error}</span>{/if}
          {#if m.registryError !== null}<span class="dim">- {m.registryError}</span>{/if}
          {#if m.compatibleProviders.length > 0}
            <span class="dim">- compatible: {m.compatibleProviders.join(", ")}</span>
          {/if}
        </div>
      {/each}
      <!-- Both verbs complete invisibly when the status text happens not
           to change (founder dogfood, June 2026) — the AckButton makes the
           button itself say it landed. -->
      <div class="row">
        <AckButton
          quiet
          label="Restart runtime"
          doneLabel="Restarted"
          verb={restartRuntime}
        />
        <AckButton
          quiet
          label="Re-detect hardware"
          doneLabel="Re-detected"
          verb={redetect}
        />
      </div>
    {/if}
  </section>

  <section>
    <h2>Application health</h2>
    {#if health !== null}
      <p class="dim">
        Phase: {health.phase}. Build {health.diagnostics.buildVersion}.
        {#if health.diagnostics.previousUncleanLaunch}
          The previous launch did not complete a clean shutdown.
        {/if}
      </p>
      {#each health.issues as issue (issue.id)}
        <div
          class="row health-row"
          class:health-blocking={issue.blocking}
          data-health-issue={issue.id}
        >
          <span class="name">{issue.title}</span>
          <span class="state">{issue.blocking ? "blocking" : "degraded"}</span>
          <span class="dim">{issue.summary}</span>
          {#if issue.lastErrorAtMs !== null}
            <span class="dim">Last failure: {formatHealthTime(issue.lastErrorAtMs)}</span>
          {/if}
          <button
            class="quiet"
            type="button"
            disabled={actionState.pending}
            onclick={() => void runHealthAction(issue)}
          >
            {issue.action.label}
          </button>
        </div>
      {/each}
      {#if health.issues.length === 0}
        <p class="dim">No degraded subsystems are currently reported.</p>
      {/if}
      {#if health.diagnostics.error !== null}
        <p class="dim">Diagnostics: {health.diagnostics.error}</p>
      {/if}
      {#if health.performance !== undefined}
        <details>
          <summary>Performance baselines</summary>
          <p class="dim">
            {health.performance.journeys.retainedSamples.toLocaleString()} recent journey
            samples. Local JSONL: {health.performance.journeys.logPath}
          </p>
          {#if health.performance.journeys.sinkError !== null}
            <p class="dim">
              Performance log unavailable: {health.performance.journeys.sinkError}
            </p>
          {/if}
          {#each health.performance.journeys.series as series (`${series.source}:${series.journey}:${series.phase}`)}
            <div class="row health-row">
              <span class="name">{series.journey} / {series.phase}</span>
              <span class="state">p95 {formatLatency(series.p95Ms)}</span>
              <span class="dim">
                p50 {formatLatency(series.p50Ms)}, p99 {formatLatency(series.p99Ms)},
                max {formatLatency(series.maxMs)}, n={series.count.toLocaleString()}
                {series.errors > 0 ? `, ${series.errors} failed` : ""}
              </span>
            </div>
          {/each}
          {#each health.performance.ingestStages.filter((stage) => stage.count > 0) as stage (stage.stage)}
            <div class="row health-row">
              <span class="name">preview pipeline / {stage.stage}</span>
              <span class="state">p95 {formatLatency(stage.p95Ms)}</span>
              <span class="dim">
                p50 {formatLatency(stage.p50Ms)}, p99 {formatLatency(stage.p99Ms)},
                max {formatLatency(stage.maxMs)}, n={stage.count.toLocaleString()}
              </span>
            </div>
          {/each}
        </details>
      {/if}
    {:else if healthError !== null}
      <p class="dim">Health report unavailable: {healthError}</p>
    {:else}
      <p class="dim">Loading health report...</p>
    {/if}
    <div class="row">
      <AckButton quiet label="Refresh health" doneLabel="Refreshed" verb={refreshHealth} />
      <AckButton quiet label="Reveal logs" doneLabel="Opened" verb={ipc.revealLogs} />
      <AckButton
        quiet
        label="Copy health report"
        doneLabel="Copied"
        verb={copyHealthReport}
      />
      {#if healthCopyNote !== ""}<span class="dim">{healthCopyNote}</span>{/if}
    </div>
  </section>

  <section>
    <h2>Application updates</h2>
    {#if updates === null}
      <p class="dim">Loading update configuration...</p>
    {:else if !updates.enabled}
      <p class="dim">
        Signed updates are unavailable in this developer or unsigned test build.
        Production packages use the verified stable release channel.
      </p>
    {:else}
      <p class="dim">
        Version {updates.currentVersion}. Updates are checked only when you ask.
      </p>
      {#if updates.phase === "current"}
        <p class="dim">This is the newest version offered to your rollout cohort.</p>
      {:else if updates.available !== null}
        <div class="row">
          <span class="name">Version {updates.available.version}</span>
          <span class="state">signed update available</span>
        </div>
        {#if updates.available.notes !== null}
          <p class="helper">{updates.available.notes}</p>
        {/if}
        {#if updateConfirmVersion === updates.available.version}
          <div class="inline-warn">
            <span>
              Download, verify, close Photoproof cleanly, install, and restart?
            </span>
            <button
              disabled={updateBusy}
              onclick={() => void installUpdate(updates?.available?.version ?? "")}
              >Install and restart</button
            >
            <button
              class="quiet"
              disabled={updateBusy}
              onclick={() => (updateConfirmVersion = null)}>Cancel</button
            >
          </div>
        {:else}
          <button
            disabled={updateBusy}
            onclick={() => (updateConfirmVersion = updates?.available?.version ?? null)}
            >Review and install</button
          >
        {/if}
      {/if}
      {#if updates.phase === "downloading"}
        <p class="dim">
          Downloading signed update:
          {formatBytes(updates.downloadedBytes)}
          {#if updates.totalBytes !== null}
            of {formatBytes(updates.totalBytes)}
          {/if}
        </p>
      {/if}
      <button class="quiet" disabled={updateBusy} onclick={() => void checkForUpdate()}>
        {updateBusy ? "Checking..." : "Check for updates"}
      </button>
    {/if}
    {#if updateError !== null}
      <p class="dim">Update check failed: {updateError}</p>
    {/if}
  </section>

  <!-- 4. Export and full-state recovery. Journal export is the open-format
       portability path; .ppbackup is the exact installed-app state path. -->
  <section>
    <h2>Export and recovery</h2>
    <p class="dim">
      Journal export includes sidecars, collections, saved topic phrases, and topic notes. A
      complete backup also preserves settings, device identity, model choices, folders, and
      caches.
    </p>
    <div class="row">
      <button disabled={busy} onclick={() => void runExport()}>Export library journal…</button>
      {#if settings?.lastExportTs}
        <span class="dim">last export {settings.lastExportTs}</span>
      {/if}
    </div>
    <div class="row">
      {#if backupConfirmPath !== null}
        <span class="dim">Photoproof will quit, verify a complete backup, then reopen.</span>
        <button disabled={busy} onclick={() => void runFullBackup()}>Back up and quit</button>
        <button class="quiet" onclick={() => (backupConfirmPath = null)}>Cancel</button>
      {:else}
        <button class="quiet" disabled={busy} onclick={() => void chooseFullBackup()}
          >Back up complete app state…</button
        >
      {/if}
    </div>
    <div class="row">
      {#if restoreConfirmPath !== null}
        <span class="dim">
          Replace current app state after verification? Photoproof will retain the current data
          directory as a rollback copy and restart.
        </span>
        <button disabled={busy} onclick={() => void runFullRestore()}>Restore and restart</button>
        <button class="quiet" onclick={() => (restoreConfirmPath = null)}>Cancel</button>
      {:else}
        <button class="quiet" disabled={busy} onclick={() => void chooseFullRestore()}
          >Restore complete app state…</button
        >
      {/if}
    </div>
    <div class="row">
      <button class="quiet" disabled={busy} onclick={() => void importSavedTopics()}
        >Import saved topics…</button
      >
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
    {#if backupReceipt !== null}
      <p class="dim">
        {backupReceipt.succeeded ? "Completed" : "Failed"} {backupReceipt.operation}:
        {backupReceipt.detail}
        {#if backupReceipt.rollbackPath !== null}
          Previous app data retained at {backupReceipt.rollbackPath}.
        {/if}
      </p>
    {/if}
    {#if exportNote !== ""}<p class="dim">{exportNote}</p>{/if}
  </section>

  <!-- 5. Attention data (DESIGN-ATTENTION-HEATMAP.md): the privacy-hygiene
       reset for the local, machine-observed dwell telemetry. The annotation
       journal (your own words/marks) is never touched. No em-dashes in the
       copy (gate: check:emdash). -->
  <section>
    <h2>Attention data</h2>
    <p class="dim">
      The heatmap records where you put attention (what you open and mark),
      capped and stored only on this machine. Your journal is never affected.
    </p>
    <div class="row">
      {#if clearAttentionConfirm}
        <!-- inline (not modal) confirm (UI §2.4) -->
        <span class="dim">Clear all recorded attention data?</span>
        <AckButton label="Clear" doneLabel="Cleared" verb={clearAttention} />
        <button class="quiet" onclick={() => (clearAttentionConfirm = false)}>Cancel</button>
      {:else}
        <button class="quiet" onclick={() => (clearAttentionConfirm = true)}
          >Clear attention data…</button
        >
      {/if}
    </div>
  </section>
  </div>
</main>

<style>
  main {
    padding: 0 18px 18px;
    overflow-y: auto;
    height: 100%;
  }
  /* Sticky so the header bar (the "Settings" title + the native traffic-light
     strip on macOS) stays pinned at the top while the sections scroll beneath
     it, instead of scrolling away. The --bg fill + z-index keep the scrolling
     content from showing through or above it. */
  .drag {
    position: sticky;
    top: 0;
    z-index: 10;
    background: var(--bg);
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
  .boot-state,
  .action-state {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-top: 18px;
    padding: 10px 12px;
    border: 1px solid var(--chrome);
    border-radius: 7px;
    color: var(--text-dim);
    font-size: 12px;
  }
  .boot-state.warning,
  .boot-state.error,
  .action-state.error {
    border-color: var(--text-faint);
    background: var(--bg-raised);
  }
  .boot-state strong {
    color: var(--text);
  }
  .boot-state .issue {
    flex-basis: 100%;
    color: var(--text-faint);
  }
  .action-state span {
    flex: 1;
  }
  .settings-body.blocked {
    opacity: 0.45;
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
  .editor-input {
    flex: 1;
    min-width: 0;
    margin-left: 8px;
  }
  /* The 1:1 budget is a short numeric field (a couple of digits + GB), so it
     stays narrow rather than stretching like the editor path input. */
  .budget-input {
    width: 72px;
    margin-left: 8px;
  }
  /* Segmented control (System / Light / Dark): three peer buttons in a hairline
     track. Token-only so it reads correctly in both themes — the active
     segment lifts to --bg-raised, inactive ones stay flush and dim. */
  .segmented {
    display: inline-flex;
    border: 1px solid var(--chrome);
    border-radius: 6px;
    overflow: hidden;
  }
  .segmented button {
    background: transparent;
    border: none;
    border-radius: 0;
    color: var(--text-dim);
    font-size: 12px;
    padding: 4px 12px;
  }
  /* Hairline separators between segments (not on the first). */
  .segmented button + button {
    border-left: 1px solid var(--chrome);
  }
  .segmented button.active {
    background: var(--bg-raised);
    color: var(--text);
  }
  .helper {
    margin: 4px 0 0;
    font-size: 11px;
    color: var(--text-faint);
  }
  .row.license {
    margin: -4px 0 10px;
    font-size: 11px;
  }
  .row.license a {
    color: var(--text-faint);
  }
</style>
