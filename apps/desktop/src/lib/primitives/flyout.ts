/**
 * Pure flyout placement math (paired with Menu.svelte; mirrors panel.ts <->
 * Panel.svelte). Cascading submenu panels fly out to the SIDE of their
 * parent row — preferred right, flipping left only when right would clip and
 * left fully fits, otherwise clamped on-screen. Same viewport idiom as
 * tooltip.ts / Popover.svelte (plain rect math; no floating-ui/popperjs).
 *
 * WHY a pure module: the flip decision is the jank-prone part (off-screen
 * submenus), so it lives where jsdom can pin every edge case without a DOM.
 */

export interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

export interface Viewport {
  width: number;
  height: number;
}

export interface FlyoutPos {
  left: number;
  top: number;
  side: "right" | "left";
}

/** Overlap onto the parent row so the diagonal travel into the child never
 * crosses a dead gap (works with the hover close delay). */
const GUTTER = 4;
/** Breathing room kept between a flown panel and the viewport edge. */
const MARGIN = 8;

export function flyoutPos(
  parentRow: Rect,
  child: { width: number; height: number },
  vp: Viewport,
): FlyoutPos {
  // Preferred: hang off the parent's right edge, overlapping by GUTTER.
  let left = parentRow.right - GUTTER;
  let side: "right" | "left" = "right";

  const overflowsRight = left + child.width + MARGIN > vp.width;
  const leftFits = parentRow.left - child.width - MARGIN >= 0;
  if (overflowsRight && leftFits) {
    // Flip to the parent's left edge (still overlapping by GUTTER).
    left = parentRow.left - child.width + GUTTER;
    side = "left";
  } else if (overflowsRight) {
    // Neither side fully fits: stay right but clamp so we never go
    // off-screen (the child just slides under the parent's right edge).
    left = vp.width - child.width - MARGIN;
  }

  // Top-align with the parent row; clamp up if the panel would run off the
  // bottom (never above MARGIN).
  let top = parentRow.top;
  if (top + child.height + MARGIN > vp.height) {
    top = Math.max(MARGIN, vp.height - child.height - MARGIN);
  }

  return { left, top, side };
}
