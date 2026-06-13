/**
 * Collection-notes slice flows against mocked IPC (inspector-slice.test.ts
 * pattern): load (stale-response guard) and compose (optimistic append,
 * empty-text refusal, the draft-survives-a-failure contract). The slice
 * reuses the P7.3 commands `collection_notes` and `add_collection_note` —
 * no new backend surface.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CollectionNoteDto } from "../src/lib/types/dto";

const backend = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  /** Notes per collection id (collection_notes). */
  notes: {} as Record<string, CollectionNoteDto[]>,
  /** When set, collection_notes parks on this promise (the stale race). */
  notesGate: null as Promise<void> | null,
  /** When true, add_collection_note throws (backend-unavailable path). */
  addThrows: false,
  /** Counter that mints note ids for add_collection_note. */
  seq: 0,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    backend.calls.push({ cmd, args });
    switch (cmd) {
      case "collection_notes":
        if (backend.notesGate !== null) await backend.notesGate;
        return backend.notes[String(args?.id)] ?? [];
      case "add_collection_note": {
        if (backend.addThrows) throw new Error("backend down");
        const id = String(args?.id);
        const note: CollectionNoteDto = {
          id: `01N${backend.seq++}`,
          ts: "2026-06-04T14:02:00.000Z",
          text: String(args?.text),
        };
        (backend.notes[id] ??= []).push(note);
        return note;
      }
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { CollectionNotesSlice } from "../src/lib/state/collection-notes.svelte";

const note = (id: string, text: string): CollectionNoteDto => ({
  id,
  ts: "2026-06-04T14:02:00.000Z",
  text,
});

const lastCall = (cmd: string) =>
  [...backend.calls].reverse().find((c) => c.cmd === cmd);

let slice: CollectionNotesSlice;
beforeEach(() => {
  backend.calls.length = 0;
  backend.notes = { "01A": [note("01A0", "the cold snap series")] };
  backend.notesGate = null;
  backend.addThrows = false;
  backend.seq = 0;
  slice = new CollectionNotesSlice();
});

describe("load", () => {
  it("pulls the collection's notes and tracks the id", async () => {
    await slice.load("01A");
    expect(slice.id).toBe("01A");
    expect(slice.notes.map((n) => n.text)).toEqual(["the cold snap series"]);
    expect(lastCall("collection_notes")?.args).toEqual({ id: "01A" });
  });

  it("a null id clears the panel without an ipc call", async () => {
    await slice.load("01A");
    backend.calls.length = 0;
    await slice.load(null);
    expect(slice.id).toBeNull();
    expect(slice.notes).toEqual([]);
    expect(lastCall("collection_notes")).toBeUndefined();
  });

  it("a held-up response loses to a newer load (stale-response guard)", async () => {
    backend.notes = {
      "01A": [note("01A0", "first collection")],
      "01B": [note("01B0", "second collection")],
    };
    let release!: () => void;
    backend.notesGate = new Promise<void>((r) => (release = r));
    const stale = slice.load("01A");
    backend.notesGate = null;
    await slice.load("01B"); // newer load lands first
    release();
    await stale; // the stale response arrives last but must not win
    expect(slice.id).toBe("01B");
    expect(slice.notes.map((n) => n.text)).toEqual(["second collection"]);
  });
});

describe("compose (append-only)", () => {
  it("appends a note and reuses add_collection_note", async () => {
    await slice.load("01A");
    const ok = await slice.compose("started during the cold snap");
    expect(ok).toBe(true);
    expect(lastCall("add_collection_note")?.args).toEqual({
      id: "01A",
      text: "started during the cold snap",
    });
    // Optimistic append: the new note lands at the end (chronological).
    expect(slice.notes.map((n) => n.text)).toEqual([
      "the cold snap series",
      "started during the cold snap",
    ]);
  });

  it("empty or whitespace text commits nothing", async () => {
    await slice.load("01A");
    expect(await slice.compose("   ")).toBe(false);
    expect(await slice.compose("")).toBe(false);
    expect(lastCall("add_collection_note")).toBeUndefined();
    expect(slice.notes).toHaveLength(1);
  });

  it("with no collection open, commits nothing", async () => {
    expect(await slice.compose("orphan note")).toBe(false);
    expect(lastCall("add_collection_note")).toBeUndefined();
  });

  it("a backend failure keeps the draft (resolves false, no append)", async () => {
    await slice.load("01A");
    backend.addThrows = true;
    expect(await slice.compose("never lands")).toBe(false);
    expect(slice.notes).toHaveLength(1);
  });

  it("an append for a collection the view has left does not land", async () => {
    // Slow add: the view switches collections before it resolves. The
    // optimistic append must guard against landing in the wrong list.
    await slice.load("01A");
    const composing = slice.compose("late arrival");
    await slice.load("01B"); // view moves on (01B has no seeded notes)
    await composing;
    expect(slice.id).toBe("01B");
    expect(slice.notes).toEqual([]); // not poisoned by 01A's note
  });
});
