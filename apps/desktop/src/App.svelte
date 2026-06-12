<script lang="ts">
  /**
   * The application shell (UI §2 + featureset): chrome REGIONS — Titlebar,
   * SourceRail, GridSurface, LookSurface, Inspector, overlays — each gated
   * {#if !ui.shell.chromeHidden} (Tab lights-out, featureset §0; future
   * chrome obeys by construction because App mounts chrome only through
   * gated regions). EXEMPT by ruling: the capture indicator (capture-state
   * truth — modes must stay visible) and an open note input. On macOS the
   * NATIVE traffic lights (Overlay titlebar) sit outside these DOM gates;
   * the perform sink (app.svelte.ts, toggle-lights-out) hides/shows them
   * in lockstep via set_traffic_lights_hidden.
   *
   * The edge-dwell hotzone is DELETED — no auto-hide fly-outs (featureset
   * §3); the rail is a push panel on `\`. ContextMenuHost + ToastHost +
   * Cheatsheet + DropConfirm (drag-folder → register-root, featureset §6)
   * mount here.
   */
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { INGEST_RELIST_MS, ui } from "./lib/state/app.svelte";
  import { dispatch } from "./lib/logic/keymap";
  import { resolveAction } from "./lib/actions/registry";
  import type { ActionDef } from "./lib/actions/types";
  import * as ipc from "./lib/ipc/commands";
  import type {
    AppSettings,
    CollectionDto,
    IndicatorPulse,
    IndicatorState,
    IngestStatus,
    JournalChanged,
    PreviewsChanged,
    RootDto,
    RuntimeStatus,
  } from "./lib/types/dto";
  import Titlebar from "./lib/components/shell/Titlebar.svelte";
  import Station from "./lib/components/shell/Station.svelte";
  import NoteInput from "./lib/components/shell/NoteInput.svelte";
  import FirstRun from "./lib/components/shell/FirstRun.svelte";
  import ConsentCard from "./lib/components/shell/ConsentCard.svelte";
  import WelcomeCard from "./lib/components/shell/WelcomeCard.svelte";
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
    // Ctrl+Cmd chords are RESERVED for the native menu/system (macOS
    // convention: ⌃⌘F fullscreen, ⌃⌘Space etc.). KeyInput collapses both
    // modifiers into ctrlOrMeta, so without this gate ⌃⌘F would match the
    // ⌘F open-search row here, preventDefault, and starve the menu's
    // accelerator. No registry chord wants both modifiers (the KeyChord
    // shape cannot even express it) — let these through untouched.
    if (e.ctrlKey && e.metaKey) return;
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

  // ---- native menu bar (menu.rs → one `menu-action` event) ------------------

  /**
   * Custom menu items carry REGISTRY ids (menu.rs); resolving them through
   * resolveAction — the chrome-button path — keeps the native menu a fourth
   * RENDERING of the one action table: availability/enablement gate on the
   * def, and a menu click can never drift from what the key does. The two
   * ui-zoom-in/out ids are the menu's spelling of the parametrized
   * `ui-zoom` row (a native item is one fixed verb; the def carries the
   * direction as a chord arg).
   */
  function onMenuAction(id: string) {
    const ctx = ui.actionContext();
    const action =
      id === "ui-zoom-in"
        ? resolveAction("ui-zoom", ctx, 1)
        : id === "ui-zoom-out"
          ? resolveAction("ui-zoom", ctx, -1)
          : resolveAction(id as ActionDef["id"], ctx);
    if (action === null) return; // unknown id or gated-off verb: inert, never an error
    void ui.perform(action);
    touchActivity();
  }

  // The M two-gesture mic's release half (CAPTURE §6.4): the registry is
  // keydown-only, so the keyup is a raw key fact — the hold-E / Space-pan
  // precedent. UNCONDITIONAL (no suppression mirror): the press side was
  // already gated by the registry (§11 typing suppression, asrReady), so
  // with no gesture in flight the machine no-ops — and when a gesture IS
  // in flight, the release must always resolve or a hold could wedge the
  // mic open (e.g. focus landed in an input mid-hold).
  function onKeyup(e: KeyboardEvent) {
    if (e.key === "m" || e.key === "M") void ui.micRelease();
  }

  // Window loss mid-hold: the keyup will never arrive (the same reason
  // LookStage releases Space/hold-E on blur).
  function onWindowBlur() {
    void ui.micWindowBlur();
  }

  // ---- activity reporting (CAPTURE §2.1), throttled -------------------------

  // At most one report per minute: the throttle must stay far below the
  // 30-minute idle boundary (CAPTURE §2.2) or a still-active user's
  // session could rotate between reports.
  const ACTIVITY_REPORT_THROTTLE_MS = 60_000;
  let lastActivityReport = 0;
  function touchActivity() {
    const now = Date.now();
    if (now - lastActivityReport < ACTIVITY_REPORT_THROTTLE_MS) return;
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
      // The pulse is pure indicator feedback now — the Look overlay's
      // refresh migrated onto the hash-aware `journal-changed` channel.
      listen<IndicatorPulse>("indicator-pulse", () => ui.shell.onPulse()),
      // Journal truth changed (any writer: typed/panel/pencil flows today;
      // M2b voice events land without UI actions): affected open surfaces
      // — journal panel, grid badges, Look overlay — refresh themselves.
      listen<JournalChanged>("journal-changed", (e) =>
        void ui.onJournalChanged(e.payload.hashes),
      ),
      // Preview artifacts landed (ingest drain): thumbs that gave up
      // retrying a 404 heal off the hash-aware ping — no restart needed.
      listen<PreviewsChanged>("previews-changed", (e) =>
        ui.grid.onPreviewsChanged(e.payload.hashes),
      ),
      listen<IndicatorState>("indicator-state", (e) => {
        ui.shell.onIndicatorState(e.payload);
      }),
      // Indicator pill + the mid-scan grid re-list (2 s throttle inside —
      // a slow network-volume scan otherwise shows an EMPTY grid until
      // some unrelated refresh happens to fire).
      listen<IngestStatus>("ingest-progress", (e) => void ui.onIngestProgress(e.payload)),
      // The Settings window's edits land live (set_stack_display emits to
      // every window; the grid re-pairs stacks on the spot).
      listen<AppSettings>("settings-changed", (e) => ui.applySettings(e.payload)),
      // Root edits from any window (Settings add/remove — the same
      // pattern): the rail updates instantly off the fresh snapshot.
      listen<RootDto[]>("roots-changed", (e) => void ui.onRootsChanged(e.payload)),
      // Collection mutations from any window (same snapshot pattern): the
      // rail's Collections tab — and a viewed collection's grid — follow.
      listen<CollectionDto[]>("collections-changed", (e) =>
        void ui.onCollectionsChanged(e.payload),
      ),
      // RUNTIME §8.3: readiness/download snapshots — features light up
      // individually and silently (mic glyph appears, nothing else moves).
      listen<RuntimeStatus>("runtime-status", (e) => ui.shell.onRuntimeStatus(e.payload)),
      // Native menu bar (macOS, menu.rs): custom items forward their
      // registry id here — the same perform sink the keyboard feeds.
      listen<string>("menu-action", (e) => onMenuAction(e.payload)),
      // Drag a folder onto the window → register-root confirm (featureset
      // §6; the OS hands paths only on drop — DropConfirm renders them).
      getCurrentWebview().onDragDropEvent((e) => {
        if (e.payload.type === "drop") ui.offerDrop(e.payload.paths);
      }),
    ];
    // While ingest runs, the grid populates incrementally (UI §3.3/§9.1)
    // on the same cadence as the event-driven re-list throttle — one
    // shared constant, one mid-scan refresh policy.
    const poll = setInterval(() => {
      if (ui.shell.ingest.running) {
        void ui.refreshItems();
        void ipc.ingestStatus().then((s) => {
          ui.shell.ingest = s;
        });
      }
    }, INGEST_RELIST_MS);
    return () => {
      clearInterval(poll);
      for (const u of unlisteners) void u.then((f) => f());
    };
  });
