<script lang="ts">
  /**
   * Popup menu (primitive) — renders a MenuModel inside a Popover (the
   * host provides the Popover). Keyboard behavior lives in the paired pure
   * controller (menu.ts); rows carry Actions, never handlers or key
   * strings (key hints render through KeyHint). Consumers: all four
   * context-menu seats, sort ▾, every submenu, M2a tool menus.
   *
   * Submenus CASCADE: the open chain (nav.path, owned by the keyboard
   * controller) renders as columns flying out to the side — root at [], a
   * child panel per path prefix. Placement is the pure flyout.ts; opening
   * and closing on hover is timed by hoverintent.ts. The keyboard controller
   * (menu.ts) is the single source of which submenu is open, so pointer and
   * keyboard drive the SAME state.
   */
  // Lucide chevron for the submenu marker (BACKLOG "Adopt Lucide icons").
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import Check from "@lucide/svelte/icons/check";
  import { onDestroy } from "svelte";
  import type { Action } from "../logic/keymap";
  import type { MenuModel, MenuRow } from "../actions/menus";
  import { navInit, navKey, rowsAt, type MenuNav } from "./menu";
  import { flyoutPos } from "./flyout";
  import { HOVER_CLOSE_MS, HOVER_OPEN_MS, HoverTimer } from "./hoverintent";
  import { copyFlash } from "./copyflash.svelte";
  import KeyHint from "./KeyHint.svelte";

  let {
    model,
    onaction,
    onclose,
  }: {
    model: MenuModel;
    onaction: (a: Action) => void;
    onclose: () => void;
  } = $props();

  let nav = $state<MenuNav>(navInit([]));
  $effect(() => {
    nav = navInit(model.rows);
  });

  /** How long a copy row holds the menu open for its check (shorter than
   * COPY_FLASH_MS: the menu closes while the check is still showing —
   * closing INTO the flash reads as "done", lingering past it as lag). */
  const COPY_CONFIRM_CLOSE_MS = 900;

  /** The armed deferred close, if any. WHY it must be cancelable: onclose
   * nulls the host's ONE shared contextMenu slot, so a timer outliving
   * THIS menu (Esc / outside-click dismissed it early) would fire into
   * whatever menu the user opened next and close it mid-read. */
  let closeTimer: ReturnType<typeof setTimeout> | undefined;
  let closePending = false;

  /** Hover-intent timers (one open-intent, one close-intent) — same
   * clear-on-destroy discipline as the copy-close timer above. */
  const openTimer = new HoverTimer();
  const closeChildTimer = new HoverTimer();
  onDestroy(() => {
    clearTimeout(closeTimer);
    openTimer.cancel();
    closeChildTimer.cancel();
  });

  function activate(row: MenuRow) {
    // While the deferred close is pending the menu is a dead seat showing
    // its confirmation: a second activation would re-copy and stack a
    // second close timer (double-click on the copy row does exactly this).
    if (closePending) return;
    if (row.disabled === true) return;
    if (row.action !== undefined) {
      onaction(row.action);
      if (row.flashKey !== undefined) {
        // Copy rows confirm inline (BACKLOG "Copy actions confirm
        // themselves"): closing on activate would discard the only seat
        // the check can flash on, so the close waits out the flash. The
        // check itself renders only when the register reports the write
        // landed — a failed copy shows nothing and the menu just closes.
        closePending = true;
        closeTimer = setTimeout(onclose, COPY_CONFIRM_CLOSE_MS);
        return;
      }
      onclose();
    }
  }

  function onKeydown(e: KeyboardEvent) {
    // Dead seat during the confirmation hold: stop trapping the keyboard
    // (no preventDefault) so Enter cannot re-activate a row and the next
    // keystroke is the app's to route, not dead menu focus.
    if (closePending) return;
    if (e.key === "Escape") return; // escape layer 2 closes (host routes)
    const r = navKey(model.rows, nav, e.key, e.timeStamp);
    nav = r.nav;
    if (r.activate !== undefined) activate(r.activate as MenuRow);
    if (
      e.key === "Enter" ||
      e.key.startsWith("Arrow") ||
      (e.key.length === 1 && e.key !== " ")
    )
      e.preventDefault();
  }

  /** Open the submenu at (path, index) the SAME way ArrowRight does, so the
   * pointer and the keyboard share one open chain. */
  function openSubmenu(path: number[], i: number) {
    const r = navKey(model.rows, { ...nav, path, focus: i }, "ArrowRight");
    nav = r.nav;
  }

  /** Collapse the open chain to `depth` columns (drop deeper panels). */
  function collapseTo(depth: number) {
    if (nav.path.length > depth) nav = { ...nav, path: nav.path.slice(0, depth) };
  }

  // The open chain as columns: root rows at [], then one panel per prefix of
  // nav.path. rowsAt walks the model, so collapsing nav.path drops panels
  // automatically (no view-side bookkeeping).
  const columns = $derived.by(() => {
    const out: { rows: readonly MenuRow[]; path: number[] }[] = [
      { rows: model.rows, path: [] },
    ];
    for (let d = 1; d <= nav.path.length; d++) {
      const prefix = nav.path.slice(0, d);
      out.push({
        rows: rowsAt(model.rows, prefix) as readonly MenuRow[],
        path: prefix,
      });
    }
    return out;
  });

  /** True when this row's panel path is the one the keyboard focus lives in
   * — focus styling lands only on the active (deepest open) column. */
  function isActivePath(path: number[]): boolean {
    return (
      path.length === nav.path.length && path.every((v, i) => v === nav.path[i])
    );
  }

  // Row element refs keyed by "<path>|<index>" — a child panel reads its
  // parent submenu-row's rect from here for placement.
  let rowEls = $state<Record<string, HTMLElement | undefined>>({});
  const rowKey = (path: number[], i: number) => `${path.join(",")}|${i}`;

  // Child panel element refs (non-root) and their computed fixed positions,
  // keyed by panel depth (the chain is linear: at most one panel per depth).
  let panelEls = $state<Record<number, HTMLElement | undefined>>({});
  let panelPos = $state<Record<number, { left: number; top: number }>>({});

  // After layout, place each non-root panel beside its parent submenu row.
  // Mirrors Popover.svelte's post-mount measure-then-position effect.
  $effect(() => {
    void columns; // re-run when the chain changes
    const next: Record<number, { left: number; top: number }> = {};
    for (let depth = 1; depth < columns.length; depth++) {
      const col = columns[depth];
      // The parent row that opened this panel: its index is the last path
      // segment, under the panel one level up.
      const parentPath = col.path.slice(0, -1);
      const parentIdx = col.path[col.path.length - 1];
      const rowEl = rowEls[rowKey(parentPath, parentIdx)];
      const panelEl = panelEls[depth];
      if (rowEl === undefined || panelEl === undefined) continue;
      const r = rowEl.getBoundingClientRect();
      const size = panelEl.getBoundingClientRect();
      const pos = flyoutPos(
        {
          left: r.left,
          top: r.top,
          right: r.right,
          bottom: r.bottom,
          width: r.width,
          height: r.height,
        },
        { width: size.width, height: size.height },
        { width: window.innerWidth, height: window.innerHeight },
      );
      next[depth] = { left: pos.left, top: pos.top };
    }
    panelPos = next;
  });
