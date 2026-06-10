/**
 * Source-rail row model (featureset §3), extracted from the old
 * Rail.svelte as pure functions: SourceSection providers — folders today;
 * projects and saved searches join as SIBLING SECTIONS in M3 with zero
 * rail edits (SourceList renders sections generically).
 */
import type { FolderNode, RootDto } from "../types/dto";

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
  /** M3 widens this union: "projects" | "saved-searches". */
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

/** Provider aggregation — M3 appends project/saved-search sections here. */
export function sections(input: SourcesInput): SourceSection[] {
  return [folderSection(input)];
}

export function flatRows(secs: readonly SourceSection[]): SourceRow[] {
  return secs.flatMap((s) => s.rows);
}

/** ↑/↓ over the flattened rows; null focus starts at the edge. */
export function moveFocus(
  rows: readonly SourceRow[],
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
