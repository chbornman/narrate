<script lang="ts">
  /**
   * First launch (UI §9.1): an empty Grid with one centered, dimmed line and
   * an Add Folder button (EmptyState primitive — empty states say the next
   * action, featureset §6). No tour, no carousel, no sample library.
   */
  import { ui } from "../../state/app.svelte";
  import EmptyState from "../../primitives/EmptyState.svelte";

  // Route through the one add-root flow on the root (folder-tree
  // improvements): it owns the picker, the optimistic ingest bridge, AND the
  // refuse + alias handling for an overlapping folder — duplicating it here
  // would re-open the overlap-double-ingest hole.
  async function addFolder() {
    await ui.addRootFromPicker();
  }
</script>

<EmptyState line="Add a folder of photographs.">
  {#snippet action()}
    <button onclick={() => void addFolder()}>Add Folder</button>
  {/snippet}
</EmptyState>
