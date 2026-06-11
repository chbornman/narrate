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
  import type { Action } from "../../logic/keymap";

  let { title }: { title: string } = $props();

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
    <button
      class="chrome"
      aria-label="Search"
      onclick={() => perform("open-search")}
      {@attach tooltip({ actionId: "open-search" })}><Search size={14} /></button
    >
    <button
      class="chrome"
      aria-label="Inspector — Metadata"
      aria-pressed={ui.inspector.open === "metadata"}
      class:on={ui.inspector.open === "metadata"}
      onclick={() => perform("open-inspector", "metadata")}
      {@attach tooltip({ actionId: "open-inspector", verb: "Metadata", arg: "metadata" })}
      ><Info size={14} /></button
    >
    <button
      class="chrome"
      aria-label="Inspector — Journal"
      aria-pressed={ui.inspector.open === "journal"}
      class:on={ui.inspector.open === "journal"}
      onclick={() => perform("open-inspector", "journal")}
      {@attach tooltip({ actionId: "open-inspector", verb: "Journal", arg: "journal" })}
      ><List size={14} /></button
    >
    <div class="controls">
      <button aria-label="Minimize" onclick={() => void win.minimize()}><Minus size={14} /></button>
      <button aria-label="Maximize" onclick={() => void win.toggleMaximize()}><Square size={12} /></button>
      <button aria-label="Close" onclick={() => void win.close()}><X size={14} /></button>
    </div>
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
