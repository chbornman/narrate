<script lang="ts">
  /**
   * Resizable edge panel (primitive) — THE one frame contract for every
   * edge bar (founder, June 12 2026): the canvas is always the center
   * section; rail (left), inspector (right), and filmstrip (bottom of the
   * center column) are peers of this one component, so their resize feel,
   * border, and background cannot diverge. PUSH, never overlay: the panel
   * is a layout child of the shell, so the grid re-snaps integer columns
   * on every resize (supersedes UI.md §3.7 overlay rail — DECISIONS 3).
   * No auto-hide mode exists, by construction (featureset §3).
   *
   * Size is GLOBAL per panel — one remembered size (state/prefs.ts
   * pp.panel.<id>.size), loaded ONCE here: the old persistKey $effect
   * also READ `size`, so every drag frame snapped back to the stored
   * width — the founder's "rail can't be click-drag resized". Openness is
   * the caller's state (the lights-out snapshot closes panels at the
   * root, so no chromeHidden gate lives here). The drag handle sits on
   * the panel's INNER edge; double-click resets the default size.
   * Consumers: SourceRail, Inspector, Filmstrip, the M5 partner host.
   */
  import { untrack, type Snippet } from "svelte";
  import {
    PANEL_SPECS,
    loadPanelSize,
    savePanelSize,
    type PanelId,
  } from "../state/prefs";
  import { clampSize, dragSize, type PanelEdge } from "./panel";

  let {
    id,
    edge,
    open,
    label,
    children,
  }: {
    id: PanelId;
    edge: PanelEdge;
    open: boolean;
    label: string;
    children: Snippet;
  } = $props();

  const spec = $derived(PANEL_SPECS[id]);
  // Loaded ONCE at init — deliberately NOT inside an effect (see header).
  // `id` is static per consumer; untrack states that intent explicitly.
  let size = $state(untrack(() => loadPanelSize(id)));

  // The pointer axis follows the edge: side panels resize on X, bottom on Y.
  const pos = (e: PointerEvent) => (edge === "bottom" ? e.clientY : e.clientX);

  let dragStart: { size: number; pos: number } | null = null;

  function onHandleDown(e: PointerEvent) {
    dragStart = { size, pos: pos(e) };
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onHandleMove(e: PointerEvent) {
    if (dragStart === null) return;
    size = dragSize(dragStart, pos(e), edge, spec.minSize, spec.maxSize);
  }
  function onHandleUp() {
    if (dragStart === null) return;
    dragStart = null;
    size = clampSize(size, spec.minSize, spec.maxSize);
    savePanelSize(id, size);
  }
  /** Double-click the handle: reset to the panel's default size. */
  function onHandleReset() {
    size = spec.defaultSize;
    savePanelSize(id, size);
  }
</script>

{#if open}
  <aside
    class="panel {edge}"
    style:width={edge === "bottom" ? undefined : `${size}px`}
    style:height={edge === "bottom" ? `${size}px` : undefined}
    aria-label={label}
  >
    <div class="body {edge}">
      {@render children()}
    </div>
    <div
      class="handle {edge}"
      role="presentation"
      onpointerdown={onHandleDown}
      onpointermove={onHandleMove}
      onpointerup={onHandleUp}
      ondblclick={onHandleReset}
    ></div>
  </aside>
{/if}

<style>
  /* The one styling contract: background, edge border, handle — shared by
   * every consumer so rail and inspector cannot drift apart. */
  .panel {
    position: relative;
    flex: 0 0 auto;
    background: var(--bg-overlay);
    overflow: hidden;
  }
  .panel.left,
  .panel.right {
    height: 100%;
  }
  .panel.bottom {
    width: 100%;
  }
  .panel.left {
    border-right: 1px solid var(--chrome);
  }
  .panel.right {
    border-left: 1px solid var(--chrome);
  }
  .panel.bottom {
    border-top: 1px solid var(--chrome);
  }
  .body {
    position: absolute;
    inset: 0;
  }
  .body.left,
  .body.right {
    overflow-y: auto;
  }
  .body.bottom {
    overflow: hidden; /* content (filmstrip) owns its own x-scroll */
  }
  .handle {
    position: absolute;
    z-index: 1;
  }
  .handle.left,
  .handle.right {
    top: 0;
    bottom: 0;
    width: 5px;
    cursor: col-resize;
  }
  .handle.left {
    right: 0;
  }
  .handle.right {
    left: 0;
  }
  .handle.bottom {
    left: 0;
    right: 0;
    top: 0;
    height: 5px;
    cursor: row-resize;
  }
  .handle:hover {
    background: var(--chrome);
  }
</style>
