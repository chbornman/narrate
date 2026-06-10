<script lang="ts">
  /**
   * Popup menu (primitive) — renders a MenuModel inside a Popover (the
   * host provides the Popover). Keyboard behavior lives in the paired pure
   * controller (menu.ts); rows carry Actions, never handlers or key
   * strings (key hints render through KeyHint). Consumers: all four
   * context-menu seats, sort ▾, every submenu, M2a tool menus.
   */
  import type { Action } from "../logic/keymap";
  import type { MenuModel, MenuRow } from "../actions/menus";
  import { navInit, navKey, rowsAt, type MenuNav } from "./menu";
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

  function activate(row: MenuRow) {
    if (row.disabled === true) return;
    if (row.action !== undefined) {
      onaction(row.action);
      onclose();
    }
  }

  function onKeydown(e: KeyboardEvent) {
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

  const level = $derived(rowsAt(model.rows, nav.path) as readonly MenuRow[]);
  const parentRows = $derived(
    nav.path.length > 0
      ? (rowsAt(model.rows, nav.path.slice(0, -1)) as readonly MenuRow[])
      : null,
  );
</script>

<svelte:window onkeydown={onKeydown} />

<div class="menu" role="menu">
  {#if parentRows !== null}
    <!-- One open submenu level renders in place (quiet, no cascade). -->
    <div class="crumb">{parentRows[nav.path[nav.path.length - 1]]?.verb}</div>
  {/if}
  {#each level as row, i (i)}
    {#if row.kind === "separator"}
      <hr />
    {:else}
      <button
        role={row.kind === "radio"
          ? "menuitemradio"
          : row.checked !== undefined
            ? "menuitemcheckbox"
            : "menuitem"}
        aria-checked={row.kind === "radio" || row.checked !== undefined
          ? row.checked === true
          : undefined}
        class:focused={nav.focus === i}
        class:checked={row.checked === true}
        disabled={row.disabled === true}
        onpointerenter={() => (nav = { ...nav, focus: i })}
        onclick={() => {
          if (row.kind === "submenu") {
            const r = navKey(model.rows, { ...nav, focus: i }, "ArrowRight");
            nav = r.nav;
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
        {#if row.kind === "submenu"}<span class="sub">▸</span>{/if}
      </button>
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
  .crumb {
    color: var(--text-faint);
    font-size: 11px;
    padding: 3px 8px;
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
  }
</style>
