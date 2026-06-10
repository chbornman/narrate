<script lang="ts">
  /**
   * Anchored floating layer (primitive): positions at an {x,y} point or an
   * element's edge, dismisses on outside pointer-down. Esc is routed
   * through logic/escape.ts by the HOST — never self-handled here.
   * Consumers: Menu (via ContextMenuHost), the indicator scope popover,
   * tooltip bodies.
   */
  import type { Snippet } from "svelte";

  let {
    anchor,
    placement = "bottom-start",
    ondismiss,
    children,
  }: {
    anchor: { x: number; y: number } | HTMLElement | null;
    placement?: "bottom-start" | "top-end";
    ondismiss: () => void;
    children: Snippet;
  } = $props();

  let el: HTMLDivElement | undefined = $state();

  const point = $derived.by(() => {
    if (anchor === null) return null;
    if (anchor instanceof HTMLElement) {
      const r = anchor.getBoundingClientRect();
      return placement === "top-end"
        ? { x: r.right, y: r.top }
        : { x: r.left, y: r.bottom };
    }
    return anchor;
  });

  // Clamp into the viewport after mount/position changes.
  let dx = $state(0);
  let dy = $state(0);
  $effect(() => {
    void point;
    dx = 0;
    dy = 0;
    const box = el?.getBoundingClientRect();
    if (box === undefined) return;
    if (box.right > window.innerWidth) dx = window.innerWidth - box.right - 8;
    if (box.bottom > window.innerHeight) dy = window.innerHeight - box.bottom - 8;
  });

  function onWindowPointerDown(e: PointerEvent) {
    if (el !== undefined && !el.contains(e.target as Node)) ondismiss();
  }
</script>

<svelte:window onpointerdown={onWindowPointerDown} />

<div
  bind:this={el}
  class="popover"
  class:fallback={point === null}
  style:left={point === null ? undefined : `${point.x + dx}px`}
  style:top={point === null ? undefined : `${point.y + dy}px`}
  role="presentation"
>
  {@render children()}
</div>

<style>
  .popover {
    position: fixed;
    z-index: 70;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 6px;
  }
  /* Keyboard-summoned (no anchor point): a quiet default near the header. */
  .popover.fallback {
    top: 34px;
    right: 16px;
  }
</style>
