/**
 * Source-rail row model (logic/sources.ts — featureset §3): the folders
 * provider (extracted from the old Rail.svelte), flatten, keyboard
 * navigation, and expand/collapse. M3 adds sibling sections; the section
 * shape is the seam.
 */
import { describe, expect, it } from "vitest";
import {
  collectionKey,
  collectionRows,
  filterTree,
  firstMatchKey,
  flatRows,
  folderSection,
  moveFocus,
  rowKey,
  sections,
  toggleExpand,
  type SourcesInput,
} from "../src/lib/logic/sources";
import type { CollectionDto, FolderNode, RootDto } from "../src/lib/types/dto";

const root = (rootId: string, online = true): RootDto => ({
  rootId,
  displayName: rootId,
  relPath: "",
  volumeId: "v1",
  online,
  absPath: online ? `/mnt/${rootId}` : null,
  archived: false,
});

const node = (name: string, relPath: string, children: FolderNode[] = []): FolderNode => ({
  name,
  relPath,
  children,
});

// autoExpandDepth 99 + no explicit-expanded set keeps the legacy
// "everything expanded" behavior for the shared fixture; the depth-cap and
// filter behaviors get their own dedicated inputs below.
const input: SourcesInput = {
  roots: [root("Active"), root("Archive", false)],
  tree: [node("2026", "2026", [node("01-iceland", "2026/01-iceland")]), node("inbox", "inbox")],
  treeRootId: "Active",
  collapsed: new Set(),
  expanded: new Set(),
  autoExpandDepth: 99,
};

describe("folders provider", () => {
  it("flattens roots with the CURRENT root's tree expanded beneath it", () => {
    const rows = folderSection(input).rows;
    expect(rows.map((r) => r.label)).toEqual([
      "Active",
      "2026",
      "01-iceland",
      "inbox",
      "Archive",
    ]);
    expect(rows[1].depth).toBe(1);
    expect(rows[2].depth).toBe(2);
  });

  it("marks offline roots (⏏ badge data)", () => {
    const rows = folderSection(input).rows;
    expect(rows.find((r) => r.label === "Archive")?.offline).toBe(true);
    expect(rows.find((r) => r.label === "Active")?.offline).toBe(false);
  });

  it("collapsed keys prune whole subtrees", () => {
    const collapsed = new Set([rowKey("Active", "2026")]);
    const rows = folderSection({ ...input, collapsed }).rows;
    expect(rows.map((r) => r.label)).toEqual(["Active", "2026", "inbox", "Archive"]);
    expect(rows.find((r) => r.label === "2026")?.expanded).toBe(false);
  });

  it("sections() is the provider aggregation seam (folders only in P4.2)", () => {
    expect(sections(input).map((s) => s.id)).toEqual(["folders"]);
  });
});

describe("collections provider (B71 — the rail's Collections tab)", () => {
  const coll = (id: string, name: string, status: string, memberCount = 0): CollectionDto => ({
    id,
    name,
    description: "",
    status,
    createdTs: "2026-06-01T00:00:00.000Z",
    updatedTs: "2026-06-01T00:00:00.000Z",
    memberCount,
    noteCount: 0,
  });

  it("maps the snapshot in backend order with stable keys and counts", () => {
    const rows = collectionRows([
      coll("01A", "Quiet Hours", "active", 12),
      coll("01B", "Fog Series", "shelved", 3),
    ]);
    expect(rows.map((r) => r.key)).toEqual([collectionKey("01A"), collectionKey("01B")]);
    expect(rows.map((r) => r.label)).toEqual(["Quiet Hours", "Fog Series"]);
    expect(rows[0].memberCount).toBe(12);
  });

  it("shelved/done rows are status-dimmed, never hidden", () => {
    const rows = collectionRows([
      coll("01A", "Quiet Hours", "active"),
      coll("01B", "Fog Series", "shelved"),
      coll("01C", "Iceland", "done"),
    ]);
    expect(rows.map((r) => r.dim)).toEqual([false, true, true]);
    expect(rows.length).toBe(3);
  });

  it("collection rows ride the same moveFocus as folder rows", () => {
    const rows = collectionRows([
      coll("01A", "Quiet Hours", "active"),
      coll("01B", "Fog Series", "shelved"),
    ]);
    expect(moveFocus(rows, null, "down")).toBe(rows[0].key);
    expect(moveFocus(rows, rows[0].key, "down")).toBe(rows[1].key);
    expect(moveFocus(rows, rows[1].key, "down")).toBe(rows[1].key);
  });
});

