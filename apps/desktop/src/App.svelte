<script lang="ts">
  /**
   * The application shell (UI §2): three surfaces — Grid, Look, Search —
   * plus the ambient indicator, the transient note input, the auto-hiding
   * rail, and (dev builds only) the debug panel. Quiet: no dashboards, no
   * onboarding, no toasts in M1.
   */
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { ui } from "./lib/state/app.svelte";
  import { dispatch, type KeyContext } from "./lib/logic/keymap";
  import * as ipc from "./lib/ipc/commands";
  import type { IndicatorPulse, IndicatorState, IngestStatus } from "./lib/types/dto";
  import Titlebar from "./lib/components/Titlebar.svelte";
  import Grid from "./lib/components/Grid.svelte";
  import Look from "./lib/components/Look.svelte";
  import Rail from "./lib/components/Rail.svelte";
  import SortMenu from "./lib/components/SortMenu.svelte";
  import SearchOverlay from "./lib/components/SearchOverlay.svelte";
  import Indicator from "./lib/components/Indicator.svelte";
  import NoteInput from "./lib/components/NoteInput.svelte";
  import FirstRun from "./lib/components/FirstRun.svelte";
  import { THUMB_STEPS } from "./lib/logic/sort";

  let railRef: Rail | undefined = $state();

  // Debug panel: compile-time gated (UI §10.1). With the define off, this
  // whole branch — and the chunk behind the dynamic import — is dead code.
  let DebugPanel: (typeof import("./lib/debug/DebugPanel.svelte"))["default"] | null =
    $state(null);
  $effect(() => {
    if (import.meta.env.PHOTOPROOF_DEBUG && ui.debugOpen && DebugPanel === null) {
      void import("./lib/debug/DebugPanel.svelte").then((m) => (DebugPanel = m.default));
    }
  });

  const title = $derived.by(() => {
    if (ui.searchOpen) return "Search";
    if (ui.surface === "look") {
      const item = ui.items.find((i) => i.hash === ui.lookHash);
      return item?.fileName ?? "Photoproof";
    }
    return ui.folderName;
  });

  // ---- keyboard (UI §11 — the map lives in logic/keymap.ts) ----------------

  function isTextInput(el: Element | null): boolean {
    return (
      el instanceof HTMLInputElement ||
      el instanceof HTMLTextAreaElement ||
      (el instanceof HTMLElement && el.isContentEditable)
    );
  }

  function onKeydown(e: KeyboardEvent) {
    const active = document.activeElement;
    const ctx: KeyContext = {
      surface: ui.surface,
      searchOpen: ui.searchOpen,
      inputFocused: isTextInput(active),
      searchInputFocused:
        active instanceof HTMLElement && active.dataset.searchInput !== undefined,
      hasSelection: ui.searchOpen
        ? ui.searchSel.order.length > 0
        : ui.sel.order.length > 0,
      railOpen: ui.railOpen,
      debugEnabled: import.meta.env.PHOTOPROOF_DEBUG,
      asrReady: false, // M1: no ASR
    };
    const action = dispatch(
      { key: e.key, ctrlOrMeta: e.ctrlKey || e.metaKey, shift: e.shiftKey },
      ctx,
    );
    if (action === null) return;
    // Backspace chip removal only fires on an EMPTY input (UI §11).
    if (action.kind === "remove-last-chip" && ui.query !== "") return;
    e.preventDefault();
    if (action.kind === "rail-nav") railRef?.nav(action.dir);
    else if (action.kind === "rail-enter") railRef?.enter();
    else void ui.perform(action);
    touchActivity();
  }

  // ---- activity reporting (CAPTURE §2.1), throttled -------------------------

  let lastActivityReport = 0;
  function touchActivity() {
    const now = Date.now();
    if (now - lastActivityReport < 60_000) return;
    lastActivityReport = now;
    void ipc.reportActivity();
  }

  // ---- rail dwell (UI §3.2: ~150 ms at the left edge) ------------------------

  let dwellTimer: ReturnType<typeof setTimeout> | undefined;
  function onEdgeEnter() {
    dwellTimer = setTimeout(() => (ui.railOpen = true), 150);
  }
  function onEdgeLeave() {
    clearTimeout(dwellTimer);
  }

  // ---- backend events ----------------------------------------------------------

  onMount(() => {
    void ui.init();
    const unlisteners: Promise<UnlistenFn>[] = [
      listen<IndicatorPulse>("indicator-pulse", () => ui.onPulse()),
      listen<IndicatorState>("indicator-state", (e) => {
        ui.scope = e.payload.currentScope;
      }),
      listen<IngestStatus>("ingest-progress", (e) => {
        ui.ingest = e.payload;
      }),
    ];
    // While ingest runs, the grid populates incrementally (UI §3.3/§9.1).
    const poll = setInterval(() => {
      if (ui.ingest.running) {
        void ui.refreshItems();
        void ipc.ingestStatus().then((s) => {
          ui.ingest = s;
        });
      }
    }, 2000);
    return () => {
      clearInterval(poll);
      for (const u of unlisteners) void u.then((f) => f());
    };
  });
</script>

<svelte:window onkeydown={onKeydown} onpointerdown={touchActivity} />

<div class="shell">
  <Titlebar {title} />

  <div class="surface">
    {#if ui.surface === "grid"}
      {#if ui.roots.length === 0}
        <FirstRun />
      {:else}
        <header class="grid-header">
          <span class="folder-name">▍{ui.folderName}</span>
          <span class="spacer"></span>
          <button class="quiet" onclick={() => (ui.sortMenuOpen = !ui.sortMenuOpen)}>
            sort ▾
          </button>
          <input
            class="thumb-slider"
            type="range"
            min="0"
            max={THUMB_STEPS.length - 1}
            step="1"
            value={ui.thumbStep}
            aria-label="Thumbnail size"
            oninput={(e) => ui.setThumbStep(Number(e.currentTarget.value))}
          />
        </header>
        <div class="grid-area">
          <Grid />
        </div>
      {/if}
      {#if ui.sortMenuOpen}<SortMenu />{/if}
    {:else}
      <Look />
    {/if}

    {#if ui.railOpen}
      <Rail bind:this={railRef} />
    {/if}
    <div
      class="edge-hotzone"
      role="presentation"
      onpointerenter={onEdgeEnter}
      onpointerleave={onEdgeLeave}
    ></div>

    {#if ui.searchOpen}
      <SearchOverlay />
    {/if}
  </div>

  <NoteInput />
  <Indicator />

  {#if import.meta.env.PHOTOPROOF_DEBUG && ui.debugOpen && DebugPanel !== null}
    <DebugPanel />
  {/if}
</div>

<style>
  .shell {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .surface {
    position: relative;
    flex: 1;
    overflow: hidden;
  }
  .grid-header {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 30px;
    padding: 0 12px;
  }
  .folder-name {
    color: var(--text-dim);
  }
  .spacer {
    flex: 1;
  }
  .quiet {
    border: none;
    background: transparent;
    color: var(--text-faint);
  }
  .quiet:hover {
    color: var(--text);
  }
  .thumb-slider {
    width: 90px;
    accent-color: var(--text-faint);
    background: transparent;
    border: none;
    padding: 0;
  }
  .grid-area {
    position: absolute;
    top: 30px;
    left: 0;
    right: 0;
    bottom: 0;
  }
  .edge-hotzone {
    position: absolute;
    top: 0;
    left: 0;
    bottom: 0;
    width: 5px;
    z-index: 25;
  }
</style>
