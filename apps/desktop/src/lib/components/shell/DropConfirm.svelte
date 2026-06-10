<script lang="ts">
  /**
   * Drag-folder → register-root confirmation (featureset §6) over the
   * Sheet primitive — one of the two enumerated Sheet instances
   * (cheatsheet, drop-confirm; new instances are spec changes). Esc routes
   * through the escape layer (close-drop-confirm); scrim and [×] dismiss
   * via the Sheet. A refused path (not a folder, offline volume) reports
   * one inline line and keeps the sheet open — never a toast (R5/§7.5).
   */
  import { ui } from "../../state/app.svelte";

  import Sheet from "../../primitives/Sheet.svelte";

  const paths = $derived(ui.dropPaths ?? []);

  function baseName(path: string): string {
    const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
    return parts[parts.length - 1] || path;
  }
</script>

<Sheet open={ui.dropPaths !== null} onclose={() => ui.cancelDrop()} label="Watch folder">
  <div class="drop-confirm">
    <h2>{paths.length === 1 ? "Watch this folder?" : "Watch these folders?"}</h2>
    {#each paths as path (path)}
      <div class="row">
        <span class="name">{baseName(path)}</span>
        <span class="path">{path}</span>
      </div>
    {/each}
    <p class="note">
      Photoproof indexes the photographs in place — files are never moved,
      modified, or deleted.
    </p>
    {#if ui.dropError !== null}
      <p class="error">{ui.dropError}</p>
    {/if}
    <div class="actions">
      <button onclick={() => ui.cancelDrop()}>Cancel</button>
      <button class="primary" onclick={() => void ui.confirmDrop()}>
        {paths.length === 1 ? "Watch folder" : "Watch folders"}
      </button>
    </div>
  </div>
</Sheet>

<style>
  .drop-confirm {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: min(440px, 80vw);
  }
  h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--text);
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .name {
    color: var(--text);
  }
  .path {
    color: var(--text-faint);
    font-size: 11px;
    word-break: break-all;
  }
  .note {
    margin: 0;
    color: var(--text-dim);
  }
  .error {
    margin: 0;
    color: var(--text-dim);
    font-style: italic;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .primary {
    border-color: var(--chrome-strong);
  }
</style>
