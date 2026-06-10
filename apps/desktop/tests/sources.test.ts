/**
 * Source-rail row model (logic/sources.ts — featureset §3): the folders
 * provider (extracted from the old Rail.svelte), flatten, keyboard
 * navigation, and expand/collapse. M3 adds sibling sections; the section
 * shape is the seam.
 */
import { describe, expect, it } from "vitest";
import {
  flatRows,
  folderSection,
  moveFocus,
  rowKey,
  sections,
  toggleExpand,
  type SourcesInput,
} from "../src/lib/logic/sources";
import type { FolderNode, RootDto } from "../src/lib/types/dto";

const root = (rootId: string, online = true): RootDto => ({
  rootId,
  displayName: rootId,
  relPath: "",
  volumeId: "v1",
  online,
  absPath: online ? `/mnt/${rootId}` : null,
});

const node = (name: string, relPath: string, children: FolderNode[] = []): FolderNode => ({
  name,
  relPath,
  children,
});

const input: SourcesInput = {
  roots: [root("Active"), root("Archive", false)],
  tree: [node("2026", "2026", [node("01-iceland", "2026/01-iceland")]), node("inbox", "inbox")],
  treeRootId: "Active",
  collapsed: new Set(),
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

  it("←/→ collapse/expand the focused row", () => {
    const row = rows.find((r) => r.label === "2026");
    if (row === undefined) throw new Error("fixture");
    let collapsed = toggleExpand(new Set(), row, "left");
    expect(collapsed.has(row.key)).toBe(true);
    collapsed = toggleExpand(collapsed, row, "right");
    expect(collapsed.has(row.key)).toBe(false);
  });
});