</script>

<svelte:window onkeydown={onKeydown} />

{#snippet panel(rows: readonly MenuRow[], path: number[])}
  {#each rows as row, i (i)}
    {#if row.kind === "separator"}
      <hr />
    {:else}
      <button
        bind:this={rowEls[rowKey(path, i)]}
        role={row.kind === "radio"
          ? "menuitemradio"
          : row.checked !== undefined
            ? "menuitemcheckbox"
            : "menuitem"}
        aria-checked={row.kind === "radio" || row.checked !== undefined
          ? row.checked === true
          : undefined}
        class:focused={isActivePath(path) && nav.focus === i}
        class:checked={row.checked === true}
        disabled={row.disabled === true}
        onpointerenter={() => {
          if (row.kind === "submenu" && row.disabled !== true) {
            // Hover-intent: arm an open after a beat (a passing pointer must
            // not flicker it open). Cancel any pending close of a sibling's
            // open child.
            closeChildTimer.cancel();
            openTimer.start(() => openSubmenu(path, i), HOVER_OPEN_MS);
          } else {
            // Non-submenu row: follow focus immediately, and (since the
            // pointer left whatever submenu was open at this level) defer
            // closing the open child so a diagonal miss does not slam it.
            openTimer.cancel();
            nav = { ...nav, path, focus: i };
            if (nav.path.length > path.length)
              closeChildTimer.start(
                () => collapseTo(path.length),
                HOVER_CLOSE_MS,
              );
          }
        }}
        onpointerleave={() => {
          // Leaving a submenu row before it opened: drop the pending open.
          if (row.kind === "submenu") openTimer.cancel();
        }}
        onclick={() => {
          if (row.kind === "submenu") {
            openTimer.cancel();
            openSubmenu(path, i);
          } else activate(row);
        }}
      >
        {#if row.kind === "radio" || row.checked !== undefined}
          <span class="check">{row.checked === true ? "•" : ""}</span>
        {/if}
        <span class="verb">{row.verb}</span>
        <span class="spacer"></span>
        {#if row.keyHint !== undefined}
          <KeyHint chord={row.keyHint} />
        {/if}
        {#if row.flashKey !== undefined && copyFlash.key === row.flashKey}
          <span class="copied"><Check size={12} aria-hidden="true" /></span>
        {/if}
        {#if row.kind === "submenu"}<span class="sub"
            ><ChevronRight size={12} /></span
          >{/if}
      </button>
    {/if}
  {/each}
{/snippet}

<!-- Flyout panels are DOM descendants of this root so Popover's outside-click
     check (el.contains) still treats them as inside — Popover needs no change.
     Each child column is position:fixed, placed beside its parent row. -->
<div class="menu" role="menu">
  {#each columns as col, depth (depth)}
    {#if depth === 0}
      {@render panel(col.rows, col.path)}
    {:else}
      <div
        bind:this={panelEls[depth]}
        class="flyout"
        role="presentation"
        style:left={panelPos[depth] !== undefined
          ? `${panelPos[depth].left}px`
          : undefined}
        style:top={panelPos[depth] !== undefined
          ? `${panelPos[depth].top}px`
          : undefined}
        onpointerenter={() => {
          // Pointer reached the child panel: cancel the deferred close that
          // leaving the parent row armed.
          closeChildTimer.cancel();
        }}
      >
        {@render panel(col.rows, col.path)}
      </div>
    {/if}
  {/each}
</div>

<style>
  .menu {
    display: flex;
    flex-direction: column;
    min-width: 200px;
    padding: 4px;
  }
  /* A flown submenu column: fixed beside its parent row, one layer above the
     Popover (z-index 70) so it is never occluded by its own host. Inherits
     the overlay chrome so it reads as a continuation of the menu. */
  .flyout {
    position: fixed;
    z-index: 71;
    display: flex;
    flex-direction: column;
    min-width: 200px;
    padding: 4px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 6px;
  }
  hr {
    border: none;
    border-top: 1px solid var(--chrome);
    margin: 4px 2px;
  }
  .menu button {
    display: flex;
    align-items: center;
    gap: 6px;
    border: none;
    background: transparent;
    text-align: left;
    color: var(--text-dim);
    padding: 5px 8px;
    border-radius: 3px;
  }
  .menu button.focused,
  .menu button:hover:not(:disabled) {
    background: var(--bg-raised);
    color: var(--text);
  }
  .menu button.checked {
    color: var(--text);
  }
  .menu button:disabled {
    color: var(--text-faint);
    cursor: default;
  }
  .check {
    width: 10px;
    flex: 0 0 auto;
  }
  .verb {
    white-space: nowrap;
  }
  .spacer {
    flex: 1;
    min-width: 14px;
  }
  .sub {
    color: var(--text-faint);
    display: inline-flex; /* svg baseline → flex centering */
    align-items: center;
  }
  /* The copy-confirmation check: inherits the row's text color — the
   * glyph appearing is the whole signal, no extra emphasis. */
  .copied {
    display: inline-flex;
    align-items: center;
  }
</style>