</script>

<svelte:window
  onkeydown={onKeydown}
  onkeyup={onKeyup}
  onblur={onWindowBlur}
  onpointerdown={touchActivity}
/>

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
              {#if ui.collectionId !== null}
                {@const memberCount =
                  ui.collections.find((c) => c.id === ui.collectionId)?.memberCount ?? 0}
                {#if memberCount > 0}
                  <!-- members exist (the rail badge counts them) but none
                       are renderable: every member is a hash this library
                       never indexed (e.g. gathered on another machine and
                       union-merged in, RETRIEVAL 10.2). Membership outlives
                       files (10.1), so the copy must not claim nothing was
                       gathered. -->
                  <EmptyState
                    line={`${memberCount} gathered ${memberCount === 1 ? "image is" : "images are"} not in this library — they appear once their files are indexed here.`}
                  />
                {:else}
                  <!-- an empty collection states its own next action: the
                       verb lives on the image context menu -->
                  <EmptyState
                    line="Nothing gathered yet — right-click an image and choose Add to collection."
                  />
                {/if}
              {:else if ui.shell.ingest.running || ui.shell.ingestExpecting}
                <!-- pending work must NEVER read "No photographs" (founder,
                     June 2026): ingestExpecting bridges the click→first-emit
                     gap on add/rescan; `running` (walk-aware via `scanning`)
                     carries from the first real event; the live discovered
                     count keeps a slow volume's long walk honest -->
                {#if ui.shell.ingest.scanning && ui.shell.ingest.discovered > 0}
                  <EmptyState
                    line={`Indexing — ${ui.shell.ingest.discovered.toLocaleString()} ${ui.shell.ingest.discovered === 1 ? "photograph" : "photographs"} found so far…`}
                  />
                {:else}
                  <EmptyState line="Indexing — photographs appear as they are found." />
                {/if}
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

  <!-- exempt from lights-out: transient note input + the station -->
  <NoteInput />
  <Station />

  <!-- one-time quiet model consent (UI §9.1.3) — a panel, never a gate -->
  <ConsentCard />

  <ContextMenuHost />
  <ToastHost />
  <Cheatsheet />
  <DropConfirm />

  <!-- first-run storage story (BACKLOG) — mounted LAST among the z-80
       overlays so the DOM order matches its escape-layer-1 position -->
  <WelcomeCard />

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
