/**
 * Topic-notes slice flows against mocked IPC (collection-notes-slice.test.ts
 * mirror): load (stale-response guard) and compose (optimistic append,
 * empty-text refusal, the draft-survives-a-failure contract). The slice drives
 * the `topic_notes` and `add_topic_note` commands - a topic's append-only note
 * log, keyed to the topic id.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TopicNoteDto } from "../src/lib/types/dto";

const backend = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  /** Notes per topic id (topic_notes). */
  notes: {} as Record<string, TopicNoteDto[]>,
  /** When set, topic_notes parks on this promise (the stale race). */
  notesGate: null as Promise<void> | null,
  /** When true, add_topic_note throws (backend-unavailable path). */
  addThrows: false,
  /** Counter that mints note ids for add_topic_note. */
  seq: 0,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    backend.calls.push({ cmd, args });
    switch (cmd) {
      case "topic_notes":
        if (backend.notesGate !== null) await backend.notesGate;
        return backend.notes[String(args?.id)] ?? [];
      case "add_topic_note": {
        if (backend.addThrows) throw new Error("backend down");
        const id = String(args?.id);
        const note: TopicNoteDto = {
          id: `01N${backend.seq++}`,
          ts: "2026-06-13T14:02:00.000Z",
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

import { TopicNotesSlice } from "../src/lib/state/topic-notes.svelte";

const note = (id: string, text: string): TopicNoteDto => ({
  id,
  ts: "2026-06-13T14:02:00.000Z",
  text,
});

const lastCall = (cmd: string) =>
  [...backend.calls].reverse().find((c) => c.cmd === cmd);

let slice: TopicNotesSlice;
beforeEach(() => {
  backend.calls.length = 0;
  backend.notes = { "01A": [note("01A0", "the harbor lens")] };
  backend.notesGate = null;
  backend.addThrows = false;
  backend.seq = 0;
  slice = new TopicNotesSlice();
});

describe("load", () => {
  it("pulls the topic's notes and tracks the id", async () => {
    await slice.load("01A");
    expect(slice.id).toBe("01A");
    expect(slice.notes.map((n) => n.text)).toEqual(["the harbor lens"]);
    expect(lastCall("topic_notes")?.args).toEqual({ id: "01A" });
  });

  it("a null id clears the panel without an ipc call", async () => {
    await slice.load("01A");
    backend.calls.length = 0;
    await slice.load(null);
    expect(slice.id).toBeNull();
    expect(slice.notes).toEqual([]);
    expect(lastCall("topic_notes")).toBeUndefined();
  });

  it("a held-up response loses to a newer load (stale-response guard)", async () => {
    backend.notes = {
      "01A": [note("01A0", "first topic")],
      "01B": [note("01B0", "second topic")],
    };
    let release!: () => void;
    backend.notesGate = new Promise<void>((r) => (release = r));
    const stale = slice.load("01A");
    backend.notesGate = null;
    await slice.load("01B"); // newer load lands first
    release();
    await stale; // the stale response arrives last but must not win
    expect(slice.id).toBe("01B");
    expect(slice.notes.map((n) => n.text)).toEqual(["second topic"]);
  });
});

describe("compose (append-only)", () => {
  it("appends a note and reuses add_topic_note", async () => {
    await slice.load("01A");
    const ok = await slice.compose("refine toward dusk shots");
    expect(ok).toBe(true);
    expect(lastCall("add_topic_note")?.args).toEqual({
      id: "01A",
      text: "refine toward dusk shots",
    });
    // Optimistic append: the new note lands at the end (chronological).
    expect(slice.notes.map((n) => n.text)).toEqual([
      "the harbor lens",
      "refine toward dusk shots",
    ]);
  });

  it("empty or whitespace text commits nothing", async () => {
    await slice.load("01A");
    expect(await slice.compose("   ")).toBe(false);
    expect(await slice.compose("")).toBe(false);
    expect(lastCall("add_topic_note")).toBeUndefined();
    expect(slice.notes).toHaveLength(1);
  });

  it("with no topic open, commits nothing", async () => {
    expect(await slice.compose("orphan note")).toBe(false);
    expect(lastCall("add_topic_note")).toBeUndefined();
  });

  it("a backend failure keeps the draft (resolves false, no append)", async () => {
    await slice.load("01A");
    backend.addThrows = true;
    expect(await slice.compose("never lands")).toBe(false);
    expect(slice.notes).toHaveLength(1);
  });

  it("an append for a topic the view has left does not land", async () => {
    // Slow add: the view switches topics before it resolves. The optimistic
    // append must guard against landing in the wrong list.
    await slice.load("01A");
    const composing = slice.compose("late arrival");
    await slice.load("01B"); // view moves on (01B has no seeded notes)
    await composing;
    expect(slice.id).toBe("01B");
    expect(slice.notes).toEqual([]); // not poisoned by 01A's note
  });
});
