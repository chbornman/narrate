<script lang="ts">
  /**
   * Grid header (extracted from the old App.svelte): folder name, sort ▾
   * (opens the ONE context-menu machinery — SortMenu.svelte is deleted),
   * thumb-size slider synced with Ctrl+wheel and -/=. Tooltips resolve
   * from the registry (tooltip.ts).
   */
  // Lucide chevron replaces the text ▾ (BACKLOG "Adopt Lucide icons").
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import { ui } from "../../state/app.svelte";
  import { THUMB_STEPS } from "../../logic/sort";
  import { tooltip } from "../../primitives/tooltip";

  let sortBtn: HTMLButtonElement | undefined = $state();

  function openSort() {
    if (ui.shell.contextMenu?.seat === "sort") {
      ui.shell.closeContextMenu();
      return;
    }
    const r = sortBtn?.getBoundingClientRect();
    ui.shell.openContextMenu("sort", r ? { x: r.left, y: r.bottom + 2 } : null);
  }
</script>

<header class="grid-header">
  <span class="folder-name">▍{ui.folderName}</span>
  <span class="spacer"></span>
  <button
    bind:this={sortBtn}
    class="quiet"
    onclick={openSort}
    {@attach tooltip({ actionId: "open-sort-menu", verb: "Sort" })}
  >
    sort <ChevronDown size={12} />
  </button>
  <input
    class="thumb-slider"
    type="range"
    min="0"
    max={THUMB_STEPS.length - 1}
    step="1"
    value={ui.grid.thumbStep}
    aria-label="Thumbnail size"
    oninput={(e) => ui.grid.setThumbStep(Number(e.currentTarget.value))}
    {@attach tooltip({ actionId: "thumb-size", verb: "Thumbnail size" })}
  />
</header>

<style>
  .grid-header {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 30px;
    padding: 0 12px;
  }
  .folder-name {
    color: var(--text-dim);
  }
  .spacer {
    flex: 1;
  }
  .quiet {
    border: none;
    background: transparent;
    color: var(--text-faint);
    display: inline-flex; /* keep the chevron svg on the text's midline */
    align-items: center;
    gap: 2px;
  }
  .quiet:hover {
    color: var(--text);
  }
  .thumb-slider {
    width: 90px;
    accent-color: var(--text-faint);
    background: transparent;
    border: none;
    padding: 0;
  }
</style>
