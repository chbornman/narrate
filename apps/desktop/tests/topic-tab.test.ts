/**
 * The Topics rail tab (DESIGN-TOPICS-COLLECTIONS.md): it lists MANUAL topics
 * (ui.topics, removable/selectable) AND SUGGESTED topics (clusterTopics for the
 * current scope, the note-grounded cluster labels) visually distinguished, with
 * a suggestion click PROMOTING it to a saved topic. Rendered against the ui
 * singleton + mocked IPC.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, fireEvent, waitFor } from "@testing-library/svelte";

const ipcLog = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    ipcLog.calls.push({ cmd, args });
    switch (cmd) {
      case "cluster_topics":
        // Two cluster suggestions; one duplicates a saved manual topic and must
        // be hidden (it lives in the manual group).
        return [
          { label: "foggy harbor", size: 12, centroid_affinity: 0.7 },
          { label: "saved one", size: 5, centroid_affinity: 0.6 },
        ];
      case "add_topic":
        return { id: "tN", phrase: args?.phrase, space: "blend", createdTs: "x" };
      case "list_topics":
        return [{ id: "tN", phrase: args?.phrase, space: "blend", createdTs: "x" }];
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));

import TopicList from "../src/lib/components/rail/TopicList.svelte";
import { ui } from "../src/lib/state/app.svelte";

beforeEach(() => {
  ipcLog.calls.length = 0;
  document.body.innerHTML = "";
  // A folder source so graphScope() resolves (cluster suggestions fetch against it).
  ui.gridScope = { kind: "folder", rootId: "r1", folder: "f" };
  ui.topics = [{ id: "t-saved", phrase: "saved one", space: "blend", createdTs: "x" }];
});

describe("Topics tab lists manual + suggested topics", () => {
  it("renders the manual topic row and the (non-duplicate) suggestion", async () => {
    const { getByText, queryAllByText } = render(TopicList);
    // The saved manual topic shows.
    expect(getByText("saved one")).toBeTruthy();
    // The cluster suggestion that is NOT already saved shows under "suggested".
    await waitFor(() => expect(getByText("foggy harbor")).toBeTruthy());
    expect(getByText("suggested")).toBeTruthy();
    // The duplicate suggestion ("saved one") is NOT shown twice (the saved row
    // is the only occurrence).
    expect(queryAllByText("saved one").length).toBe(1);
  });

  it("clicking a suggestion PROMOTES it to a saved topic (add_topic)", async () => {
    const { getByText } = render(TopicList);
    await waitFor(() => expect(getByText("foggy harbor")).toBeTruthy());
    await fireEvent.click(getByText("foggy harbor"));
    await waitFor(() =>
      expect(ipcLog.calls.some((c) => c.cmd === "add_topic")).toBe(true),
    );
    const add = [...ipcLog.calls].reverse().find((c) => c.cmd === "add_topic");
    expect(add?.args?.phrase).toBe("foggy harbor");
  });

  it("clicking a manual topic scopes the grid to it (a topic scope)", async () => {
    const { getByText } = render(TopicList);
    await fireEvent.click(getByText("saved one"));
    await waitFor(() => expect(ui.gridScope.kind).toBe("topic"));
    if (ui.gridScope.kind === "topic") expect(ui.gridScope.phrase).toBe("saved one");
  });
});
