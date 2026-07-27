<script lang="ts">
  /**
   * The application shell (UI §2 + featureset). LAYOUT CONTRACT (founder,
   * June 12 2026): the canvas is ALWAYS the center section — main is
   * columns [rail auto][center 1fr][inspector auto], and the center
   * column is rows [canvas 1fr][filmstrip auto]. The filmstrip therefore
   * spans the CANVAS width by construction and resizes as the side
   * panels toggle; every edge bar is a peer Panel with one frame
   * contract (primitives/Panel.svelte).
   *
   * Tab lights-out (featureset §0) is a SNAPSHOT-RESTORE at the root
   * (app.svelte.ts toggleLightsOut): hiding records which panels were
   * open and closes them; Tab again restores exactly that set. Titlebar
   * and the grid header still gate on {#if !ui.shell.chromeHidden}.
   * EXEMPT by ruling: the capture indicator (capture-state truth — modes
   * must stay visible) and an open note input. On macOS the NATIVE
   * traffic lights (Overlay titlebar) sit outside these DOM gates; the
   * perform sink hides/shows them in lockstep via
   * set_traffic_lights_hidden.
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
  import { resolveAction } from "./lib/actions/registry";
  import type { ActionDef } from "./lib/actions/types";
  import * as ipc from "./lib/ipc/commands";
  import type {
    ApplicationHealth,
    ApplicationStateChanged,
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
  import BootStatus from "./lib/components/shell/BootStatus.svelte";
  import Cheatsheet from "./lib/components/shell/Cheatsheet.svelte";
  import ContextMenuHost from "./lib/components/shell/ContextMenuHost.svelte";
  import DropConfirm from "./lib/components/shell/DropConfirm.svelte";
  import SourceRail from "./lib/components/rail/SourceRail.svelte";
  import Filmstrip from "./lib/components/shell/Filmstrip.svelte";
  import GridSurface from "./lib/components/grid/GridSurface.svelte";
  import LookSurface from "./lib/components/look/LookSurface.svelte";
  // The visualizer view (DESIGN-VIEW-MODES.md): a force-directed map of the
  // current scope, now a PEER view rendered when ui.viewMode === "visualizer"
  // (no longer an overlay boolean).
  import TopicGraph from "./lib/components/graph/TopicGraph.svelte";
  import Inspector from "./lib/components/inspector/Inspector.svelte";
  import EmptyState from "./lib/primitives/EmptyState.svelte";
  import ToastHost from "./lib/primitives/ToastHost.svelte";
  // The zero-results line (UI §5.2) — reused from the search render module
  // now that a committed query renders into the grid (M3 search-as-scope).
  import { ZERO_RESULTS_LINE } from "./lib/search/render";

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
    // Search is a grid scope now (M3), not a view — the title follows the
    // grid's source/folder name (folderName), the same as any other scope.
    // Look titles with the viewed file; grid + visualizer use the scope name.
    if (ui.viewMode === "look") {
      const item = ui.grid.items.find((i) => i.hash === ui.look.currentHash);
      return item?.fileName ?? "Photoproof";
    }
    return ui.folderName;
  });
  let applicationHealth = $state<ApplicationHealth | null>(null);

  /** Health is event/refocus refreshed, never polled. The titlebar and
   * Settings both render the backend's same issue projection. */
  async function refreshApplicationHealth(): Promise<void> {
    try {
      applicationHealth = await ipc.applicationHealth();
    } catch {
      // BootStatus remains the authority for command-channel failure. Keep the
      // last good health snapshot instead of flashing a false healthy state.
    }
  }

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
      // Space is the mic key (June 12 2026) and nothing else: when the
      // row is gated off (ASR not ready, rail focused, …) the undispatched
      // Space must not fall through to the browser default — previously
      // the grid's open-look row consumed it, and without this the grid
      // scroll container would jump a page (or a focused control would
      // "click"). EXCEPT while typing: there Space must keep typing
      // spaces, which is the whole point of the §11 suppression.
      if (e.key === " " && !ctx.inputFocused) e.preventDefault();
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

  // The Space two-gesture mic's release half (CAPTURE §6.4): the registry
  // is keydown-only, so the keyup is a raw key fact — the hold-E
  // precedent. UNCONDITIONAL (no suppression mirror): the press side was
  // already gated by the registry (§11 typing suppression, asrReady), so
  // with no gesture in flight the machine no-ops — and when a gesture IS
  // in flight, the release must always resolve or a hold could wedge the
  // mic open (e.g. focus landed in an input mid-hold).
  function onKeyup(e: KeyboardEvent) {
    if (e.key === " ") void ui.micRelease();
  }

  // Window loss mid-hold: the keyup will never arrive (the same reason
  // PencilOverlay releases hold-E on blur).
  function onWindowBlur() {
    void ui.micWindowBlur();
    // Pause dwell capture (heatmap): the app backgrounded, so the current
    // focus episode ends here — this + the backend 60 s cap handle a walk-away
    // (DESIGN-ATTENTION-HEATMAP.md).
    ui.dwellPause();
  }

  // visibilitychange is the other half of the blur-pause: a tab/window hidden
  // by the OS (not just keyboard-focus loss) also ends the focus episode.
  function onVisibilityChange() {
    if (document.visibilityState === "hidden") ui.dwellPause();
  }

  async function recoverBoot() {
    if (ui.boot.failures.some((failure) => failure.subsystem === "bootstrap")) {
      if (ui.bootstrapRecoveryAction === "reset-device-identity") {
        const confirmed = window.confirm(
          "Both saved device-identity copies are unusable. Resetting creates a new local replica identity; quarantined evidence will be retained. Reset and relaunch?",
        );
        if (confirmed) await ipc.bootstrapResetDeviceIdentity();
      } else {
        await ipc.bootstrapRelaunch();
      }
    } else if (ui.boot.failures.some((failure) => failure.subsystem === "events")) {
      window.location.reload();
    } else {
      void ui.retryBoot();
    }
  }

  function settleLiveUpdate(task: Promise<unknown>): void {
    void task.catch((error) => ui.eventListenersFailed(error));
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
    const bootStarted = performance.now();
    ui.debugEnabled = Boolean(import.meta.env.PHOTOPROOF_DEBUG);
    const unlisteners: Promise<UnlistenFn>[] = [
      // The pulse is pure indicator feedback now — the Look overlay's
      // refresh migrated onto the hash-aware `journal-changed` channel.
      listen<IndicatorPulse>("indicator-pulse", () => ui.shell.onPulse()),
      // Journal truth changed (any writer: typed/panel/pencil flows today;
      // M2b voice events land without UI actions): affected open surfaces
      // — journal panel, grid badges, Look overlay — refresh themselves.
      listen<JournalChanged>("journal-changed", (e) =>
        settleLiveUpdate(ui.onJournalChanged(e.payload.hashes)),
      ),
      // Preview artifacts landed (ingest drain): thumbs that gave up
      // retrying a 404 heal off the hash-aware ping — no restart needed.
      listen<PreviewsChanged>("previews-changed", (e) =>
        ui.grid.onPreviewsChanged(e.payload.hashes),
      ),
      listen<IndicatorState>("indicator-state", (e) => {
        ui.shell.onIndicatorState(e.payload);
      }),
      // Indicator pill + the mid-scan grid re-list. The grid now re-lists on
      // the Seam 1 `imagesVersion` handshake inside onIngestProgress (debounced)
      // — a new image entering the open folder advances the version; no poll.
      listen<IngestStatus>("ingest-progress", (e) => {
        settleLiveUpdate(ui.onIngestProgress(e.payload));
      }),
      // The Settings window's edits land live (set_stack_display emits to
      // every window; the grid re-pairs stacks on the spot).
      listen<AppSettings>("settings-changed", (e) =>
        settleLiveUpdate(ui.onSettingsChanged(e.payload)),
      ),
      // Root edits from any window (Settings add/remove — the same
      // pattern): the rail updates instantly off the fresh snapshot.
      listen<RootDto[]>("roots-changed", (e) => {
        settleLiveUpdate(ui.onRootsChanged(e.payload));
        void refreshApplicationHealth();
      }),
      // Collection mutations from any window (same snapshot pattern): the
      // rail's Collections tab — and a viewed collection's grid — follow.
      listen<CollectionDto[]>("collections-changed", (e) =>
        settleLiveUpdate(ui.onCollectionsChanged(e.payload)),
      ),
      // Settings may union-import the portable saved-topic document. Pull the
      // committed topic snapshot before its application-state revision event
      // advances the catch-up clock.
      listen<void>("topics-changed", () =>
        settleLiveUpdate(ui.onTopicsChanged()),
      ),
      // RUNTIME §8.3: readiness/download snapshots — features light up
      // individually and silently (mic glyph appears, nothing else moves).
      listen<RuntimeStatus>("runtime-status", (e) => {
        ui.onRuntimeStatus(e.payload);
        void refreshApplicationHealth();
      }),
      // Process-monotone catch-up clock. Domain snapshot events above provide
      // the fast path; a revision gap triggers one coherent backend snapshot
      // so a late/reinstalled window cannot remain silently stale.
      listen<ApplicationStateChanged>("application-state-changed", (e) =>
        settleLiveUpdate(ui.onApplicationStateChanged(e.payload)),
      ),
      // Native menu bar (macOS, menu.rs): custom items forward their
      // registry id here — the same perform sink the keyboard feeds.
      listen<string>("menu-action", (e) => onMenuAction(e.payload)),
      // Drag a folder onto the window → register-root confirm (featureset
      // §6; the OS hands paths only on drop — DropConfirm renders them).
      getCurrentWebview().onDragDropEvent((e) => {
        if (e.payload.type === "drop") ui.offerDrop(e.payload.paths);
      }),
    ];
    const listenersInstalled = Promise.all(unlisteners)
      .then(() => ui.eventListenersReady())
      .catch((error) => ui.eventListenersFailed(error));
    // Subscribe BEFORE any cold state reads. Once boot settles, one versioned
    // catch-up closes the tiny install/read race and seeds the revision clock.
    void listenersInstalled
      .then(() => ui.init())
      .then(() => ui.catchUpApplicationState())
      .then(() => {
        void refreshApplicationHealth();
        requestAnimationFrame(() => {
          ipc.recordPerformance(
            "startup",
            "first-paint",
            performance.now() - bootStarted,
          );
        });
      })
      .catch((error) => {
        ipc.recordPerformance(
          "startup",
          "total",
          performance.now() - bootStarted,
          false,
        );
        ui.failBoot(error);
      });
    // Seam 1 (ARCHITECTURE-CONTRACTS.md step 3): the mid-scan grid re-list is
    // driven entirely by the `imagesVersion` handshake in onIngestProgress now.
    // The old setInterval(INGEST_RELIST_MS) poll that ALSO re-listed while
    // ingest ran is DELETED — it was a second, redundant timer for the same job
    // (the "each view invents its own staleness story" anti-pattern). One
    // versioned refresh policy, zero wall-clock timers.
    return () => {
      for (const u of unlisteners) void u.then((f) => f()).catch(() => {});
      void ipc.flushPerformance();
    };
  });
