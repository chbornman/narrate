<script lang="ts">
  /**
   * The left source rail (featureset §3): a PUSH Panel — resizable,
   * width-persisted, `\` toggles (D5). Replaces the old Rail.svelte; the
   * dwell hot-zone and pin affordance are DELETED (no auto-hide fly-outs,
   * ever). Rows render through SourceList over logic/sources.ts sections —
   * collections/saved-searches join as sibling sections in M3 with zero
   * edits here. Right-click on a folder row opens the rail-folder seat.
   * The footer carries the rail's one standing affordance: "Add folder…"
   * (founder, dogfood rounds 1+2 — no Settings round-trip).
   */
  import { ui } from "../../state/app.svelte";
  import { RAIL_WIDTH_KEY } from "../../state/prefs";
  import type { SourceRow } from "../../logic/sources";
  import Panel from "../../primitives/Panel.svelte";
  import SourceList from "./SourceList.svelte";

  const secs = $derived(ui.railSections());

  function onOpen(row: SourceRow) {
    ui.shell.railFocusKey = row.key;
    ui.shell.railFocused = true;
    void ui.perform({ kind: "rail-folder-open", rootId: row.rootId, folder: row.folder });
  }

  function onContextMenu(row: SourceRow, x: number, y: number) {
    ui.shell.openContextMenu("rail-folder", { x, y }, {
      rootId: row.rootId,
      folder: row.folder,
    });
  }
</script>

<Panel
  side="left"
  open={ui.shell.railOpen && !ui.shell.chromeHidden}
  minSize={160}
  maxSize={420}
  persistKey={RAIL_WIDTH_KEY}
  label="Sources"
>
  <div class="rail-body">
    <div class="rows">
      <SourceList
        sections={secs}
        focusKey={ui.shell.railFocusKey}
        currentKey={ui.grid.rootId === null
          ? null
          : `folders:${ui.grid.rootId}:${ui.grid.folder}`}
        onopen={onOpen}
        oncontextmenu={onContextMenu}
      />
    </div>
    <!-- emphasized (a full bordered button against the quiet rows) but
         token-only; also seated on the rail-folder context menu -->
    <button class="add-folder" onclick={() => void ui.perform({ kind: "add-root" })}>
      Add folder…
    </button>
  </div>
</Panel>

<style>
  .rail-body {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .rows {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .add-folder {
    flex: 0 0 auto;
    margin: 8px;
    padding: 6px 10px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .add-folder:hover {
    background: var(--bg-overlay);
  }
</style>
