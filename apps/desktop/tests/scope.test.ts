/**
 * Write-scope derivation and the note-input snapshot machine (CAPTURE §3–4,
 * UI §6/§7.2). Pure logic — the UI reports targets, renders echoes. P4.2:
 * scope.ts consumes PRE-EXPANDED target lists (stack expansion happens
 * upstream in the grid/look slices — D1), so the collapsed-pair CAPTURE
 * conformance (one multi-target event, display member first: JPEG then
 * RAW) is asserted here over the expanded lists.
 */
import { describe, expect, it } from "vitest";
import { notePlaceholder, scopeLabel, scopeTargets } from "../src/lib/logic/scope";
import * as note from "../src/lib/logic/note";
import type { ScopeView } from "../src/lib/types/dto";

const base = {
  viewMode: "grid" as const,
  searchOpen: false,
  gridSelection: [] as string[],
  searchSelection: [] as string[],
  lookTargets: [] as string[],
};

describe("scope targets (CAPTURE §3 table)", () => {
  it("grid, no selection → session (zero targets)", () => {
    expect(scopeTargets(base)).toEqual([]);
  });

  it("grid, selection of 1 → that image", () => {
    expect(scopeTargets({ ...base, gridSelection: ["a"] })).toEqual(["a"]);
  });

  it("grid, selection of N ≥ 2 → selected images IN SELECTION ORDER", () => {
    expect(scopeTargets({ ...base, gridSelection: ["e", "b", "d"] })).toEqual([
      "e",
      "b",
      "d",
    ]);
  });

  it("single-image view → the viewed image, narrowing a multi-selection", () => {
    expect(
      scopeTargets({
        ...base,
        viewMode: "look",
        gridSelection: ["a", "b", "c"],
        lookTargets: ["b"],
      }),
    ).toEqual(["b"]);
  });

  it("search results → same rules over the result selection", () => {
    expect(
      scopeTargets({
        ...base,
        searchOpen: true,
        gridSelection: ["a"],
        searchSelection: ["x", "y"],
      }),
    ).toEqual(["x", "y"]);
  });
});

describe("Visualizer scope precedence (DESIGN-VIEW-MODES.md)", () => {
  // The visualizer, when the active view, OWNS the write scope: dictation/rating
  // must target the SELECTED node (viewSelection), never the stale grid/Look
  // selection underneath. viewSelection is the R6 seed-from-active photo.
  it("visualizer + a node selected → that node, overriding grid/Look", () => {
    expect(
      scopeTargets({
        ...base,
        viewMode: "visualizer",
        gridSelection: ["a", "b"],
        lookTargets: ["c"],
        viewSelection: "sel",
      }),
    ).toEqual(["sel"]);
  });

  it("visualizer + nothing selected → NEUTRAL session scope (empty), not stale", () => {
    expect(
      scopeTargets({
        ...base,
        viewMode: "visualizer",
        gridSelection: ["a", "b"],
        viewSelection: null,
      }),
    ).toEqual([]);
  });

  it("NOT in the visualizer → viewSelection is ignored, normal grid/Look rules apply", () => {
    expect(
      scopeTargets({
        ...base,
        viewMode: "grid",
        gridSelection: ["a"],
        viewSelection: "sel",
      }),
    ).toEqual(["a"]);
  });
});

describe("collapsed RAW+JPEG stacks (D1 / CAPTURE conformance)", () => {
  // The slices pre-expand: one selected collapsed pair arrives as TWO
  // ordered hashes — display member (JPEG) FIRST, then RAW. That order is
  // what event_targets.position records (DECISIONS entry 4).
  it("a collapsed pair in the grid selection passes through as one ordered multi-target list", () => {
    expect(scopeTargets({ ...base, gridSelection: ["jpegHash", "rawHash"] })).toEqual([
      "jpegHash",
      "rawHash",
    ]);
  });

  it("a viewed collapsed pair in Look targets both members, display first", () => {
    expect(
      scopeTargets({ ...base, viewMode: "look", lookTargets: ["jpegHash", "rawHash"] }),
    ).toEqual(["jpegHash", "rawHash"]);
  });

  it("the indicator reads the TARGET count truthfully: 1 cell, ● 2", () => {
    // One collapsed cell = two targets; the echoed scope kind is multi,
    // count 2 — the indicator must NOT paper over it with the cell count.
    expect(scopeLabel("multi", 2)).toBe("2");
  });
});

describe("indicator scope text (UI §7.2)", () => {
  it("renders ● N for selections and ● session for none — ● 0 never renders", () => {
    expect(scopeLabel("single", 1)).toBe("1");
    expect(scopeLabel("multi", 3)).toBe("3");
    expect(scopeLabel("session", 0)).toBe("session");
  });
});

describe("note input scope snapshot (UI §6 acceptance)", () => {
  const scope = (kind: ScopeView["kind"], count: number, hashes: string[] = []): ScopeView => ({
    kind,
    count,
    previewHashes: hashes,
  });

  it("summon snapshots the scope; placeholder echoes it", () => {
    const s = note.summon(scope("multi", 3));
    expect(s.open).toBe(true);
    expect(s.snapshot?.count).toBe(3);
    expect(notePlaceholder("multi", 3)).toBe("note on 3 images");
    expect(notePlaceholder("single", 1)).toBe("note on 1 image");
    expect(notePlaceholder("session", 0)).toBe("note on this session");
  });

  it("a scope change while open CANCELS the input (summon-time binding holds)", () => {
    const s = note.summon(scope("multi", 3, ["a", "b", "c"]));
    const after = note.onScopeChanged(s, scope("single", 1, ["a"]));
    expect(after.open).toBe(false);
  });

  it("an identical scope echo does not cancel", () => {
    const s = note.summon(scope("multi", 2, ["a", "b"]));
    expect(note.onScopeChanged(s, scope("multi", 2, ["a", "b"])).open).toBe(true);
  });

  it("submit closes immediately and yields the summon-time scope", () => {
    const s = note.summon(scope("single", 1, ["a"]));
    const { state, scope: bound } = note.submit(s);
    expect(state.open).toBe(false);
    expect(bound?.previewHashes).toEqual(["a"]);
  });

  it("cancel discards without residue", () => {
    expect(note.cancel(note.summon(scope("session", 0))).open).toBe(false);
  });
});
