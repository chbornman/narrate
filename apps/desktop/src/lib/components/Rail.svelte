<script lang="ts">
  /**
   * The folder rail (UI §3.2): collapsible, auto-hides, overlays — never
   * reflows the grid. Pointer dwell at the left edge (~150 ms) or Tab
   * summons it; folder selection or pointer-leave hides it unless pinned.
   */
  import { ui } from "../state/app.svelte";
  import { saveRailPinned } from "../state/prefs";
  import type { FolderNode } from "../types/dto";

  interface Row {
    label: string;
    rootId: string;
    folder: string;
    depth: number;
    isRoot: boolean;
    hasChildren: boolean;
    offline: boolean;
  }

  let focus = $state(0);
  let collapsed = $state<Set<string>>(new Set());

  function flatten(): Row[] {
    const rows: Row[] = [];
    for (const r of ui.roots) {
      rows.push({
        label: r.displayName,
        rootId: r.rootId,
        folder: "",
        depth: 0,
        isRoot: true,
        hasChildren: ui.rootId === r.rootId && ui.tree.length > 0,
        offline: !r.online,
      });
      if (ui.rootId === r.rootId && !collapsed.has(`${r.rootId}:`)) {
        walk(r.rootId, ui.tree, 1, rows);
      }
    }
    return rows;
  }

  function walk(rootId: string, nodes: FolderNode[], depth: number, out: Row[]) {
    for (const n of nodes) {
      out.push({
        label: n.name,
        rootId,
        folder: n.relPath,
        depth,
        isRoot: false,
        hasChildren: n.children.length > 0,
        offline: false,
      });
      if (!collapsed.has(`${rootId}:${n.relPath}`)) {
        walk(rootId, n.children, depth + 1, out);
      }
    }
  }

  const rows = $derived(flatten());

  export function nav(dir: "up" | "down" | "left" | "right") {
    if (dir === "up") focus = Math.max(0, focus - 1);
    else if (dir === "down") focus = Math.min(rows.length - 1, focus + 1);
    else {
      const row = rows[focus];
      if (row === undefined) return;
      const key = `${row.rootId}:${row.folder}`;
      const next = new Set(collapsed);
      if (dir === "left") next.add(key);
      else next.delete(key);
      collapsed = next;
    }
  }

  export function enter() {
    const row = rows[focus];
    if (row === undefined) return;
    void open(row);
  }

  async function open(row: Row) {
    await ui.openFolder(row.rootId, row.folder);
  }
</script>

<nav
  class="rail"
  aria-label="Watched folders"
  onpointerleave={() => {
    if (!ui.railPinned) ui.railOpen = false;
  }}
>
  <div class="pin">
    <button
      class:active={ui.railPinned}
      aria-label="Pin rail"
      onclick={() => {
        ui.railPinned = !ui.railPinned;
        saveRailPinned(ui.railPinned);
      }}>⌖</button
    >
  </div>
  {#each rows as row, i (row.rootId + ":" + row.folder)}
    <button
      class="row"
      class:focused={i === focus}
      class:current={row.rootId === ui.rootId && row.folder === ui.folder}
      style:padding-left="{10 + row.depth * 14}px"
      onclick={() => void open(row)}
    >
      <span class="label">{row.label}</span>
      {#if row.offline}<span class="badge">⏏</span>{/if}
    </button>
  {/each}
</nav>

<style>
  .rail {
    position: absolute;
    top: 0;
    left: 0;
    bottom: 0;
    width: 220px;
    background: var(--bg-overlay);
    border-right: 1px solid var(--chrome);
    overflow-y: auto;
    z-index: 30; /* overlay, never reflows the grid (§3.2) */
    padding: 4px 0 12px;
  }
  .pin {
    display: flex;
    justify-content: flex-end;
    padding: 2px 6px;
  }
  .pin button {
    border: none;
    background: transparent;
    color: var(--text-faint);
    padding: 2px 6px;
  }
  .pin button.active {
    color: var(--text);
  }
  .row {
    display: flex;
    width: 100%;
    align-items: center;
    gap: 6px;
    border: none;
    background: transparent;
    color: var(--text-dim);
    text-align: left;
    padding: 4px 10px;
    border-radius: 0;
  }
  .row:hover,
  .row.focused {
    background: var(--bg-raised);
    color: var(--text);
  }
  .row.current {
    color: var(--text);
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    color: var(--text-faint);
    font-size: 11px;
  }
</style>
