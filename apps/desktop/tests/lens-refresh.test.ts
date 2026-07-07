/**
 * The scope-following LENSES (Diversify filter, Duplicates lens, heat tint)
 * against the composition root with mocked IPC (the app-flows.test.ts
 * pattern) — the AUDIT-2026-07-07 U-series seams:
 *
 *   U1  a mid-ingest re-list (refreshItems, which deliberately skips
 *       reportScope) must re-run an active Diversify pass, and hashes the
 *       last pass never saw must render (never silently hide new photos);
 *   U2  selectRedundantDuplicates focuses the UNIT of the first gathered
 *       hash (not index 0 of its own hash list) and reports scope;
 *   U4  a lens toggle's direct fetch records the scope signature, so the
 *       next reportScope at an unchanged scope does NOT re-run the pass;
 *   U6  a diversify pass in flight shows through diversifyPending;
 *   U7  the shared kind-prefixed signature: same-size, same-endpoint scopes
 *       of DIFFERENT kinds re-run every lens pass.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const fixture = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  /** Hashes `list_folder` returns (shaped into GridItems by the mock). */
  items: [] as string[],
  /** The next diversify_scope report's shown/hidden split. */
  diversify: { shown: [] as string[], hidden: [] as string[] },
  /** The next find_near_duplicates result. */
  groups: [] as { imageHashes: string[]; count: number }[],
  /** When armed, diversify_scope parks its response until `release()` — the
   * deterministic way to observe the U1 interim (pass in flight) state. */
  gateDiversify: false,
  gated: [] as (() => void)[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    fixture.calls.push({ cmd, args });
    switch (cmd) {
      case "set_scope": {
        const targets = (args?.targets ?? []) as string[];
        const kind =
          targets.length === 0 ? "session" : targets.length === 1 ? "single" : "multi";
        return { kind, count: targets.length, previewHashes: targets.slice(0, 8) };
      }
      case "list_folder":
        return fixture.items.map((h) => ({
          hash: h,
          fileName: `${h}.jpg`,
          relPath: `${h}.jpg`,
          captureTs: null,
          addedTs: "2026-02-01T00:00:00Z",
          hasJournal: false,
          rating: null,
          offline: false,
        }));
      case "folder_tree":
      case "list_roots":
      case "list_archived_roots":
        return [];
      case "diversify_scope": {
        const report = () => ({
          shown: [...fixture.diversify.shown],
          hidden: [...fixture.diversify.hidden],
          cutoff: 0.5,
          degraded: false,
        });
        if (fixture.gateDiversify)
          return new Promise((resolve) => fixture.gated.push(() => resolve(report())));
        return report();
      }
      case "find_near_duplicates":
        return fixture.groups.map((g) => ({ ...g }));
      case "image_intensity":
        return [];
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";

const count = (cmd: string) => fixture.calls.filter((c) => c.cmd === cmd).length;
const lastCall = (cmd: string) =>
  [...fixture.calls].reverse().find((c) => c.cmd === cmd);
/** Drain pending microtasks + zero-timers so a `void`-launched pass settles. */
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const releaseGate = () => {
  for (const release of fixture.gated.splice(0)) release();
};

let ui: Ui;
beforeEach(async () => {
  fixture.calls.length = 0;
  fixture.items = [];
  fixture.diversify = { shown: [], hidden: [] };
  fixture.groups = [];
  fixture.gateDiversify = false;
  fixture.gated.length = 0;
  localStorage.clear();
  ui = new Ui();
});

describe("U1: mid-ingest re-lists keep an active Diversify pass honest", () => {
  it("a re-list with new hashes re-runs the pass and never hides the new items", async () => {
    // Open a folder of a,b,c; the pass folds c (a near-dup of b).
    fixture.items = ["a", "b", "c"];
    fixture.diversify = { shown: ["a", "b"], hidden: ["c"] };
    await ui.openFolder("R1", "");
    ui.toggleDiversify();
    await flush();
    expect(ui.grid.shownItems.map((i) => i.hash)).toEqual(["a", "b"]);
    expect(ui.diversifyHidden).toBe(1);
    const passesBefore = count("diversify_scope");

    // Mid-ingest, two NEW photos (d, e) land and the grid re-lists WITHOUT a
    // reportScope. Park the re-run's response so the interim state is
    // observable; its eventual result also folds e (a near-dup of d).
    fixture.items = ["a", "b", "c", "d", "e"];
    fixture.diversify = { shown: ["a", "b", "d"], hidden: ["c", "e"] };
    fixture.gateDiversify = true;
    await ui.refreshItems();

    // The re-run trigger (the U1 fix): the changed item-set invalidated the
    // scope signature and a fresh pass is in flight.
    expect(count("diversify_scope")).toBe(passesBefore + 1);
    expect(ui.diversifyPending).toBe(true); // U6: the chrome can say so
    // Interim honesty: d and e were never seen by the LAST pass, so they
    // render instead of vanishing; only the explicitly folded c stays hidden.
    expect(ui.grid.shownItems.map((i) => i.hash)).toEqual(["a", "b", "d", "e"]);
    expect(ui.diversifyHidden).toBe(1);

    // The pass lands: the settled fold (c and e) applies, pending clears.
    releaseGate();
    await flush();
    expect(ui.diversifyPending).toBe(false);
    expect(ui.grid.shownItems.map((i) => i.hash)).toEqual(["a", "b", "d"]);
    expect(ui.diversifyHidden).toBe(2);
  });
});

describe("U2: selectRedundantDuplicates focus + scope report", () => {
  it("focuses the UNIT of the first gathered hash and reports the scope", async () => {
    fixture.items = ["a", "b", "c", "d"];
    // Two pairs; no ratings, so each group's first member is the keeper —
    // the redundant gather is [b, d].
    fixture.groups = [
      { imageHashes: ["a", "b"], count: 2 },
      { imageHashes: ["c", "d"], count: 2 },
    ];
    await ui.openFolder("R1", "");
    ui.toggleDuplicates();
    await flush();
    await ui.selectRedundantDuplicates();
    expect(ui.grid.sel.order).toEqual(["b", "d"]);
    // The old `focus: 0` indexed the gathered-hash list and landed on unit
    // "a" — an unselected keep-worthy image. The focus must be b's UNIT index.
    const unitIdx = ui.grid.unitHashes.indexOf("b");
    expect(unitIdx).toBeGreaterThan(0); // the regression is only visible off 0
    expect(ui.grid.sel.focus).toBe(unitIdx);
    expect(ui.grid.sel.anchor).toBe(unitIdx);
    expect(ui.grid.activeHash).toBe("b");
    // reportScope ran (the applySelection twin): the backend write scope
    // carries the gathered selection immediately, not on the next focus move.
    expect(lastCall("set_scope")?.args?.targets).toEqual(["b", "d"]);
  });

  it("falls back to focus -1 when the first gathered hash is not on the surface", async () => {
    fixture.items = ["a", "b"];
    // A stale scan can carry hashes that already left the grid: no unit hosts
    // "y", so there is honestly no active cell.
    fixture.groups = [{ imageHashes: ["x", "y"], count: 2 }];
    await ui.openFolder("R1", "");
    ui.toggleDuplicates();
    await flush();
    await ui.selectRedundantDuplicates();
    expect(ui.grid.sel.order).toEqual(["y"]);
    expect(ui.grid.sel.focus).toBe(-1);
    expect(ui.grid.activeHash).toBeNull();
  });
});

describe("U4: a lens toggle's fetch records the scope key (no redundant pass)", () => {
  it("toggling each lens on runs ONE pass; an unchanged-scope reportScope re-runs none", async () => {
    fixture.items = ["a", "b", "c"];
    fixture.diversify = { shown: ["a", "b", "c"], hidden: [] };
    await ui.openFolder("R1", "");

    ui.toggleDuplicates();
    ui.toggleDiversify();
    ui.toggleHeat();
    await flush();
    expect(count("find_near_duplicates")).toBe(1);
    expect(count("diversify_scope")).toBe(1);
    expect(count("image_intensity")).toBe(1);

    // The very next reportScope (any focus move funnels here) must not pay a
    // second O(n^2) scan / diversify pass / intensity fetch for the SAME scope.
    await ui.reportScope();
    expect(count("find_near_duplicates")).toBe(1);
    expect(count("diversify_scope")).toBe(1);
    expect(count("image_intensity")).toBe(1);
  });
});

describe("U7: scope signatures are kind-prefixed for every lens", () => {
  it("same-size same-endpoint scopes of DIFFERENT kinds re-run the passes", async () => {
    fixture.items = ["a", "b", "c"];
    fixture.diversify = { shown: ["a", "b", "c"], hidden: [] };
    await ui.openFolder("R1", "");
    ui.toggleDuplicates();
    ui.toggleDiversify();
    ui.toggleHeat();
    await flush();
    expect(count("find_near_duplicates")).toBe(1);
    expect(count("diversify_scope")).toBe(1);
    expect(count("image_intensity")).toBe(1);

    // A collection holding the SAME items: identical length and endpoint
    // hashes, so a kind-less signature reads "unchanged" — only the kind
    // prefix makes this edge re-run (the dedup key always had it; diversify
    // and heat omitted it before U7).
    ui.gridScope = { kind: "collection", id: "C1" };
    await ui.reportScope();
    expect(count("find_near_duplicates")).toBe(2);
    expect(count("diversify_scope")).toBe(2);
    expect(count("image_intensity")).toBe(2);
  });
});
