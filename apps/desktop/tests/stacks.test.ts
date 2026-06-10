/**
 * RAW+JPEG stacks (logic/stacks.ts, featureset §5 / D1): display-level
 * auto-pairing by basename + folder, live reversible collapse (global +
 * per-pair XOR), JPEG-on-top with the R flip, and expandTargets — the
 * CAPTURE-conformance seam: a collapsed pair is ONE cell but TWO ordered
 * write-scope targets, display member first (DECISIONS 4), which is why
 * the indicator truthfully reads "● 2" for one selected cell.
 */
import { describe, expect, it } from "vitest";
import * as stacks from "../src/lib/logic/stacks";
import { scopeLabel } from "../src/lib/logic/scope";
import type { GridItem } from "../src/lib/types/dto";

const item = (relPath: string): GridItem => {
  const fileName = relPath.split("/").pop() as string;
  return {
    hash: `h:${relPath}`,
    fileName,
    relPath,
    captureTs: null,
    addedTs: "2026-06-01T00:00:00Z",
    hasJournal: false,
    rating: null,
    offline: false,
  };
};

const collapsed = (items: GridItem[], overrides: string[] = [], flips: string[] = []) =>
  stacks.buildUnits(items, {
    globalCollapsed: true,
    overrides: new Set(overrides),
    flips: new Set(flips),
  });

const expanded = (items: GridItem[], overrides: string[] = []) =>
  stacks.buildUnits(items, {
    globalCollapsed: false,
    overrides: new Set(overrides),
    flips: new Set(),
  });

describe("auto-pairing by basename + folder (D1)", () => {
  it("pairs a JPEG and a RAW sharing basename and folder into ONE cell", () => {
    const m = collapsed([item("a/IMG_1.cr2"), item("a/IMG_1.jpg"), item("a/IMG_2.jpg")]);
    expect(m.units.length).toBe(2);
    // JPEG on top (collapsed preview = JPEG), RAW hidden as alt.
    expect(m.units[0].primary.hash).toBe("h:a/IMG_1.jpg");
    expect(m.units[0].alt?.hash).toBe("h:a/IMG_1.cr2");
    expect(m.units[1].alt).toBeNull();
    expect(m.pairs).toEqual(["a/img_1", null]);
  });

  it("the collapsed cell sits at the first-occurring member's position", () => {
    const m = collapsed([item("a/IMG_1.cr2"), item("a/IMG_0.jpg"), item("a/IMG_1.jpg")]);
    // RAW came first in grid order, so the pair occupies slot 0.
    expect(m.units.map((u) => u.primary.hash)).toEqual(["h:a/IMG_1.jpg", "h:a/IMG_0.jpg"]);
  });

  it("same basename in DIFFERENT folders never pairs", () => {
    const m = collapsed([item("a/IMG_1.jpg"), item("b/IMG_1.cr2")]);
    expect(m.units.length).toBe(2);
    expect(m.pairs).toEqual([null, null]);
  });

  it("extensions and basenames match case-insensitively", () => {
    const m = collapsed([item("a/img_1.JPG"), item("a/IMG_1.CR2")]);
    expect(m.units.length).toBe(1);
    expect(m.units[0].primary.fileName).toBe("img_1.JPG");
  });

  it("only the RAW+JPEG classes pair (PNG siblings stay solo)", () => {
    const m = collapsed([item("a/IMG_1.png"), item("a/IMG_1.jpg")]);
    expect(m.units.length).toBe(2);
  });

  it("a duplicate-basename extra stays a solo cell (first of each class pairs)", () => {
    const m = collapsed([item("a/IMG_1.jpg"), item("a/IMG_1.jpeg"), item("a/IMG_1.nef")]);
    expect(m.units.length).toBe(2);
    expect(m.units[0].alt?.hash).toBe("h:a/IMG_1.nef");
    expect(m.units[1].primary.hash).toBe("h:a/IMG_1.jpeg");
    expect(m.pairs[1]).toBeNull();
  });
});

