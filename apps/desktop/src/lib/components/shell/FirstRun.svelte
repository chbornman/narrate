<script lang="ts">
  /**
   * First launch (UI §9.1): an empty Grid with one centered, dimmed line and
   * an Add Folder button (EmptyState primitive — empty states say the next
   * action, featureset §6). No tour, no carousel, no sample library.
   */
  import { open } from "@tauri-apps/plugin-dialog";
  import * as ipc from "../../ipc/commands";
  import { ui } from "../../state/app.svelte";
  import EmptyState from "../../primitives/EmptyState.svelte";

  async function addFolder() {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir !== "string") return;
    const root = await ipc.addRoot(dir);
    await ui.refreshRoots();
    await ui.openFolder(root.rootId, "");
  }
</script>

<EmptyState line="Add a folder of photographs.">
  {#snippet action()}
    <button onclick={() => void addFolder()}>Add Folder</button>
  {/snippet}
</EmptyState>
