<script lang="ts">
  /**
   * Resizable side panel (primitive) — PUSH, never overlay: it is a flex
   * child of the shell row, so the grid re-snaps integer columns on every
   * resize (supersedes UI.md §3.7 overlay rail — DECISIONS entry 3). No
   * auto-hide mode exists, by construction (featureset §3). Width persists
   * via persistKey; openness is the caller's state (gated through
   * lights-out in App.svelte). Consumers: SourceRail, Inspector, the M5
   * partner host.
   */
  import type { Snippet } from "svelte";
  import { clampSize, dragSize, loadSize, saveSize } from "./panel";

  let {
    side,
    open,
    size = $bindable(240),
    minSize = 160,
    maxSize = 480,
    persistKey,
    label,
    children,
  }: {
    side: "left" | "right";
    open: boolean;
    size?: number;
    minSize?: number;
    maxSize?: number;
    persistKey?: string;
    label: string;
    children: Snippet;
  } = $props();

  $effect(() => {
    if (persistKey !== undefined) size = loadSize(persistKey, size, minSize, maxSize);
  });

  let dragStart: { size: number; x: number } | null = null;

  function onHandleDown(e: PointerEvent) {
    dragStart = { size, x: e.clientX };
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onHandleMove(e: PointerEvent) {
    if (dragStart === null) return;
    size = dragSize(dragStart, e.clientX, side, minSize, maxSize);
  }
  function onHandleUp() {
    if (dragStart === null) return;
    dragStart = null;
    size = clampSize(size, minSize, maxSize);
    if (persistKey !== undefined) saveSize(persistKey, size);
  }
</script>

{#if open}
  <aside class="panel {side}" style:width="{size}px" aria-label={label}>
    <div class="body">
      {@render children()}
    </div>
    <div
      class="handle {side}"
      role="presentation"
      onpointerdown={onHandleDown}
      onpointermove={onHandleMove}
      onpointerup={onHandleUp}
    ></div>
  </aside>
{/if}

<style>
  .panel {
    position: relative;
    flex: 0 0 auto;
    height: 100%;
    background: var(--bg-overlay);
    overflow: hidden;
  }
  .panel.left {
    border-right: 1px solid var(--chrome);
  }
  .panel.right {
    border-left: 1px solid var(--chrome);
  }
  .body {
    position: absolute;
    inset: 0;
    overflow-y: auto;
  }
  .handle {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 5px;
    cursor: col-resize;
    z-index: 1;
  }
  .handle.left {
    right: 0;
  }
  .handle.right {
    left: 0;
  }
  .handle:hover {
    background: var(--chrome);
  }
</style>