</script>

<svelte:window
  onkeydown={onKeydown}
  onkeyup={onKeyup}
  onblur={onWindowBlur}
  onpointerdown={touchActivity}
/>
<!-- Dwell-capture blur-pause, OS-hide half (DESIGN-ATTENTION-HEATMAP.md). -->
<svelte:document onvisibilitychange={onVisibilityChange} />

<div class="shell" data-surround={ui.shell.surround}>
  {#if !ui.shell.chromeHidden}
    <Titlebar
      {title}
      health={applicationHealth}
      onrefreshhealth={refreshApplicationHealth}
    />
  {/if}

  <div class="main">
    <!-- push panel: closed by the lights-out snapshot at the root -->
    <SourceRail />

    <!-- the CENTER column: canvas over filmstrip — the strip spans the
         canvas width by construction (founder, June 12 2026) -->
    <div class="center">
      <div class="surface">
        <!-- ONE view-mode chain (DESIGN-VIEW-MODES.md): grid / visualizer /
             look are peer views — the visualizer renders INSTEAD of the grid
             (TopicGraph fills its container), not over it. -->
        {#if ui.viewMode === "grid"}
          {#if ui.roots.length === 0}
            <FirstRun />
          {:else}
            <GridSurface />
            {#if ui.grid.units.length === 0}
              <!-- empty folder: say the next action (featureset §6); during
                   ingest photographs stream in, so the line stays honest -->
              <div class="grid-empty">
                {#if ui.gridScope.kind === "query"}
                  <!-- A committed query that matched nothing (M3 search-as-
                       scope): the honest zero-results line (UI §5.2), NOT the
                       empty-folder copy. Checked FIRST because a query over a
                       collection still has a non-null collectionId. -->
                  <EmptyState line={ZERO_RESULTS_LINE} />
                {:else if ui.collectionId !== null}
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
                      line={`${memberCount} gathered ${memberCount === 1 ? "image is" : "images are"} not in this library - they appear once their files are indexed here.`}
                    />
                  {:else}
                    <!-- an empty collection states its own next action: the
                         verb lives on the image context menu -->
                    <EmptyState
                      line="Nothing gathered yet - right-click an image and choose Add to collection."
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
                      line={`Indexing - ${ui.shell.ingest.discovered.toLocaleString()} ${ui.shell.ingest.discovered === 1 ? "photograph" : "photographs"} found so far…`}
                    />
                  {:else}
                    <EmptyState line="Indexing - photographs appear as they are found." />
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
        {:else if ui.viewMode === "visualizer"}
          <!-- The visualizer (DESIGN-VIEW-MODES.md): a force-directed map of
               the current scope. Self-contained; it reads ui.graphScope() and
               re-uses the grid's scope/Look flows. -->
          <TopicGraph />
        {:else if ui.viewMode === "look"}
          <LookSurface />
        {/if}
      </div>

      <!-- bottom edge of the CENTER column (F, both surfaces) -->
      <Filmstrip />
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

  <BootStatus
    status={ui.boot}
    recoveryAction={ui.bootstrapRecoveryAction}
    onretry={recoverBoot}
  />
</div>

<style>
  .shell {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  /* The layout contract (founder, June 12 2026): columns
   * [rail auto][center 1fr][inspector auto]; the center column is rows
   * [canvas 1fr][filmstrip auto]. Auto-sized Panel peers around a 1fr
   * center — the panels PUSH, never overlay, and a closed panel
   * collapses to nothing, so the filmstrip is canvas-width and the grid
   * re-snaps columns by construction. */
  .main {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .center {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .surface {
    position: relative;
    flex: 1;
    overflow: hidden;
    min-width: 0;
    min-height: 0;
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
