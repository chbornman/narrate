/**
 * Graph node selection -> write scope, the gaps graph-store.test.ts leaves.
 * That suite proves selectGraphNode reports the hash and openFromGraph opens
 * Look. Here we pin the SCOPING consequences the task calls out:
 *   - a selected node yields a SINGLE-target scope echo (kind "single", count 1)
 *     so dictation/rating actually target it (a session scope no-ops a rate);
 *   - deselect returns to a SESSION echo (kind "session", count 0) = neutral;
 *   - re-selecting a DIFFERENT node moves the single target (no stale union);
 *   - both gestures that open a node (Enter and the double-click, which share
 *     openFromGraph) land in Look on the node and CLEAR viewSelection so the
 *     departed visualizer is clean for its next open.
 *
 * Same mocked-IPC composition-root harness as graph-store.test.ts, but the
 * set_scope echo reports kind+count off the targets so the scope CONSEQUENCE
 * (not just the recorded targets arg) can be asserted.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GridItem } from "../src/lib/types/dto";

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

let ui: Ui;
beforeEach(() => {
  ipcLog.calls.length = 0;
  localStorage.clear();
  ui = new Ui();
  ui.grid.rawItems = ["a", "b", "x"].map(item);
});

// The shell holds the echoed ScopeView (onScopeEcho). The set_scope mock echoes
// kind+count off the targets, so reading ui.shell.scope tells us the SCOPE the
// indicator/rating gate actually sees, not merely the targets we sent.
describe("selecting a graph node scopes dictation/rating to it", () => {
  it("a selected node yields a single-target scope (rating can land)", async () => {
    await ui.openVisualizer();
    await ui.selectGraphNode("b");
    expect(ui.viewSelection).toBe("b");
    // The reported targets and the echoed scope both say "one image".
    expect(lastCall("set_scope")?.args?.targets).toEqual(["b"]);
    expect(ui.shell.scope.kind).toBe("single");
    expect(ui.shell.scope.count).toBe(1);
  });

  it("deselecting returns to a SESSION scope (neutral, a rate would no-op)", async () => {
    await ui.openVisualizer();
    await ui.selectGraphNode("b");
    await ui.selectGraphNode(null);
    expect(ui.viewSelection).toBe(null);
    expect(lastCall("set_scope")?.args?.targets).toEqual([]);
    expect(ui.shell.scope.kind).toBe("session");
    expect(ui.shell.scope.count).toBe(0);
  });

  it("re-selecting a different node MOVES the single target (no stale union)", async () => {
    await ui.openVisualizer();
    await ui.selectGraphNode("b");
    expect(ui.shell.scope.previewHashes).toEqual(["b"]);
    await ui.selectGraphNode("x");
    // The scope is the NEW node alone, never [b, x].
    expect(lastCall("set_scope")?.args?.targets).toEqual(["x"]);
    expect(ui.shell.scope.kind).toBe("single");
    expect(ui.shell.scope.previewHashes).toEqual(["x"]);
  });
});

describe("both open gestures (Enter / double-click) share openFromGraph", () => {
  it("Enter on a selected node opens Look on it and clears the selection", async () => {
    await ui.openVisualizer();
    await ui.selectGraphNode("b");
    // Enter routes through openFromGraph(viewSelection).
    await ui.openFromGraph(ui.viewSelection!);
    expect(ui.viewMode).toBe("look");
    expect(ui.look.currentHash).toBe("b");
    // The departed visualizer is left clean for its next open.
    expect(ui.viewSelection).toBe(null);
  });

  it("double-click on an UNselected node opens Look directly (no select step)", async () => {
    await ui.openVisualizer();
    // The dblclick gesture calls openFromGraph(hash) straight, even with nothing
    // selected — it does not require a prior selectGraphNode.
    await ui.openFromGraph("x");
    expect(ui.viewMode).toBe("look");
    expect(ui.look.currentHash).toBe("x");
    expect(ui.viewSelection).toBe(null);
  });
});
