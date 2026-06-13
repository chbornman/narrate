/**
 * Inspector slice flows against mocked IPC (ui-store.test.ts pattern):
 * load (journal + metadata, stale-response guard), the correct/retract/
 * redact flows, and the sanctioned-toast feed — the slice is the queue's
 * ONLY feeder, the Undo RE-STATES (DECISIONS E4), and the redaction toast
 * carries the report-driven offline-pending copy (UI §7.5).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { JournalEntryDto } from "../src/lib/types/dto";

const ipcLog = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  journal: [] as unknown[],
  redactReport: {
    redacted: ["01A"],
    sidecarsUpdated: 1,
    offlinePending: [] as string[],
  },
  retractResult: true,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    ipcLog.calls.push({ cmd, args });
    switch (cmd) {
      case "image_journal":
        return ipcLog.journal;
      case "image_metadata":
        return { hash: args?.hash, fileName: "IMG_0001.jpg" };
      case "revise_event":
      case "unretract_event":
        return true;
      case "retract_event":
        return ipcLog.retractResult;
      case "redact_event":
        return ipcLog.redactReport;
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { InspectorSlice } from "../src/lib/state/inspector.svelte";
import { toasts } from "../src/lib/primitives/toast.svelte";

const HASH = "ab".repeat(32);

const entry = (id: string, over: Partial<JournalEntryDto> = {}): JournalEntryDto => ({
  id,
  sessionId: "S1",
  ts: "2026-06-04T14:02:00Z",
  kind: "remark",
  source: "typed",
  text: "keep this one",
  originalText: null,
  corrected: false,
  retracted: false,
  rating: null,
  targets: [HASH],
  linkedEvent: null,
  ...over,
});

const calls = (cmd: string) => ipcLog.calls.filter((c) => c.cmd === cmd);

let slice: InspectorSlice;
beforeEach(() => {
  ipcLog.calls = [];
  ipcLog.journal = [entry("01A")];
  ipcLog.retractResult = true;
  ipcLog.redactReport = { redacted: ["01A"], sidecarsUpdated: 1, offlinePending: [] };
  toasts.items = [];
  slice = new InspectorSlice();
});

describe("open/close (frozen contract behavior)", () => {
  it("openTab sets the tab; close clears tab AND flow state", () => {
    slice.openTab("journal");
    expect(slice.open).toBe("journal");
    slice.editingEventId = "01A";
    slice.redactTargetId = "01A";
    slice.close();
    expect(slice.open).toBe(false);
    expect(slice.editingEventId).toBeNull();
    expect(slice.redactTargetId).toBeNull();
  });
});

describe("load", () => {
  it("fetches journal + metadata for the hash", async () => {
    await slice.load(HASH);
    expect(slice.hash).toBe(HASH);
    expect(slice.entries.map((e) => e.id)).toEqual(["01A"]);
    expect(slice.metadata).not.toBeNull();
    expect(calls("image_journal")[0]?.args?.hash).toBe(HASH);
    expect(calls("image_metadata")[0]?.args?.hash).toBe(HASH);
  });

  it("null clears without touching the backend", async () => {
    await slice.load(HASH);
    ipcLog.calls = [];
    await slice.load(null);
    expect(slice.entries).toEqual([]);
    expect(slice.metadata).toBeNull();
    expect(ipcLog.calls).toEqual([]);
  });
});

describe("correct (inline revision)", () => {
  it("commits the revision, closes the edit, and re-folds", async () => {
    await slice.load(HASH);
    slice.editingEventId = "01A";
    ipcLog.calls = [];
    await slice.commitCorrection("01A", "keep this one — it has that stillness");
    expect(slice.editingEventId).toBeNull();
    expect(calls("revise_event")[0]?.args).toEqual({
      eventId: "01A",
      text: "keep this one — it has that stillness",
    });
    expect(calls("image_journal").length).toBe(1); // reloaded
  });

  it("empty text commits nothing — the edit simply closes", async () => {
    slice.editingEventId = "01A";
    await slice.commitCorrection("01A", "   ");
    expect(slice.editingEventId).toBeNull();
    expect(calls("revise_event")).toEqual([]);
  });
});

describe("retract → sanctioned toast 1 with Undo (= RE-STATE, E4)", () => {
  it("retracts, re-folds, and queues exactly one 'retracted' toast", async () => {
    await slice.load(HASH);
    ipcLog.calls = [];
    await slice.retract("01A");
    expect(calls("retract_event")[0]?.args).toEqual({ eventId: "01A" });
    expect(calls("image_journal").length).toBe(1);
    expect(toasts.items.length).toBe(1);
    expect(toasts.items[0].kind).toBe("retracted");
    expect(toasts.items[0].text).toBe("Retracted");
    expect(toasts.items[0].action?.label).toBe("Undo");
  });

  it("the Undo action calls unretract_event (re-state, never un-retract)", async () => {
    await slice.retract("01A");
    ipcLog.calls = [];
    toasts.act(toasts.items[0].id);
    await vi.waitFor(() => {
      expect(calls("unretract_event")[0]?.args).toEqual({ eventId: "01A" });
    });
    expect(toasts.items).toEqual([]); // acting dismisses
  });

  it("an uncommitted retraction toasts nothing", async () => {
    ipcLog.retractResult = false;
    await slice.retract("01A");
    expect(toasts.items).toEqual([]);
  });
});

describe("redact flow (the one modal → sanctioned toast 2)", () => {
  it("beginRedact opens the modal target; cancel clears it", () => {
    slice.beginRedact("01A");
    expect(slice.redactTargetId).toBe("01A");
    slice.cancelRedact();
    expect(slice.redactTargetId).toBeNull();
  });

  it("confirmRedact scrubs, closes the modal, re-folds, and toasts the report copy", async () => {
    await slice.load(HASH);
    slice.beginRedact("01A");
    ipcLog.calls = [];
    await slice.confirmRedact();
    expect(slice.redactTargetId).toBeNull();
    expect(calls("redact_event")[0]?.args).toEqual({ eventId: "01A" });
    expect(calls("image_journal").length).toBe(1);
    expect(toasts.items.length).toBe(1);
    expect(toasts.items[0].kind).toBe("redacted");
    expect(toasts.items[0].text).toBe("Redacted from journal, sidecars, and indexes");
  });

  it("offline-pending volumes appear in the toast copy (§8.4/I4)", async () => {
    ipcLog.redactReport = {
      redacted: ["01A"],
      sidecarsUpdated: 0,
      offlinePending: ["Archive2019"],
    };
    slice.beginRedact("01A");
    await slice.confirmRedact();
    expect(toasts.items[0].text).toBe(
      "Redacted from journal, sidecars, and indexes - 1 offline sidecar pending",
    );
  });

  it("confirm with no target is a no-op", async () => {
    await slice.confirmRedact();
    expect(calls("redact_event")).toEqual([]);
    expect(toasts.items).toEqual([]);
  });
});

describe("sanctioned toast 3 (offline redaction completing on mount)", () => {
  it("names the volume; the kind enum admits nothing else", () => {
    slice.offlineRedactionComplete("Archive2019");
    expect(toasts.items[0].kind).toBe("offline-redaction-complete");
    expect(toasts.items[0].text).toBe('Redaction completed on "Archive2019"');
  });
});
