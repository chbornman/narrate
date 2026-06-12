/**
 * Look navigation (logic/looknav.ts): navigation set = entry selection
 * (featureset §2) and R member flips over entries (featureset §5). The
 * exhaustive Space tap-vs-hold table that used to live here died with the
 * machine itself (June 12 2026: Space is 100% the microphone key —
 * michold.test.ts owns the surviving tap-vs-hold semantics, on Space).
 */
import { describe, expect, it } from "vitest";
import {
  displayedHash,
  navigationSet,
  toEntry,
  toggleFlip,
} from "../src/lib/logic/looknav";
import type { DisplayUnit } from "../src/lib/types/display";
import type { GridItem } from "../src/lib/types/dto";

const item = (hash: string): GridItem => ({
  hash,
  fileName: `${hash}.jpg`,
  relPath: `${hash}.jpg`,
  captureTs: null,
  addedTs: "2026-02-01T00:00:00Z",
  hasJournal: false,
  offline: false,
  rating: null,
});

const unit = (primary: string, alt: string | null = null): DisplayUnit => ({
  primary: item(primary),
  alt: alt === null ? null : item(alt),
});

// Five grid cells; "c" is a collapsed pair (JPEG displayed, RAW hidden).
const UNITS = [unit("a"), unit("b"), unit("c", "c-raw"), unit("d"), unit("e")];

describe("toEntry (the frozen DisplayUnit → LookEntry seam)", () => {
  it("maps a lone unit and a pair", () => {
    expect(toEntry(unit("a"))).toEqual({ display: "a", alt: null });
    expect(toEntry(unit("c", "c-raw"))).toEqual({ display: "c", alt: "c-raw" });
  });
});

describe("navigationSet — the set is the entry selection (featureset §2)", () => {
  it("multi-selection: cycles within it, in GRID order regardless of click order", () => {
    const nav = navigationSet(UNITS, ["d", "b"], "b"); // clicked d first
    expect(nav?.order.map((e) => e.display)).toEqual(["b", "d"]);
    expect(nav?.index).toBe(0);
    expect(navigationSet(UNITS, ["d", "b"], "d")?.index).toBe(1);
  });

  it("single-image entry cycles the whole folder", () => {
    const nav = navigationSet(UNITS, ["b"], "b");
    expect(nav?.order.map((e) => e.display)).toEqual(["a", "b", "c", "d", "e"]);
    expect(nav?.index).toBe(1);
  });

  it("no selection cycles the folder", () => {
    expect(navigationSet(UNITS, [], "c")?.index).toBe(2);
    expect(navigationSet(UNITS, [], "c")?.order).toHaveLength(5);
  });

  it("entering OUTSIDE the selection cycles the folder (scope narrowed to the viewed image anyway — CAPTURE §3)", () => {
    const nav = navigationSet(UNITS, ["b", "d"], "e");
    expect(nav?.order).toHaveLength(5);
    expect(nav?.index).toBe(4);
  });

  it("pair units carry their alt into the entry (R flips it later)", () => {
    const nav = navigationSet(UNITS, ["b", "c"], "c");
    expect(nav?.order).toEqual([
      { display: "b", alt: null },
      { display: "c", alt: "c-raw" },
    ]);
  });

  it("stale selection hashes are ignored; an unknown entry is null", () => {
    const nav = navigationSet(UNITS, ["b", "gone", "d"], "d");
    expect(nav?.order.map((e) => e.display)).toEqual(["b", "d"]);
    expect(navigationSet(UNITS, [], "gone")).toBeNull();
  });
});

describe("member flips (R)", () => {
  const pair = { display: "c", alt: "c-raw" };
  const lone = { display: "a", alt: null };

  it("flip shows the alt; flip again restores; the input set is untouched", () => {
    const empty: ReadonlySet<string> = new Set();
    const flipped = toggleFlip(empty, pair);
    expect(displayedHash(pair, flipped)).toBe("c-raw");
    expect(displayedHash(pair, toggleFlip(flipped, pair))).toBe("c");
    expect(empty.size).toBe(0); // immutable in, new set out
  });

  it("lone images no-op quietly", () => {
    const flips: ReadonlySet<string> = new Set();
    expect(toggleFlip(flips, lone)).toBe(flips);
    expect(displayedHash(lone, new Set(["a"]))).toBe("a"); // alt-less: never flips
  });
});
