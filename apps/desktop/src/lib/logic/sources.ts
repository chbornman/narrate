/**
 * Source-rail row models (featureset §3), extracted from the old
 * Rail.svelte as pure functions. Folders render as SourceSection rows
 * (SourceList); collections render as their own flat CollectionRow list
 * (CollectionList) on a PEER TAB of the rail — the founder's June 2026
 * direction (B71: collections are the point, folders are mechanical),
 * which SUPERSEDED the earlier plan of collections joining as sibling
 * sections. The two row shapes stay separate providers; only moveFocus
 * is shared (generic over `key`). Saved searches, when they land, get
 * the same treatment: a peer surface, not a folders section.
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
  /** Folder tree of the CURRENT root (the only one expanded, as before).
   * Already FILTERED by the caller when a folder filter is active (the rail
   * passes filterTree(tree, query)), so the row builder stays pure. */
  tree: FolderNode[];
  treeRootId: string | null;
  /** Collapsed row keys. */
  collapsed: ReadonlySet<string>;
  /** Lazy deep-tree ergonomics (folder-tree improvements): folders deeper
   * than this auto-COLLAPSE so a deep root does not render its whole tree
   * eagerly — the user expands the branch they want. An explicit expand
   * (the key removed from `collapsed` AND added to `expanded`) overrides.
   * When a filter is active the rail raises this past any depth so every
   * surviving match shows. */
  autoExpandDepth: number;
  /** Rows the user EXPLICITLY opened past the auto-collapse depth (their
   * keys). Lets a hand-opened deep branch stay open across re-renders. */
  expanded: ReadonlySet<string>;
}

export function rowKey(rootId: string, folder: string): string {
  return `folders:${rootId}:${folder}`;
}

/** Is a row expanded? It must not be user-collapsed, and either sit within
 * the auto-expand depth or have been explicitly opened by the user (lazy
 * deep-tree ergonomics: deep branches stay closed until asked for). */
function isExpanded(
  key: string,
  depth: number,
  input: Pick<SourcesInput, "collapsed" | "expanded" | "autoExpandDepth">,
): boolean {
  if (input.collapsed.has(key)) return false;
  return depth < input.autoExpandDepth || input.expanded.has(key);
}

function walk(
  rootId: string,
  nodes: FolderNode[],
  depth: number,
  input: SourcesInput,
  out: SourceRow[],
) {
  for (const n of nodes) {
    const key = rowKey(rootId, n.relPath);
    const expanded = isExpanded(key, depth, input);
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
    if (expanded) walk(rootId, n.children, depth + 1, input, out);
  }
}

export function folderSection(input: SourcesInput): SourceSection {
  const rows: SourceRow[] = [];
  for (const r of input.roots) {
    const key = rowKey(r.rootId, "");
    const hasChildren = input.treeRootId === r.rootId && input.tree.length > 0;
    // The root row honors the user-collapse only (it is always within the
    // auto-expand depth — a root with a loaded tree should show its top
    // level): a deep root lazily collapses its DESCENDANTS, not its head.
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
    if (hasChildren && expanded) walk(r.rootId, input.tree, 1, input, rows);
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

// ---------------------------------------------------------------------------
// Folder filter / jump-to-folder (folder-tree improvements)
//
// A deep or 30-root library is unnavigable by scrolling. The filter input on
// the Folders tab types to narrow the CURRENT root's tree: only folders whose
// name matches (case-insensitively) survive, along with every ANCESTOR of a
// match so the path to it stays reachable, and "jump" focuses the first match.
// Pure functions here; the rail wires them to the input + focus.
// ---------------------------------------------------------------------------

/** A folder name matches the query: case-insensitive substring. An empty
 * query matches nothing here (callers short-circuit to the full tree). */
function nameMatches(name: string, queryLower: string): boolean {
  return name.toLowerCase().includes(queryLower);
}

/** Prune a folder tree to the subtrees that contain a match. A node is kept
 * when its OWN name matches OR any descendant matches (so the path down to a
 * deep match stays whole). Returns a NEW tree; the input is never mutated. */
export function filterTree(nodes: FolderNode[], query: string): FolderNode[] {
  const q = query.trim().toLowerCase();
  if (q === "") return nodes; // no filter ⇒ the whole tree
  const out: FolderNode[] = [];
  for (const n of nodes) {
    const keptChildren = filterTree(n.children, query);
    // Keep this node if it matches itself or has a surviving descendant.
    if (nameMatches(n.name, q) || keptChildren.length > 0) {
      out.push({ name: n.name, relPath: n.relPath, children: keptChildren });
    }
  }
  return out;
}

/** The first matching folder's rowKey (depth-first, parents before children),
 * for "jump to folder" — Enter in the filter input focuses it. null when the
 * query matches nothing. A blank query also returns null (nothing to jump to). */
export function firstMatchKey(
  rootId: string,
  nodes: FolderNode[],
  query: string,
): string | null {
  const q = query.trim().toLowerCase();
  if (q === "") return null;
  for (const n of nodes) {
    if (nameMatches(n.name, q)) return rowKey(rootId, n.relPath);
    const deeper = firstMatchKey(rootId, n.children, query);
    if (deeper !== null) return deeper;
  }
  return null;
}

/** ←/→ (or a twist click) collapse/expand a row. Returns BOTH the collapsed
 * set and the explicit-expanded set: opening a deep branch must survive the
 * auto-collapse depth, so a right/expand records the key in `expanded` as
 * well as clearing it from `collapsed`; a left/collapse does the inverse.
 * Both sets are returned fresh; inputs are never mutated. */
export function toggleExpand(
  collapsed: ReadonlySet<string>,
  expanded: ReadonlySet<string>,
  row: SourceRow,
  dir: "left" | "right",
): { collapsed: ReadonlySet<string>; expanded: ReadonlySet<string> } {
  const nextCollapsed = new Set(collapsed);
  const nextExpanded = new Set(expanded);
  if (dir === "left") {
    nextCollapsed.add(row.key);
    nextExpanded.delete(row.key); // a hand-collapsed deep row stays closed
  } else {
    nextCollapsed.delete(row.key);
    nextExpanded.add(row.key); // keep a deep branch open past the depth cap
  }
  return { collapsed: nextCollapsed, expanded: nextExpanded };
}
