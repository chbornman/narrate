/**
 * Inspector rendering on real DOM (search-render.test.ts pattern, where
 * the DOM is load-bearing): JournalTab display states (UI §8.2 — dividers,
 * folds, retracted toggle, redacted stubs), JournalEntry hover actions
 * (§8.3), the RedactionModal frame (R5/§8.4 — required copy incl. the I4
 * offline-volume and external-copy limits, default-focus Cancel,
 * hold-to-confirm), and MetadataTab's read-only rows.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";

const ipcLog = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    ipcLog.calls.push({ cmd, args });
    switch (cmd) {
      case "image_journal":
        return [];
      case "image_metadata":
        return null;
      case "retract_event":
        return true;
      case "report_activity":
        return "S1";
      case "redact_event":
        return { redacted: ["01A"], sidecarsUpdated: 1, offlinePending: [] };
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { ui } from "../src/lib/state/app.svelte";
import { toasts } from "../src/lib/primitives/toast.svelte";
import JournalTab from "../src/lib/components/inspector/JournalTab.svelte";
import JournalEntry from "../src/lib/components/inspector/JournalEntry.svelte";
import MetadataTab from "../src/lib/components/inspector/MetadataTab.svelte";
import RedactionModal from "../src/lib/components/inspector/RedactionModal.svelte";
import type { JournalEntryDto } from "../src/lib/types/dto";
import type { Action } from "../src/lib/logic/keymap";

const HASH = "ab".repeat(32);

const entry = (id: string, over: Partial<JournalEntryDto> = {}): JournalEntryDto => ({
  id,
  sessionId: "S1",
  ts: "2026-06-04T14:02:00Z",
  kind: "remark",
  source: "voice",
  text: "the hand is the whole picture",
  originalText: null,
  corrected: false,
  retracted: false,
  rating: null,
  targets: [HASH],
  linkedEvent: null,
  ...over,
});

beforeEach(() => {
  document.body.innerHTML = "";
  ipcLog.calls = [];
  toasts.items = [];
  ui.roots = [];
  ui.inspector.entries = [];
  ui.inspector.metadata = null;
  ui.inspector.showRetracted = false;
  ui.inspector.editingEventId = null;
  ui.inspector.redactTargetId = null;
});

describe("JournalTab display states (UI §8.2)", () => {
  it("zero events read only 'Nothing yet.' — no prompt to add notes", () => {
    const { container } = render(JournalTab);
    expect(container.textContent?.trim()).toBe("Nothing yet.");
  });

  it("renders session dividers and verbatim remarks", () => {
    ui.inspector.entries = [entry("01A")];
    const { container } = render(JournalTab);
    expect(container.textContent).toContain("Session ·");
    expect(container.textContent).toContain("“the hand is the whole picture”");
  });

  it("retracted entries hide behind the toggle; shown, they strike through", async () => {
    ui.inspector.entries = [
      entry("01A"),
      entry("01B", { text: "too literal", retracted: true }),
    ];
    const { container } = render(JournalTab);
    expect(container.textContent).not.toContain("too literal");
    const toggle = screen.getByRole("button", { name: /show 1 retracted/ });
    await fireEvent.click(toggle);
    expect(container.textContent).toContain("too literal");
    expect(container.querySelector(".row.retracted")).not.toBeNull();
    // The affordance flips its copy.
    expect(screen.getByRole("button", { name: /hide 1 retracted/ })).toBeTruthy();
  });

  it("redacted items show only the '[redacted]' marker line", () => {
    ui.inspector.entries = [entry("01A", { kind: "redacted", text: null })];
    const { container } = render(JournalTab);
    expect(container.textContent).toContain("[redacted]");
    expect(container.textContent).not.toContain("“");
  });

  it("corrected rows show the fold with an 'edited' expand to the original", async () => {
    ui.inspector.entries = [
      entry("01A", { text: "moody light", corrected: true, originalText: "muddy light" }),
    ];
    const { container } = render(JournalTab);
    expect(container.textContent).toContain("“moody light”");
    expect(container.textContent).not.toContain("muddy light");
    await fireEvent.click(screen.getByRole("button", { name: /edited/ }));
    expect(container.textContent).toContain("originally: “muddy light”");
  });

  it("ratings render as quiet fold lines (C6: 0 = cleared)", () => {
    ui.inspector.entries = [
      entry("01A", { kind: "rating", text: null, rating: 4 }),
      entry("01B", { kind: "rating", text: null, rating: 0 }),
    ];
    const { container } = render(JournalTab);
    expect(container.textContent).toContain("rating set to 4");
    expect(container.textContent).toContain("rating cleared");
  });

  it("the Retract row rides ui.perform: a committed retraction also leaves the pencil undo stack (CAPTURE §8.5)", async () => {
    // THE UI path of the journal panel — routing the row verb around the
    // dispatch case would leave the stale stack entry for Ctrl+Z to mint
    // a duplicate tombstone against.
    ui.inspector.entries = [entry("01A", { kind: "stroke", text: null, stroke: null })];
    ui.look.undoStack = [];
    ui.look.undoSessionId = null;
    ui.look.onStrokeCommitted("01A", HASH, "S1");
    render(JournalTab);
    await fireEvent.click(screen.getByRole("button", { name: "Retract" }));
    await vi.waitFor(() => {
      expect(ipcLog.calls.some((c) => c.cmd === "retract_event")).toBe(true);
      expect(ui.look.undoStack).toEqual([]);
    });
  });
});

describe("JournalEntry hover actions (UI §8.3)", () => {
  it("a live remark offers Correct · Retract · Redact… as Actions", async () => {
    const seen: Action[] = [];
    render(JournalEntry, {
      entry: entry("01A"),
      onaction: (a: Action) => seen.push(a),
      oncorrect: () => {},
    });
    await fireEvent.click(screen.getByRole("button", { name: "Correct" }));
    await fireEvent.click(screen.getByRole("button", { name: "Retract" }));
    await fireEvent.click(screen.getByRole("button", { name: "Redact…" }));
    expect(seen).toEqual([
      { kind: "journal-correct", eventId: "01A" },
      { kind: "journal-retract", eventId: "01A" },
      { kind: "journal-redact", eventId: "01A" },
    ]);
  });

  it("a rating offers Select + Retract; a redacted stub offers nothing", () => {
    render(JournalEntry, {
      entry: entry("01A", { kind: "rating", text: null, rating: 3 }),
      onaction: () => {},
      oncorrect: () => {},
    });
    // Select rides every kind except stubs (founder, June 2026).
    expect(screen.getByRole("button", { name: "Select" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retract" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Correct" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Redact…" })).toBeNull();

    document.body.innerHTML = "";
    render(JournalEntry, {
      entry: entry("01B", { kind: "redacted", text: null }),
      onaction: () => {},
      oncorrect: () => {},
    });
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("linked rows carry the subtle link mark on both partners (P6.1, UI §8.2)", () => {
    // The voice remark carries the stored backward pointer …
    render(JournalEntry, {
      entry: entry("01B", { linkedEvent: "01A" }),
      onaction: () => {},
      oncorrect: () => {},
    });
    expect(screen.getByTitle("Linked — drawn and spoken together")).toBeTruthy();

    // … and the earlier stroke shows the resolved incoming link too
    // (the backend traverses the one-way pointer in both directions).
    document.body.innerHTML = "";
    render(JournalEntry, {
      entry: entry("01A", {
        kind: "stroke",
        text: null,
        linkedEvent: "01B",
        stroke: { baseW: 40, orientation: 1, points: [[100, 100, 1000, 0]] },
      }),
      onaction: () => {},
      oncorrect: () => {},
    });
    expect(screen.getByTitle("Linked — drawn and spoken together")).toBeTruthy();

    // Unlinked rows show no mark — silence while marking is common.
    document.body.innerHTML = "";
    render(JournalEntry, {
      entry: entry("01C"),
      onaction: () => {},
      oncorrect: () => {},
    });
    expect(screen.queryByTitle("Linked — drawn and spoken together")).toBeNull();
  });

  it("editing renders a textarea seeded with the folded text; Enter commits", async () => {
    const corrected: [string, string][] = [];
    render(JournalEntry, {
      entry: entry("01A", { text: "moody light" }),
      editing: true,
      onaction: () => {},
      oncorrect: (id: string, text: string) => corrected.push([id, text]),
    });
    const edit = screen.getByRole("textbox", { name: "Correct this entry" });
    expect((edit as HTMLTextAreaElement).value).toBe("moody light");
    await fireEvent.input(edit, { target: { value: "broody light" } });
    await fireEvent.keyDown(edit, { key: "Enter" });
    expect(corrected).toEqual([["01A", "broody light"]]);
  });
});

describe("RedactionModal — the app's ONE modal (R5, §8.4)", () => {
  it("renders nothing without a target", () => {
    const { container } = render(RedactionModal);
    expect(container.querySelector(".frame")).toBeNull();
  });

  it("carries the REQUIRED copy: the quote, what it does, what it does NOT do (I4)", () => {
    ui.inspector.entries = [entry("01A", { text: "the secret" })];
    ui.inspector.redactTargetId = "01A";
    const { container } = render(RedactionModal);
    const text = (container.textContent ?? "").replace(/\s+/g, " ");
    expect(text).toContain("“the secret”"); // item 1: verbatim, quoted
    // item 2: does — db, indexes/vectors, sidecars, marker, no undo/merge-back
    expect(text).toContain("database");
    expect(text).toContain("search index");
    expect(text).toContain("sidecar");
    expect(text).toContain("marker");
    expect(text).toContain("never be undone");
    // item 3: does NOT — external copies; offline-volume latency
    expect(text).toContain("cannot scrub copies exported or backed up outside Photoproof");
    expect(text).toContain("offline volumes are scrubbed only when that volume next connects");
  });

  it("names currently-offline volumes when applicable (§8.4 item 3)", () => {
    ui.roots = [
      {
        rootId: "r1",
        displayName: "Archive2019",
        relPath: "",
        volumeId: "v1",
        online: false,
        absPath: null,
      },
    ];
    ui.inspector.entries = [entry("01A")];
    ui.inspector.redactTargetId = "01A";
    const { container } = render(RedactionModal);
    expect(container.textContent).toContain("offline now: Archive2019");
  });

  it("default focus lands on Cancel; Cancel closes without redacting", async () => {
    ui.inspector.entries = [entry("01A")];
    ui.inspector.redactTargetId = "01A";
    render(RedactionModal);
    const cancel = screen.getByRole("button", { name: "Cancel" });
    await vi.waitFor(() => expect(document.activeElement).toBe(cancel));
    await fireEvent.click(cancel);
    expect(ui.inspector.redactTargetId).toBeNull();
    expect(ipcLog.calls.filter((c) => c.cmd === "redact_event")).toEqual([]);
  });

  it("a held confirm redacts; an early release does not (confirmhold)", async () => {
    ui.inspector.entries = [entry("01A")];
    ui.inspector.redactTargetId = "01A";
    render(RedactionModal);
    const confirm = screen.getByRole("button", { name: /Hold to redact permanently/ });
    vi.useFakeTimers();
    try {
      // Early release: nothing happens.
      await fireEvent.pointerDown(confirm);
      await vi.advanceTimersByTimeAsync(400);
      await fireEvent.pointerUp(confirm);
      await vi.advanceTimersByTimeAsync(2000);
      expect(ipcLog.calls.filter((c) => c.cmd === "redact_event")).toEqual([]);
      // Full hold: the redaction commits once.
      await fireEvent.pointerDown(confirm);
      await vi.advanceTimersByTimeAsync(1600);
      expect(
        ipcLog.calls.filter((c) => c.cmd === "redact_event").map((c) => c.args),
      ).toEqual([{ eventId: "01A" }]);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("MetadataTab (K16: read-only)", () => {
  it("renders 'No image.' without metadata", () => {
    const { container } = render(MetadataTab);
    expect(container.textContent?.trim()).toBe("No image.");
  });

  it("renders labeled read-only rows with copy affordances on hash/path", () => {
    ui.inspector.metadata = {
      hash: HASH,
      fileName: "IMG_4471.jpg",
      relPath: "iceland/IMG_4471.jpg",
      absPath: "/mnt/vol/iceland/IMG_4471.jpg",
      byteSize: 25_480_000,
      format: "jpeg",
      pixelWidth: 6000,
      pixelHeight: 4000,
      orientation: 1,
      captureTs: null,
      cameraMake: "Fujifilm",
      cameraModel: "X-T5",
      lensModel: null,
      focalLengthMm: null,
      iso: 400,
      fNumber: null,
      exposureTime: null,
      gps: null,
      previewSource: "embedded",
      previewPending: false,
      firstIngestedAt: "2026-06-01T08:00:00Z",
    };
    const { container } = render(MetadataTab);
    expect(container.textContent).toContain("Fujifilm X-T5");
    expect(container.textContent).toContain("6000 × 4000");
    expect(container.textContent).toContain(HASH);
    expect(screen.getByRole("button", { name: "Copy Hash" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Copy Path" })).toBeTruthy();
    // Read-only: no inputs, ever (K16).
    expect(container.querySelector("input, textarea, select")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Multi-select header (founder, June 2026): the panel shows the ANCHOR
// image's truth with a quiet "N selected" line — grid only, ≥2 targets.
// ---------------------------------------------------------------------------
import Inspector from "../src/lib/components/inspector/Inspector.svelte";
import type { GridItem } from "../src/lib/types/dto";

// jsdom has no ResizeObserver; the Panel primitive's sizing needs one.
class ResizeObserverShim {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver ??= ResizeObserverShim as typeof ResizeObserver;

const gridItem = (relPath: string): GridItem => ({
  hash: `h:${relPath}`,
  fileName: relPath.split("/").pop() as string,
  relPath,
  captureTs: null,
  addedTs: "2026-06-01T00:00:00Z",
  hasJournal: false,
  rating: null,
  offline: false,
});

describe("multi-select header — anchor image + 'N selected' (grid only)", () => {
  it("renders the quiet count with several images selected", () => {
    ui.surface = "grid";
    ui.inspector.open = "journal";
    ui.grid.rawItems = [gridItem("a.jpg"), gridItem("b.jpg"), gridItem("c.jpg")];
    ui.grid.sel = { order: ["h:a.jpg", "h:b.jpg", "h:c.jpg"], focus: 0, anchor: 0 };
    render(Inspector);
    expect(screen.getByText("3 selected")).toBeTruthy();
  });

  it("stays silent for a single selection and outside the grid", () => {
    ui.surface = "grid";
    ui.inspector.open = "journal";
    ui.grid.rawItems = [gridItem("a.jpg"), gridItem("b.jpg")];
    ui.grid.sel = { order: ["h:a.jpg"], focus: 0, anchor: 0 };
    render(Inspector);
    expect(screen.queryByText(/selected/)).toBeNull();

    document.body.innerHTML = "";
    ui.surface = "look";
    ui.grid.sel = { order: ["h:a.jpg", "h:b.jpg"], focus: 0, anchor: 0 };
    render(Inspector);
    expect(screen.queryByText(/selected/)).toBeNull();
  });
});

describe("'+N others' vs the inspected image's pair-mate (DECISIONS B61)", () => {
  // A collapsed RAW+JPEG pair in the grid: an entry minted against the
  // pair targets BOTH members (DECISIONS 4), but the mate is the same
  // picture — the stack badge already says "2", so the sibling mark must
  // stay silent unless a genuinely DIFFERENT image is targeted.
  const setup = () => {
    ui.grid.setItems([gridItem("a/IMG_1.jpg"), gridItem("a/IMG_1.cr2"), gridItem("a/IMG_2.jpg")]);
    ui.inspector.hash = "h:a/IMG_1.jpg";
  };

  it("suppresses the mark when the only extra target is the pair-mate", () => {
    setup();
    ui.inspector.entries = [
      entry("01A", { targets: ["h:a/IMG_1.jpg", "h:a/IMG_1.cr2"] }),
    ];
    const { container } = render(JournalTab);
    expect(container.querySelector(".siblings")).toBeNull();
  });

  it("counts only genuinely different images beyond the pair", () => {
    setup();
    ui.inspector.entries = [
      entry("01A", {
        targets: ["h:a/IMG_1.jpg", "h:a/IMG_1.cr2", "h:a/IMG_2.jpg"],
      }),
    ];
    render(JournalTab);
    expect(screen.getByText("+1 other")).toBeTruthy();
  });

  it("a non-pair sibling still reads '+1 other' (the original mark)", () => {
    setup();
    ui.inspector.hash = "h:a/IMG_2.jpg"; // solo image — no mate
    ui.inspector.entries = [
      entry("01A", { targets: ["h:a/IMG_2.jpg", "h:a/IMG_1.jpg"] }),
    ];
    render(JournalTab);
    expect(screen.getByText("+1 other")).toBeTruthy();
  });
});
