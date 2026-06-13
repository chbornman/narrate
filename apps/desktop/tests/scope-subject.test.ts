/**
 * Scope-subject voice routing END TO END (DESIGN-VOICE-SUBJECTS.md): the
 * precedence focused-image > collection > topic > neutral, exercised through
 * the composition root's reportScope (NOT the pure scopeSubject helper, which
 * scope.test.ts already covers with hand-built sources). The gap this fills is
 * the WIRING: reportScope derives the collection/topic subject from the DERIVED
 * `collectionId`/`topicDetailId` (which unwrap gridScope), reads activeHash off
 * the live view arm, and ships {targets, subject} to set_scope. We assert the
 * cross-product of (viewMode, collection scope, topic scope, activeHash) the
 * existing suite does not, by reading back the set_scope payload.
 *
 * Mirrors graph-store.test.ts's harness; the set_scope mock echoes the payload
 * so both the targets and the subject can be read off the recorded call.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CollectionDto, GridItem, TopicDto } from "../src/lib/types/dto";

const ipcLog = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    ipcLog.calls.push({ cmd, args });
    switch (cmd) {
      case "set_scope": {
        const targets = (args?.targets ?? []) as string[];
        const kind =
          targets.length === 0 ? "session" : targets.length === 1 ? "single" : "multi";
        return { kind, count: targets.length, previewHashes: targets.slice(0, 8) };
      }
      case "list_collection_members":
        return ["m", "n"].map((h) => ({
          hash: h,
          fileName: `${h}.jpg`,
          relPath: `${h}.jpg`,
          captureTs: null,
          addedTs: "2026-02-01T00:00:00Z",
          hasJournal: false,
          rating: null,
          offline: false,
        }));
      case "list_images":
        return ((args?.hashes ?? []) as string[]).map((h) => ({
          hash: h,
          fileName: `${h}.jpg`,
          relPath: `${h}.jpg`,
          captureTs: null,
          addedTs: "2026-02-01T00:00:00Z",
          hasJournal: false,
          rating: null,
          offline: false,
        }));
      case "collections_for_image":
        return [];
      case "folder_tree":
      case "list_roots":
        return [];
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";
import * as sel from "../src/lib/logic/selection";
import { scopeSubject, subjectScopeText } from "../src/lib/logic/scope";

const item = (hash: string): GridItem => ({
  hash,
  fileName: `${hash}.jpg`,
  relPath: `${hash}.jpg`,
  captureTs: null,
  addedTs: "2026-02-01T00:00:00Z",
  hasJournal: false,
  rating: null,
  offline: false,
});

const lastCall = (cmd: string) =>
  [...ipcLog.calls].reverse().find((c) => c.cmd === cmd);
const lastSubject = () => lastCall("set_scope")?.args?.subject;
const lastTargets = () => lastCall("set_scope")?.args?.targets;

const COLL: CollectionDto = {
  id: "coll01",
  name: "Cover Edit",
  description: "",
  status: "active",
  createdTs: "2026-02-01T00:00:00Z",
  updatedTs: "2026-02-01T00:00:00Z",
  memberCount: 2,
  noteCount: 0,
};

const TOPIC: TopicDto = {
  id: "topic07",
  phrase: "golden hour",
  space: "blend",
  createdTs: "2026-02-01T00:00:00Z",
};

let ui: Ui;
beforeEach(() => {
  ipcLog.calls.length = 0;
  localStorage.clear();
  ui = new Ui();
  ui.grid.rawItems = ["a", "b", "x"].map(item);
  // The store carries the loaded collection + topic so scopeCollection /
  // scopeTopic can resolve the NAME the subject reports.
  ui.collections = [COLL];
  ui.topics = [TOPIC];
});

// (1) Collection open + no image focused -> Collection subject, EMPTY targets.
// scope.test.ts asserts the pure helper; this proves the wiring: the collection
// scope's id is DERIVED (collectionId) and the name resolved from the store.
describe("collection scope, nothing focused -> Collection subject (end to end)", () => {
  it("reportScope ships subject=Collection(id,name) with zero targets", async () => {
    await ui.openCollection("coll01"); // grid in collection scope, no selection
    expect(ui.collectionId).toBe("coll01");
    expect(lastTargets()).toEqual([]);
    expect(lastSubject()).toEqual({ kind: "collection", id: "coll01", name: "Cover Edit" });
  });

  it("a query scope OVER the collection still routes to the Collection subject", async () => {
    // collectionId unwraps a query scope sitting over a collection (the residue
    // still points there), so dictation still names the collection.
    ui.gridScope = {
      kind: "query",
      query: "fog",
      chips: [],
      within: { kind: "collection", id: "coll01" },
    };
    ui.grid.sel = sel.EMPTY;
    await ui.reportScope();
    expect(lastSubject()).toEqual({ kind: "collection", id: "coll01", name: "Cover Edit" });
  });
});

// (2) Topic detail open + no image focused (no collection) -> Topic subject.
describe("topic scope, nothing focused -> Topic subject (end to end)", () => {
  it("a SAVED topic scope (carries topicId) routes to the Topic subject", async () => {
    // openTopic with the saved id surfaces the note log; topicDetailId derives
    // from the scope's topicId.
    await ui.openTopic("golden hour", "topic07");
    expect(ui.topicDetailId).toBe("topic07");
    expect(ui.collectionId).toBe(null);
    expect(lastTargets()).toEqual([]);
    expect(lastSubject()).toEqual({ kind: "topic", id: "topic07", name: "golden hour" });
  });

  it("a PHRASE-only topic scope (no saved id) routes NEUTRAL, no subject", async () => {
    // A graph-lens / live re-rank topic scope carries no topicId, so
    // topicDetailId is null and there is no subject to name (mirrors how a
    // query scope shows no collection notes without a within-collection).
    await ui.openTopic("golden hour"); // no id
    expect(ui.topicDetailId).toBe(null);
    expect(lastTargets()).toEqual([]);
    expect(lastSubject()).toBe(null);
  });
});

// (3) A focused image ALWAYS wins, across the cross-product of views.
describe("a focused image overrides any open subject (every view arm)", () => {
  it("grid selection inside a collection -> image targets, subject None", async () => {
    await ui.openCollection("coll01"); // members m, n loaded
    await ui.applySelection(sel.click(sel.EMPTY, ui.grid.unitHashes, 0)); // focus a member
    expect(lastTargets()).toEqual([ui.grid.unitHashes[0]]);
    expect(lastSubject()).toBe(null);
  });

  it("a Look-viewed image inside a collection -> image targets, subject None", async () => {
    await ui.openCollection("coll01");
    await ui.openLook(ui.grid.unitHashes[0]);
    expect(ui.viewMode).toBe("look");
    expect((lastTargets() as string[]).length).toBeGreaterThan(0);
    expect(lastSubject()).toBe(null);
  });

  it("a SELECTED visualizer node inside a collection -> the node, subject None", async () => {
    await ui.openCollection("coll01");
    await ui.openVisualizer();
    await ui.selectGraphNode("m");
    expect(ui.viewMode).toBe("visualizer");
    expect(lastTargets()).toEqual(["m"]);
    expect(lastSubject()).toBe(null);
  });

  it("a DESELECTED visualizer node inside a collection -> the Collection subject", async () => {
    // The visualizer owns the scope: with nothing selected it is neutral, which
    // is exactly the condition that lets the collection subject ride.
    await ui.openCollection("coll01");
    await ui.openVisualizer();
    await ui.selectGraphNode(null);
    expect(ui.viewSelection).toBe(null);
    expect(lastTargets()).toEqual([]);
    expect(lastSubject()).toEqual({ kind: "collection", id: "coll01", name: "Cover Edit" });
  });
});

// (4) Neutral: no subject open, nothing focused -> session note (subject null).
describe("nothing focused, no subject open -> neutral session (no subject)", () => {
  it("a plain folder grid with no selection ships empty targets and null subject", async () => {
    ui.gridScope = { kind: "folder", rootId: "r1", folder: "trip" };
    ui.grid.sel = sel.EMPTY;
    await ui.reportScope();
    expect(lastTargets()).toEqual([]);
    expect(lastSubject()).toBe(null);
  });
});

// (5) The indicator/echo NAMES the subject the same way scope.ts derives it:
// pair the live store-derived subject with subjectScopeText so the indicator
// copy and the routed subject can never disagree.
describe("the indicator names exactly the routed subject", () => {
  it("a collection subject reads 'noting: <name>'", async () => {
    await ui.openCollection("coll01");
    const subject = lastSubject() as { kind: "collection" | "topic"; name: string };
    expect(subjectScopeText(subject.kind, subject.name)).toBe("noting: Cover Edit");
  });

  it("a topic subject reads 'noting topic: <phrase>', no em-dash", async () => {
    await ui.openTopic("golden hour", "topic07");
    const subject = lastSubject() as { kind: "collection" | "topic"; name: string };
    const text = subjectScopeText(subject.kind, subject.name);
    expect(text).toBe("noting topic: golden hour");
    expect(text).not.toContain("—");
  });
});

// (6) The store-derived ScopeSource the wiring builds matches the pure helper:
// guards against a future reportScope drift away from scope.ts's precedence by
// re-deriving the subject from the SAME inputs the store exposes.
describe("store-derived inputs agree with the pure scopeSubject helper", () => {
  it("collection-open-no-focus derives the same subject both ways", async () => {
    await ui.openCollection("coll01");
    const viaHelper = scopeSubject({
      viewMode: "grid",
      searchOpen: false,
      gridSelection: [],
      searchSelection: [],
      lookTargets: [],
      collection: { id: "coll01", name: "Cover Edit" },
      topic: null,
    });
    expect(lastSubject()).toEqual(viaHelper);
  });
});
