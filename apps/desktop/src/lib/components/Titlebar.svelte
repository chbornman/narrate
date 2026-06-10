<script lang="ts">
  /**
   * Minimal custom titlebar (UI §2.3: native chrome minimal/hidden on
   * Windows/Linux). A drag strip, the window title, three quiet controls.
   */
  import { getCurrentWindow } from "@tauri-apps/api/window";

  let { title }: { title: string } = $props();

  const win = getCurrentWindow();
  $effect(() => {
    void win.setTitle(title);
  });
</script>

<div class="titlebar" data-tauri-drag-region>
  <span class="title" data-tauri-drag-region>{title}</span>
  <div class="controls">
    <button aria-label="Minimize" onclick={() => void win.minimize()}>–</button>
    <button aria-label="Maximize" onclick={() => void win.toggleMaximize()}>□</button>
    <button aria-label="Close" onclick={() => void win.close()}>×</button>
  </div>
</div>

<style>
  .titlebar {
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 4px 0 10px;
    background: var(--bg);
    flex: 0 0 auto;
  }
  .title {
    color: var(--text-dim);
    font-size: 12px;
    pointer-events: none;
  }
  .controls {
    display: flex;
    gap: 2px;
  }
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
  }
  .controls button:hover {
    background: var(--bg-raised);
    color: var(--text);
  }
</style>
