<script lang="ts">
  /**
   * Minimal custom titlebar (UI §2.3: native chrome minimal/hidden on
   * Windows/Linux). A drag strip, the window title, the window controls —
   * plus the dogfood-round-1 chrome buttons (visible UI for actions, not
   * keyboard-only): rail toggle on the left end; search and the inspector
   * Metadata/Journal toggles on the right end. Every button dispatches
   * through resolveAction over the SAME registry row its key uses (zero
   * new verbs; enablement stays on the defs), and its tooltip resolves
   * from the same def (KeyHint copy can never drift). The whole bar
   * (accessories included) is a gated chrome region — it obeys Tab
   * lights-out via App.svelte's region gate.
   *
   * Platform split (UI §2.3): macOS keeps NATIVE decorations — rounded
   * corners, shadow, traffic lights — via tauri.macos.conf.json
   * (`titleBarStyle: Overlay` + `hiddenTitle`), so this strip becomes an
   * overlay: the custom minimize/maximize/close buttons are dropped (the
   * lights own those verbs) and a left inset reserves the lights'
   * footprint so the rail toggle clears them. Windows/Linux stay
   * undecorated (`decorations: false` in the base config) and render the
   * custom controls, unchanged.
   */
  import { getCurrentWindow } from "@tauri-apps/api/window";
  // Lucide replaces the ad-hoc text glyphs (BACKLOG "Adopt Lucide icons"):
  // one stroke set, sized per-site, color inherited from the button's
  // token (currentColor) so hover/on states keep working unchanged.
  import PanelLeft from "@lucide/svelte/icons/panel-left";
  import Search from "@lucide/svelte/icons/search";
  import Info from "@lucide/svelte/icons/info";
  import List from "@lucide/svelte/icons/list";
  import Minus from "@lucide/svelte/icons/minus";
  import Square from "@lucide/svelte/icons/square";
  import X from "@lucide/svelte/icons/x";
  import { ui } from "../../state/app.svelte";
  import { resolveAction } from "../../actions/registry";
  import { tooltip } from "../../primitives/tooltip";
  import { isMac } from "../../logic/platform";
  import { jobsRemaining, jobsTitle, offlineWarningTitle } from "../../logic/jobs";
  import type { Action } from "../../logic/keymap";

  let { title }: { title: string } = $props();

  /** Sampled once per mount — the platform cannot change under a window. */
  const mac = isMac();

  const win = getCurrentWindow();
  $effect(() => {
    void win.setTitle(title);
  });

  /** Chrome buttons are a rendering of the registry: resolve, then the
   * one perform sink. A def that declines (unavailable/disabled) no-ops. */
  function perform(id: Action["kind"], arg?: unknown) {
    const action = resolveAction(id, ui.actionContext(), arg);
    if (action !== null) void ui.perform(action);
  }
</script>

