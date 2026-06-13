/** Grid sort (UI §3.6): the complete v1 set, exact semantics. */
import { describe, expect, it } from "vitest";
import { DEFAULT_SORT, sortItems } from "../src/lib/logic/sort";
import type { GridItem } from "../src/lib/types/dto";

const item = (name: string, capture: string | null, added: string): GridItem => ({
  hash: name,
  fileName: name,
  relPath: name,
  captureTs: capture,
  addedTs: added,
  hasJournal: false,
  rating: null,
  offline: false,
});

const items = [
  item("c.jpg", "2026-01-01T00:00:00Z", "2026-03-01T00:00:00Z"),
  item("a.jpg", "2026-02-01T00:00:00Z", "2026-03-03T00:00:00Z"),
  item("b.jpg", null, "2026-03-02T00:00:00Z"),
];

describe("sort modes", () => {
  it("default is capture date, newest first; undated images sort last", () => {
    expect(DEFAULT_SORT).toBe("capture-desc");
    expect(sortItems(items, "capture-desc").map((i) => i.fileName)).toEqual([
      "a.jpg",
      "c.jpg",
      "b.jpg",
    ]);
  });

  it("capture oldest-first keeps undated last", () => {
    expect(sortItems(items, "capture-asc").map((i) => i.fileName)).toEqual([
      "c.jpg",
      "a.jpg",
      "b.jpg",
    ]);
  });

  it("filename A–Z", () => {
    expect(sortItems(items, "filename").map((i) => i.fileName)).toEqual([
      "a.jpg",
      "b.jpg",
      "c.jpg",
    ]);
  });

  it("date added = ingest time, newest first", () => {
    expect(sortItems(items, "added").map((i) => i.fileName)).toEqual([
      "a.jpg",
      "b.jpg",
      "c.jpg",
    ]);
  });

  it("does not mutate its input", () => {
    const before = items.map((i) => i.fileName);
    sortItems(items, "filename");
    expect(items.map((i) => i.fileName)).toEqual(before);
  });

  it("attention: hottest first by the intensity map; missing scores are cold", () => {
    // a=0.2, c=0.9, b has no score (treated as 0 / cold). Hottest leads;
    // unscored cells fall back to filename order.
    const intensity = new Map([
      ["a.jpg", 0.2],
      ["c.jpg", 0.9],
    ]);
    expect(
      sortItems(items, "attention", intensity).map((i) => i.fileName),
    ).toEqual(["c.jpg", "a.jpg", "b.jpg"]);
  });

  it("attention: with no intensity map every cell is cold (filename order)", () => {
    expect(sortItems(items, "attention").map((i) => i.fileName)).toEqual([
      "a.jpg",
      "b.jpg",
      "c.jpg",
    ]);
  });
});