describe("keyboard navigation", () => {
  const rows = flatRows(sections(input));

  it("↓/↑ walk the flattened rows; null focus starts at the edge", () => {
    expect(moveFocus(rows, null, "down")).toBe(rows[0].key);
    expect(moveFocus(rows, rows[0].key, "down")).toBe(rows[1].key);
    expect(moveFocus(rows, rows[1].key, "up")).toBe(rows[0].key);
    // Clamped at the ends.
    expect(moveFocus(rows, rows[0].key, "up")).toBe(rows[0].key);
    const last = rows[rows.length - 1].key;
    expect(moveFocus(rows, last, "down")).toBe(last);
  });

  it("←/→ collapse/expand the focused row, tracking both sets", () => {
    const row = rows.find((r) => r.label === "2026");
    if (row === undefined) throw new Error("fixture");
    let s = toggleExpand(new Set(), new Set(), row, "left");
    expect(s.collapsed.has(row.key)).toBe(true);
    expect(s.expanded.has(row.key)).toBe(false);
    s = toggleExpand(s.collapsed, s.expanded, row, "right");
    expect(s.collapsed.has(row.key)).toBe(false);
    // Expanding records the key so a deep branch survives the depth cap.
    expect(s.expanded.has(row.key)).toBe(true);
  });
});

describe("lazy deep-tree (auto-collapse past a depth)", () => {
  // A deep root: root → a → b → c. A row at `depth` auto-expands only while
  // depth < autoExpandDepth, so with cap 2 the rows at depths 0 and 1 expand
  // (showing through depth 2), and depth 2 stays collapsed until opened.
  const deep: SourcesInput = {
    roots: [root("Deep")],
    tree: [node("a", "a", [node("b", "a/b", [node("c", "a/b/c")])])],
    treeRootId: "Deep",
    collapsed: new Set(),
    expanded: new Set(),
    autoExpandDepth: 2,
  };

  it("does not render the whole deep tree eagerly", () => {
    const rows = folderSection(deep).rows;
    // Deep(0), a(1), b(2) shown; b is at the cap so its child c is NOT shown.
    expect(rows.map((r) => r.label)).toEqual(["Deep", "a", "b"]);
    expect(rows.find((r) => r.label === "b")?.expanded).toBe(false);
    expect(rows.find((r) => r.label === "a")?.expanded).toBe(true);
  });

  it("an explicitly expanded deep branch shows past the cap", () => {
    const expanded = new Set([rowKey("Deep", "a/b")]);
    const rows = folderSection({ ...deep, expanded }).rows;
    // Opening b (depth 2) reveals c (depth 3).
    expect(rows.map((r) => r.label)).toEqual(["Deep", "a", "b", "c"]);
    expect(rows.find((r) => r.label === "b")?.expanded).toBe(true);
  });
});

describe("folder filter / jump-to-folder", () => {
  const tree: FolderNode[] = [
    node("2026", "2026", [
      node("01-iceland", "2026/01-iceland"),
      node("02-norway", "2026/02-norway"),
    ]),
    node("inbox", "inbox"),
  ];

  it("a blank query returns the tree unchanged", () => {
    expect(filterTree(tree, "")).toBe(tree);
    expect(filterTree(tree, "   ")).toBe(tree);
  });

  it("keeps matches AND their ancestors so the path stays reachable", () => {
    const out = filterTree(tree, "iceland");
    // 2026 kept as the ancestor of the match; norway and inbox pruned.
    expect(out.map((n) => n.name)).toEqual(["2026"]);
    expect(out[0].children.map((n) => n.name)).toEqual(["01-iceland"]);
  });

  it("matching a parent keeps it but prunes non-matching children", () => {
    const out = filterTree(tree, "2026");
    // The parent matches; with no child matching, children are pruned.
    expect(out.map((n) => n.name)).toEqual(["2026"]);
    expect(out[0].children).toEqual([]);
  });

  it("is case-insensitive", () => {
    expect(filterTree(tree, "INBOX").map((n) => n.name)).toEqual(["inbox"]);
  });

  it("firstMatchKey returns the first match depth-first for jump", () => {
    expect(firstMatchKey("R", tree, "norway")).toBe(rowKey("R", "2026/02-norway"));
    // Parent before child: a query hitting the parent jumps to the parent.
    expect(firstMatchKey("R", tree, "2026")).toBe(rowKey("R", "2026"));
    expect(firstMatchKey("R", tree, "nope")).toBeNull();
    expect(firstMatchKey("R", tree, "")).toBeNull();
  });
});