<div class="titlebar" data-tauri-drag-region>
  {#if mac}
    <!-- Native traffic lights (Overlay style) render over this corner;
         the empty strip keeps them draggable and pushes the rail toggle
         clear of them. -->
    <div class="traffic-inset" data-tauri-drag-region aria-hidden="true"></div>
  {/if}
  <div class="cluster">
    <button
      class="chrome"
      aria-label="Toggle source rail"
      aria-pressed={ui.shell.railOpen}
      class:on={ui.shell.railOpen}
      onclick={() => perform("toggle-rail")}
      {@attach tooltip({ actionId: "toggle-rail" })}><PanelLeft size={14} /></button
    >
  </div>
  <span class="title" data-tauri-drag-region>{title}</span>
  <div class="cluster">
    {#if ui.shell.ingest.offlineVolumes.length > 0}
      <!-- Offline-volume warning (founder: warn + pause): a drive holding
           library photos is disconnected, so its files cannot be read and that
           ingest work is PAUSED (not churning). Say so plainly instead of
           stalling silently; the per-drive detail waits on hover. -->
      <span class="warn" role="status" title={offlineWarningTitle(ui.shell.ingest.offlineVolumes)}>
        {ui.shell.ingest.offlineVolumes.length === 1 ? "drive offline" : "drives offline"}
      </span>
    {/if}
    {#if jobsRemaining(ui.shell.ingest) > 0}
      <!-- Background jobs (BACKLOG "Header shows background jobs"): one
           quiet word while ANY pass kind has queued work — ingest, preview
           rebuilds, doctor re-pends, model backfills. Count + kind stay on
           hover; the hairline on the indicator keeps the §7.5 fraction. -->
      <span class="jobs" role="status" title={jobsTitle(ui.shell.ingest)}>digesting</span>
    {/if}
    <button
      class="chrome"
      aria-label="Search"
      onclick={() => perform("open-search")}
      {@attach tooltip({ actionId: "open-search" })}><Search size={14} /></button
    >
    <button
      class="chrome"
      aria-label="Inspector - Metadata"
      aria-pressed={ui.inspector.open === "metadata"}
      class:on={ui.inspector.open === "metadata"}
      onclick={() => perform("open-inspector", "metadata")}
      {@attach tooltip({ actionId: "open-inspector", verb: "Metadata", arg: "metadata" })}
      ><Info size={14} /></button
    >
    <button
      class="chrome"
      aria-label="Inspector - Journal"
      aria-pressed={ui.inspector.open === "journal"}
      class:on={ui.inspector.open === "journal"}
      onclick={() => perform("open-inspector", "journal")}
      {@attach tooltip({ actionId: "open-inspector", verb: "Journal", arg: "journal" })}
      ><List size={14} /></button
    >
    {#if !mac}
      <div class="controls">
        <button aria-label="Minimize" onclick={() => void win.minimize()}><Minus size={14} /></button>
        <button aria-label="Maximize" onclick={() => void win.toggleMaximize()}><Square size={12} /></button>
        <button aria-label="Close" onclick={() => void win.close()}><X size={14} /></button>
      </div>
    {/if}
  </div>
</div>

<style>
  .titlebar {
    height: 28px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 4px;
    background: var(--bg);
    flex: 0 0 auto;
  }
  .title {
    flex: 1;
    color: var(--text-dim);
    font-size: 12px;
    pointer-events: none;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .cluster {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  /* The background-jobs register: faint text, no motion — the word
   * appearing IS the signal; detail waits on hover. */
  .jobs {
    color: var(--text-faint);
    font-size: 11px;
    padding: 0 6px;
    white-space: nowrap;
  }
  /* The offline-volume warning: same quiet register as .jobs but in the
   * station's error color so a disconnected drive reads as a problem, not
   * routine background work. Detail (which drive, how many photos) on hover. */
  .warn {
    color: var(--station-error);
    font-size: 11px;
    padding: 0 6px;
    white-space: nowrap;
  }
  /* macOS only: the three 12px lights sit at the default Overlay
     position (x≈12, vertically centered in a 28px bar — exactly this
     strip's height); 78px covers their span plus breathing room. */
  .traffic-inset {
    flex: 0 0 78px;
    align-self: stretch;
  }
  .controls {
    display: flex;
    gap: 2px;
    margin-left: 6px;
  }
  /* Quiet icon buttons — the chrome rendering of registry rows. */
  .chrome,
  .controls button {
    border: none;
    background: transparent;
    color: var(--text-faint);
    width: 28px;
    height: 22px;
    padding: 0;
    border-radius: 3px;
    font-size: 13px;
    line-height: 1;
    /* Lucide renders an <svg>: flex-center it (a text glyph centered via
     * line-height; an inline svg would sit on the baseline). */
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .chrome:hover,
  .controls button:hover {
    background: var(--bg-raised);
    color: var(--text);
  }
  .chrome.on {
    color: var(--text-dim);
    background: var(--bg-raised);
  }
</style>
