<script lang="ts">
  /**
   * First launch (UI §9.1): an empty Grid with one centered, dimmed line and
   * an Add Folder button. No tour, no carousel, no sample library.
   */
  import { open } from "@tauri-apps/plugin-dialog";
  import * as ipc from "../ipc/commands";
  import { ui } from "../state/app.svelte";

  async function addFolder() {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir !== "string") return;
    const root = await ipc.addRoot(dir);
    await ui.refreshRoots();
    await ui.openFolder(root.rootId, "");
  }
</script>

<div class="first-run">
  <p>Add a folder of photographs.</p>
  <button onclick={() => void addFolder()}>Add Folder</button>
</div>

<style>
  .first-run {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 14px;
  }
  p {
    color: var(--text-faint);
    margin: 0;
  }
</style>
