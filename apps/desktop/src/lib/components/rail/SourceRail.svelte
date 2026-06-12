<script lang="ts">
  /**
   * The left source rail (featureset §3): a PUSH Panel — resizable,
   * width-persisted, `\` toggles (D5). Two PEER tabs (founder, June 2026):
   * Folders and Collections — collections are the point (intent, gathered
   * context); folders are just where files happen to sit. Folder rows
   * render through SourceList over logic/sources.ts sections; collection
   * rows through CollectionList over the collections snapshot. Right-click
   * on a folder row opens the rail-folder seat. The footer carries the
   * visible tab's one standing affordance: "Add folder…" (founder, dogfood
   * rounds 1+2 — no Settings round-trip) or the inline "New collection…"
   * input (same idiom, text instead of a picker).
   */
  import { tick } from "svelte";
  import { ui } from "../../state/app.svelte";
  import { collectionKey } from "../../logic/sources";
  import type { CollectionRow, SourceRow } from "../../logic/sources";
  import Panel from "../../primitives/Panel.svelte";
  import SourceList from "./SourceList.svelte";
  import CollectionList from "./CollectionList.svelte";

  const secs = $derived(ui.railSections());
  const collectionRows = $derived(ui.railCollectionRows());
  const tab = $derived(ui.shell.railTab);

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

  function onOpenCollection(row: CollectionRow) {
    ui.shell.railFocusKey = row.key;
    ui.shell.railFocused = true;
    void ui.perform({ kind: "collection-open", id: row.id });
  }

  // ---- inline create (the rail's footer affordance, collections tab) -----
  let creating = $state(false);
  let draft = $state("");
  let inputEl: HTMLInputElement | undefined = $state();

  async function beginCreate() {
    creating = true;
    draft = "";
    await tick(); // the input mounts on the flag flip
    inputEl?.focus();
  }

  function onCreateKeydown(e: KeyboardEvent) {
    // The input owns its keys entirely: Enter commits, Esc cancels, and
    // nothing leaks to the global keymap (the WelcomeCard containment
    // pattern — without this, Esc would also peel an escape layer).
    e.stopPropagation();
    if (e.key === "Enter") {
      const name = draft;
      creating = false;
      draft = "";
      void ui.createCollection(name);
    } else if (e.key === "Escape") {
      creating = false;
      draft = "";
    }
  }
</script>

<!-- lights-out hides this through the root's panel snapshot, not a gate -->
<Panel id="rail" edge="left" open={ui.shell.railOpen} label="Sources">
  <div class="rail-body">
    <!-- two PEER tabs; the strip is the rail's one piece of standing chrome -->
    <div class="tabs" role="tablist" aria-label="Source kind">
      <button
        role="tab"
        aria-selected={tab === "folders"}
        class:current={tab === "folders"}
        onclick={() => ui.shell.setRailTab("folders")}
      >
        Folders
      </button>
      <button
        role="tab"
        aria-selected={tab === "collections"}
        class:current={tab === "collections"}
        onclick={() => ui.shell.setRailTab("collections")}
      >
        Collections
      </button>
    </div>

    <div class="rows">
      {#if tab === "folders"}
        <SourceList
          sections={secs}
          focusKey={ui.shell.railFocusKey}
          currentKey={ui.grid.rootId === null
            ? null
            : `folders:${ui.grid.rootId}:${ui.grid.folder}`}
          onopen={onOpen}
          oncontextmenu={onContextMenu}
        />
      {:else}
        <CollectionList
          rows={collectionRows}
          focusKey={ui.shell.railFocusKey}
          currentKey={ui.collectionId === null ? null : collectionKey(ui.collectionId)}
          onopen={onOpenCollection}
        />
      {/if}
    </div>

    <!-- emphasized (a full bordered button against the quiet rows) but
         token-only; the folder verb is also seated on the rail-folder menu -->
    {#if tab === "folders"}
      <button class="footer-verb" onclick={() => void ui.perform({ kind: "add-root" })}>
        Add folder…
      </button>
    {:else if creating}
      <input
        bind:this={inputEl}
        bind:value={draft}
        class="footer-input"
        placeholder="Collection name"
        aria-label="New collection name"
        onkeydown={onCreateKeydown}
        onblur={() => (creating = false)}
      />
    {:else}
      <button class="footer-verb" onclick={() => void beginCreate()}>
        New collection…
      </button>
    {/if}
  </div>
</Panel>

<style>
  .rail-body {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .tabs {
    flex: 0 0 auto;
    display: flex;
    gap: 2px;
    padding: 6px 8px 0;
  }
  .tabs button {
    flex: 1;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    padding: 4px 6px;
    border-bottom: 1px solid transparent;
    border-radius: 0;
  }
  .tabs button:hover {
    color: var(--text-dim);
  }
  .tabs button.current {
    color: var(--text);
    border-bottom-color: var(--chrome-strong);
  }
  .rows {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .footer-verb {
    flex: 0 0 auto;
    margin: 8px;
    padding: 6px 10px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .footer-verb:hover {
    background: var(--bg-overlay);
  }
  .footer-input {
    flex: 0 0 auto;
    margin: 8px;
    padding: 6px 10px;
    background: var(--bg-raised);
    border: 1px solid var(--chrome-strong);
    border-radius: 4px;
    color: var(--text);
    font-size: 12px;
  }
</style>
