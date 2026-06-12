/**
 * Source-rail row model (featureset §3), extracted from the old
 * Rail.svelte as pure functions: SourceSection providers — folders today;
 * collections and saved searches join as SIBLING SECTIONS in M3 with zero
 * rail edits (SourceList renders sections generically).
 */
import type { CollectionDto, FolderNode, RootDto } from "../types/dto";

export interface SourceRow {
  /** Stable key: `${sectionId}:${rootId}:${folder}`. */
  key: string;
  label: string;
  rootId: string;
  folder: string;
  depth: number;
  isRoot: boolean;
  hasChildren: boolean;
  expanded: boolean;
  offline: boolean;
}

export interface SourceSection {
  /** M3 widens this union: "collections" | "saved-searches" (B71). */
  id: "folders";
  label: string;
  rows: SourceRow[];
}

export interface SourcesInput {
  roots: RootDto[];
  /** Folder tree of the CURRENT root (the only one expanded, as before). */
  tree: FolderNode[];
  treeRootId: string | null;
  /** Collapsed row keys. */
  collapsed: ReadonlySet<string>;
}

export function rowKey(rootId: string, folder: string): string {
  return `folders:${rootId}:${folder}`;
}

function walk(
  rootId: string,
  nodes: FolderNode[],
  depth: number,
  collapsed: ReadonlySet<string>,
  out: SourceRow[],
) {
  for (const n of nodes) {
    const key = rowKey(rootId, n.relPath);
    const expanded = !collapsed.has(key);
    out.push({
      key,
      label: n.name,
      rootId,
      folder: n.relPath,
      depth,
      isRoot: false,
      hasChildren: n.children.length > 0,
      expanded,
      offline: false,
    });
    if (expanded) walk(rootId, n.children, depth + 1, collapsed, out);
  }
}

export function folderSection(input: SourcesInput): SourceSection {
  const rows: SourceRow[] = [];
  for (const r of input.roots) {
    const key = rowKey(r.rootId, "");
    const hasChildren = input.treeRootId === r.rootId && input.tree.length > 0;
    const expanded = !input.collapsed.has(key);
    rows.push({
      key,
      label: r.displayName,
      rootId: r.rootId,
      folder: "",
      depth: 0,
      isRoot: true,
      hasChildren,
      expanded,
      offline: !r.online,
    });
    if (hasChildren && expanded)
      walk(r.rootId, input.tree, 1, input.collapsed, rows);
  }
  return { id: "folders", label: "Folders", rows };
}

/** Provider aggregation — M3 appends collection/saved-search sections here. */
export function sections(input: SourcesInput): SourceSection[] {
  return [folderSection(input)];
}

export function flatRows(secs: readonly SourceSection[]): SourceRow[] {
  return secs.flatMap((s) => s.rows);
}

// ---------------------------------------------------------------------------
// Collections rows (B71 — the rail's Collections tab, sibling of folders)
// ---------------------------------------------------------------------------

export interface CollectionRow {
  /** Stable key: `collections:${id}` — same namespace shape as folder keys. */
  key: string;
  id: string;
  label: string;
  memberCount: number;
  status: string;
  /** Shelved/done collections render status-dimmed, never hidden. */
  dim: boolean;
}

export function collectionKey(id: string): string {
  return `collections:${id}`;
}

/** Backend list order is kept (id order = creation order): the rail shows
 * collections as the user accreted them — no re-sorting surprises. */
export function collectionRows(collections: readonly CollectionDto[]): CollectionRow[] {
  return collections.map((c) => ({
    key: collectionKey(c.id),
    id: c.id,
    label: c.name,
    memberCount: c.memberCount,
    status: c.status,
    dim: c.status !== "active",
  }));
}

/** ↑/↓ over the flattened rows; null focus starts at the edge. Generic
 * over the key so folder AND collection rows share one mover. */
export function moveFocus(
  rows: readonly { key: string }[],
  currentKey: string | null,
  dir: "up" | "down",
): string | null {
  if (rows.length === 0) return null;
  const i = currentKey === null ? -1 : rows.findIndex((r) => r.key === currentKey);
  if (i < 0) return rows[dir === "down" ? 0 : rows.length - 1].key;
  const next = Math.max(0, Math.min(rows.length - 1, i + (dir === "down" ? 1 : -1)));
  return rows[next].key;
}

/** ←/→ collapse/expand the focused row (returns the new collapsed set). */
export function toggleExpand(
  collapsed: ReadonlySet<string>,
  row: SourceRow,
  dir: "left" | "right",
): ReadonlySet<string> {
  const next = new Set(collapsed);
  if (dir === "left") next.add(row.key);
  else next.delete(row.key);
  return next;
}
