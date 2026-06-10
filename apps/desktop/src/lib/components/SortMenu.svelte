<script lang="ts">
  /** Sort menu (UI §3.6): the complete v1 set, persisted per folder. */
  import { ui } from "../state/app.svelte";
  import { SORT_MODES } from "../logic/sort";
</script>

<div class="menu" role="menu">
  {#each SORT_MODES as { mode, label } (mode)}
    <button
      role="menuitemradio"
      aria-checked={ui.sort === mode}
      class:active={ui.sort === mode}
      onclick={() => {
        ui.setSort(mode);
        ui.sortMenuOpen = false;
      }}
    >
      {label}
    </button>
  {/each}
</div>

<style>
  .menu {
    position: absolute;
    top: 30px;
    right: 120px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 4px;
    display: flex;
    flex-direction: column;
    min-width: 200px;
    z-index: 40;
    padding: 4px;
  }
  .menu button {
    border: none;
    background: transparent;
    text-align: left;
    color: var(--text-dim);
    padding: 5px 8px;
    border-radius: 3px;
  }
  .menu button:hover {
    background: var(--bg-raised);
    color: var(--text);
  }
  .menu button.active {
    color: var(--text);
  }
</style>