describe("live, reversible collapse (global + per-pair overrides)", () => {
  const items = [item("a/IMG_1.jpg"), item("a/IMG_1.cr2"), item("a/IMG_2.jpg")];

  it("expanded pairs render both members, still marked as pair members", () => {
    const m = expanded(items);
    expect(m.units.length).toBe(3);
    expect(m.units.every((u) => u.alt === null)).toBe(true);
    // Both members keep the pair key — the collapse control stays live.
    expect(m.pairs).toEqual(["a/img_1", "a/img_1", null]);
  });

  it("a per-pair override XORs the global state, both directions", () => {
    expect(collapsed(items, ["a/img_1"]).units.length).toBe(3); // global collapsed, pair expanded
    expect(expanded(items, ["a/img_1"]).units.length).toBe(2); // global expanded, pair collapsed
  });

  it("isCollapsed is the XOR table", () => {
    const none: ReadonlySet<string> = new Set();
    const some: ReadonlySet<string> = new Set(["k"]);
    expect(stacks.isCollapsed("k", true, none)).toBe(true);
    expect(stacks.isCollapsed("k", true, some)).toBe(false);
    expect(stacks.isCollapsed("k", false, none)).toBe(false);
    expect(stacks.isCollapsed("k", false, some)).toBe(true);
  });
});

describe("R flip (featureset §5: the displayed member swaps)", () => {
  it("a flipped collapsed pair shows the RAW on top", () => {
    const m = collapsed([item("a/IMG_1.jpg"), item("a/IMG_1.cr2")], [], ["a/img_1"]);
    expect(m.units[0].primary.hash).toBe("h:a/IMG_1.cr2");
    expect(m.units[0].alt?.hash).toBe("h:a/IMG_1.jpg");
  });
});

describe("expandTargets — the ● 2 truth (CAPTURE conformance, DECISIONS 4)", () => {
  const items = [item("a/IMG_1.jpg"), item("a/IMG_1.cr2"), item("a/IMG_2.jpg")];

  it("ONE selected collapsed cell yields TWO ordered targets, JPEG then RAW", () => {
    const m = collapsed(items);
    const targets = stacks.expandTargets(["h:a/IMG_1.jpg"], m.units);
    expect(targets).toEqual(["h:a/IMG_1.jpg", "h:a/IMG_1.cr2"]);
    // …and the indicator renders the TARGET count, not the cell count.
    expect(scopeLabel("multi", targets.length)).toBe("2");
  });

  it("a flipped pair reports the displayed member (RAW) first", () => {
    const m = collapsed(items, [], ["a/img_1"]);
    expect(stacks.expandTargets(["h:a/IMG_1.cr2"], m.units)).toEqual([
      "h:a/IMG_1.cr2",
      "h:a/IMG_1.jpg",
    ]);
  });

  it("a hash naming the HIDDEN member still expands to the full pair", () => {
    const m = collapsed(items);
    expect(stacks.expandTargets(["h:a/IMG_1.cr2"], m.units)).toEqual([
      "h:a/IMG_1.jpg",
      "h:a/IMG_1.cr2",
    ]);
  });

  it("expanded members annotate INDIVIDUALLY, in selection order", () => {
    const m = expanded(items);
    expect(
      stacks.expandTargets(["h:a/IMG_1.cr2", "h:a/IMG_2.jpg"], m.units),
    ).toEqual(["h:a/IMG_1.cr2", "h:a/IMG_2.jpg"]);
  });

  it("selection order is preserved; duplicates collapse to first occurrence", () => {
    const m = collapsed(items);
    expect(
      stacks.expandTargets(["h:a/IMG_2.jpg", "h:a/IMG_1.jpg", "h:a/IMG_1.cr2"], m.units),
    ).toEqual(["h:a/IMG_2.jpg", "h:a/IMG_1.jpg", "h:a/IMG_1.cr2"]);
  });

  it("a hash not on the surface passes through untouched (stale selection)", () => {
    const m = collapsed(items);
    expect(stacks.expandTargets(["gone"], m.units)).toEqual(["gone"]);
  });
});

