<script lang="ts">
  /**
   * Anchored floating layer (primitive): positions at an {x,y} point or an
   * element's edge, dismisses on outside pointer-down. Esc is routed
   * through logic/escape.ts by the HOST — never self-handled here.
   * Consumers: Menu (via ContextMenuHost), the indicator scope popover,
   * tooltip bodies.
   *
   * Placements:
   *   · "bottom-start" — anchored at the element's bottom-left, opening DOWN
   *     (the default: menus, the ranking popover).
   *   · "top-end" — anchored at the element's TOP-RIGHT, opening UPWARD so
   *     the panel floats ABOVE the anchor and never covers it (founder, June
   *     13 2026: the station's detail must sit above the always-visible icon
   *     row). A `translateY(-100%)` plus a small gap lifts the body clear of
   *     the anchor's top edge; the viewport clamp runs on the post-transform
   *     box, so a tall panel still pins inside the screen.
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

  // "top-end" opens upward: the panel's RIGHT edge tracks the anchor's right
  // edge and its body floats above the top edge (transform below). Other
  // placements drop down from the anchor's bottom-left.
  const up = $derived(placement === "top-end");

  const point = $derived.by(() => {
    if (anchor === null) return null;
    if (anchor instanceof HTMLElement) {
      const r = anchor.getBoundingClientRect();
      return up ? { x: r.right, y: r.top } : { x: r.left, y: r.bottom };
    }
    return anchor;
  });

  // Clamp into the viewport after mount/position changes. getBoundingClientRect
  // reads the POST-transform box, so the upward placement's translate is
  // already reflected — we only nudge it back inside any edge it overruns.
  let dx = $state(0);
  let dy = $state(0);
  $effect(() => {
    void point;
    void up;
    dx = 0;
    dy = 0;
    const box = el?.getBoundingClientRect();
    if (box === undefined) return;
    if (box.right > window.innerWidth) dx = window.innerWidth - box.right - 8;
    if (box.left < 0) dx = -box.left + 8;
    if (box.bottom > window.innerHeight) dy = window.innerHeight - box.bottom - 8;
    // Upward panels can run past the TOP of the screen (a tall task list on a
    // short window): pin the top edge in, taking precedence over the bottom.
    if (box.top < 0) dy = -box.top + 8;
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
  style:transform={point === null
    ? undefined
    : up
      ? // anchor at top-right, float ABOVE: pull the box left by its own width
        // (right-align to point.x) and up by its full height + an 8px gap, so
        // the body never overlaps the anchor's top edge (the icon row).
        "translate(-100%, calc(-100% - 8px))"
      : undefined}
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
