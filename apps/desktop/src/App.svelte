<script lang="ts">
  /**
   * The application shell (UI §2 + featureset): chrome REGIONS — Titlebar,
   * SourceRail, GridSurface, LookSurface, Inspector, overlays — each gated
   * {#if !ui.shell.chromeHidden} (Tab lights-out, featureset §0; future
   * chrome obeys by construction because App mounts chrome only through
   * gated regions). EXEMPT by ruling: the capture indicator (capture-state
   * truth — modes must stay visible) and an open note input.
   *
   * The edge-dwell hotzone is DELETED — no auto-hide fly-outs (featureset
   * §3); the rail is a push panel on `\`. ContextMenuHost + ToastHost +
   * Cheatsheet + DropConfirm (drag-folder → register-root, featureset §6)
   * mount here.
   */
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { ui } from "./lib/state/app.svelte";
  import { dispatch } from "./lib/logic/keymap";
  import * as ipc from "./lib/ipc/commands";
  import type {
    AppSettings,
    IndicatorPulse,
    IndicatorState,
    IngestStatus,
    RootDto,
    RuntimeStatus,
  } from "./lib/types/dto";
  import Titlebar from "./lib/components/shell/Titlebar.svelte";
  import Indicator from "./lib/components/shell/Indicator.svelte";
  import NoteInput from "./lib/components/shell/NoteInput.svelte";
  import FirstRun from "./lib/components/shell/FirstRun.svelte";
  import ConsentCard from "./lib/components/shell/ConsentCard.svelte";
  import Cheatsheet from "./lib/components/shell/Cheatsheet.svelte";
  import ContextMenuHost from "./lib/components/shell/ContextMenuHost.svelte";
  import DropConfirm from "./lib/components/shell/DropConfirm.svelte";
  import SourceRail from "./lib/components/rail/SourceRail.svelte";
  import GridSurface from "./lib/components/grid/GridSurface.svelte";
  import LookSurface from "./lib/components/look/LookSurface.svelte";
  import Inspector from "./lib/components/inspector/Inspector.svelte";
  import SearchOverlay from "./lib/components/search/SearchOverlay.svelte";
  import EmptyState from "./lib/primitives/EmptyState.svelte";
  import ToastHost from "./lib/primitives/ToastHost.svelte";

  // Debug panel: compile-time gated (UI §10.1). With the define off, this
  // whole branch — and the chunk behind the dynamic import — is dead code.
  let DebugPanel: (typeof import("./lib/debug/DebugPanel.svelte"))["default"] | null =
    $state(null);
  $effect(() => {
    if (import.meta.env.PHOTOPROOF_DEBUG && ui.shell.debugOpen && DebugPanel === null) {
      void import("./lib/debug/DebugPanel.svelte").then((m) => (DebugPanel = m.default));
    }
  });

  const title = $derived.by(() => {
    if (ui.searchOpen) return "Search";
    if (ui.surface === "look") {
      const item = ui.grid.items.find((i) => i.hash === ui.look.currentHash);
      return item?.fileName ?? "Photoproof";
    }
    return ui.folderName;
  });

  // ---- keyboard (one registry; dispatch = match over it) --------------------

  function isTextInput(el: Element | null): boolean {
    return (
      el instanceof HTMLInputElement ||
      el instanceof HTMLTextAreaElement ||
      (el instanceof HTMLElement && el.isContentEditable)
    );
  }

  function onKeydown(e: KeyboardEvent) {
    const active = document.activeElement;
    const ctx = ui.actionContext({
      inputFocused: isTextInput(active),
      searchInputFocused:
        active instanceof HTMLElement && active.dataset.searchInput !== undefined,
    });
    const action = dispatch(
      { key: e.key, ctrlOrMeta: e.ctrlKey || e.metaKey, shift: e.shiftKey },
      ctx,
    );
    if (action === null) {
      // Tab is consumed globally in the main window even when suppressed —
      // webview focus traversal is forfeited (DECISIONS 2, a11y note).
      if (e.key === "Tab") e.preventDefault();
      return;
    }
    e.preventDefault();
    void ui.perform(action);
    touchActivity();
  }

  // ---- activity reporting (CAPTURE §2.1), throttled -------------------------

  let lastActivityReport = 0;
  function touchActivity() {
    const now = Date.now();
    if (now - lastActivityReport < 60_000) return;
    lastActivityReport = now;
    // The echo is the post-touch session id: a rotation (the 30-minute
    // idle boundary, CAPTURE §2.2) closed the session the pencil undo
    // stack belongs to — clear it (§8.5 "cleared at session close").
    ipc
      .reportActivity()
      .then((sessionId) => ui.look.syncUndoSession(sessionId))
      .catch(() => {});
  }

  // ---- backend events ----------------------------------------------------------

  onMount(() => {
    ui.debugEnabled = Boolean(import.meta.env.PHOTOPROOF_DEBUG);
    void ui.init();
    const unlisteners: Promise<UnlistenFn>[] = [
      listen<IndicatorPulse>("indicator-pulse", (e) => {
        ui.shell.onPulse();
        // Stroke-affecting commits (any source: pencil flows, journal
        // panel, redaction) refresh the Look overlay's fold (P5.1).
        if (["stroke", "retraction", "redaction"].includes(e.payload.eventKind))
          ui.look.strokesVersion += 1;
      }),
      listen<IndicatorState>("indicator-state", (e) => {
        ui.shell.onIndicatorState(e.payload);
      }),
      listen<IngestStatus>("ingest-progress", (e) => {
        ui.shell.ingest = e.payload;
      }),
      // The Settings window's edits land live (set_stack_display emits to
      // every window; the grid re-pairs stacks on the spot).
      listen<AppSettings>("settings-changed", (e) => ui.applySettings(e.payload)),
      // Root edits from any window (Settings add/remove — the same
      // pattern): the rail updates instantly off the fresh snapshot.
      listen<RootDto[]>("roots-changed", (e) => void ui.onRootsChanged(e.payload)),
      // RUNTIME §8.3: readiness/download snapshots — features light up
      // individually and silently (mic glyph appears, nothing else moves).
      listen<RuntimeStatus>("runtime-status", (e) => ui.shell.onRuntimeStatus(e.payload)),
      // Drag a folder onto the window → register-root confirm (featureset
      // §6; the OS hands paths only on drop — DropConfirm renders them).
      getCurrentWebview().onDragDropEvent((e) => {
        if (e.payload.type === "drop") ui.offerDrop(e.payload.paths);
      }),
    ];
    // While ingest runs, the grid populates incrementally (UI §3.3/§9.1).
    const poll = setInterval(() => {
      if (ui.shell.ingest.running) {
        void ui.refreshItems();
        void ipc.ingestStatus().then((s) => {
          ui.shell.ingest = s;
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

<div class="shell" data-surround={ui.shell.surround}>
  {#if !ui.shell.chromeHidden}
    <Titlebar {title} />
  {/if}

  <div class="main">
    <!-- push panel: openness gated through lights-out inside the region -->
    <SourceRail />

    <div class="surface">
      {#if ui.surface === "grid"}
        {#if ui.roots.length === 0}
          <FirstRun />
        {:else}
          <GridSurface />
          {#if ui.grid.units.length === 0}
            <!-- empty folder: say the next action (featureset §6); during
                 ingest photographs stream in, so the line stays honest -->
            <div class="grid-empty">
              {#if ui.shell.ingest.running}
                <EmptyState line="Indexing — photographs appear as they are found." />
              {:else}
                <EmptyState line="No photographs in this folder.">
                  {#snippet action()}
                    <button onclick={() => void ui.perform({ kind: "toggle-rail" })}>
                      Browse sources
                    </button>
                  {/snippet}
                </EmptyState>
              {/if}
            </div>
          {/if}
        {/if}
      {:else}
        <LookSurface />
      {/if}

      {#if ui.searchOpen}
        <SearchOverlay />
      {/if}
    </div>

    <Inspector />
  </div>

  <!-- exempt from lights-out: transient note input + the indicator -->
  <NoteInput />
  <Indicator />

  <!-- one-time quiet model consent (UI §9.1.3) — a panel, never a gate -->
  <ConsentCard />

  <ContextMenuHost />
  <ToastHost />
  <Cheatsheet />
  <DropConfirm />

  {#if import.meta.env.PHOTOPROOF_DEBUG && ui.shell.debugOpen && DebugPanel !== null}
    <DebugPanel />
  {/if}
</div>

<style>
  .shell {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .main {
    flex: 1;
    display: flex;
    min-height: 0; /* the flex row, panels push — never overlay */
  }
  .surface {
    position: relative;
    flex: 1;
    overflow: hidden;
    min-width: 0;
  }
  /* The empty-folder line floats over the (header-bearing) grid surface
   * without intercepting its pointer seats; only the action clicks. */
  .grid-empty {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .grid-empty :global(button) {
    pointer-events: auto;
  }
</style>