describe("display-member preference (Settings → 'Stacked pairs show', dogfood round 1)", () => {
  const items = [item("a/IMG_1.jpg"), item("a/IMG_1.cr2"), item("a/IMG_2.jpg")];
  const build = (
    display: stacks.StackDisplayMember | undefined,
    flips: string[] = [],
  ) =>
    stacks.buildUnits(items, {
      globalCollapsed: true,
      overrides: new Set(),
      flips: new Set(flips),
      display,
    });

  it("defaults to JPEG on top (option absent — existing callers unchanged)", () => {
    const m = build(undefined);
    expect(m.units[0].primary.hash).toBe("h:a/IMG_1.jpg");
    expect(m.units[0].alt?.hash).toBe("h:a/IMG_1.cr2");
  });

  it("'raw' shows the RAW member on top, JPEG hidden as alt", () => {
    const m = build("raw");
    expect(m.units[0].primary.hash).toBe("h:a/IMG_1.cr2");
    expect(m.units[0].alt?.hash).toBe("h:a/IMG_1.jpg");
    // Solo JPEGs are untouched by the preference.
    expect(m.units[1].primary.hash).toBe("h:a/IMG_2.jpg");
    expect(m.units[1].alt).toBeNull();
  });

  it("R flips XOR the preference (flip under 'raw' shows the JPEG)", () => {
    const m = build("raw", ["a/img_1"]);
    expect(m.units[0].primary.hash).toBe("h:a/IMG_1.jpg");
    expect(m.units[0].alt?.hash).toBe("h:a/IMG_1.cr2");
  });

  it("write targets follow the preference: display member first (U4 order rule)", () => {
    const m = build("raw");
    expect(stacks.expandTargets(["h:a/IMG_1.cr2"], m.units)).toEqual([
      "h:a/IMG_1.cr2",
      "h:a/IMG_1.jpg",
    ]);
    // …and naming the hidden JPEG still expands to the full pair, RAW first.
    expect(stacks.expandTargets(["h:a/IMG_1.jpg"], m.units)).toEqual([
      "h:a/IMG_1.cr2",
      "h:a/IMG_1.jpg",
    ]);
  });

  it("the preference does not affect pairing or expanded members", () => {
    const m = stacks.buildUnits(items, {
      globalCollapsed: false,
      overrides: new Set(),
      flips: new Set(),
      display: "raw",
    });
    expect(m.units.length).toBe(3);
    expect(m.pairs).toEqual(["a/img_1", "a/img_1", null]);
  });
});

describe("expandedLinks — expanded members read as linked (dogfood round 1)", () => {
  const items = [item("a/IMG_1.jpg"), item("a/IMG_1.cr2"), item("a/IMG_2.jpg")];

  it("links the two expanded members when adjacent on one row", () => {
    const m = expanded(items);
    expect(stacks.expandedLinks(m, 4)).toEqual([
      { left: false, right: true },
      { left: true, right: false },
      { left: false, right: false },
    ]);
  });

  it("a row wrap between members breaks the link (chevrons still mark membership)", () => {
    const m = expanded(items);
    // cols = 1: every unit is its own row.
    expect(stacks.expandedLinks(m, 1)).toEqual([
      { left: false, right: false },
      { left: false, right: false },
      { left: false, right: false },
    ]);
  });

  it("a foreign cell between members breaks the link (capture-time sort can split a pair)", () => {
    const split = [item("a/IMG_1.jpg"), item("a/IMG_0.jpg"), item("a/IMG_1.cr2")];
    const m = stacks.buildUnits(split, {
      globalCollapsed: false,
      overrides: new Set(),
      flips: new Set(),
    });
    expect(stacks.expandedLinks(m, 10).every((l) => !l.left && !l.right)).toBe(true);
  });

  it("collapsed pairs and solos never link", () => {
    const m = collapsed(items);
    expect(stacks.expandedLinks(m, 4).every((l) => !l.left && !l.right)).toBe(true);
  });
});
