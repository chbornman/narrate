/**
 * Collection-note display logic (logic/collection-notes.ts — B71, the
 * composer UI slice over the P7.3 store): chronological (oldest-first)
 * ordering by append order, and the per-note local-time stamp the rail
 * view renders. A collection note is about the grouping's intent, append
 * only — the list reads the way it accreted.
 */
import { describe, expect, it } from "vitest";
import { chronological, formatNoteStamp } from "../src/lib/logic/collection-notes";
import type { CollectionNoteDto } from "../src/lib/types/dto";

/** Local-time ISO so the stamp assertion is timezone-stable. */
const ts = (y: number, mo: number, d: number, h: number, mi: number) =>
  new Date(y, mo - 1, d, h, mi).toISOString();

const note = (id: string, text = "why these are together"): CollectionNoteDto => ({
  id,
  ts: ts(2026, 6, 4, 14, 2),
  text,
});

describe("chronological (append order = ULID order)", () => {
  it("sorts oldest-first by id, regardless of arrival order", () => {
    const out = chronological([note("01C"), note("01A"), note("01B")]);
    expect(out.map((n) => n.id)).toEqual(["01A", "01B", "01C"]);
  });

  it("does not mutate the input array", () => {
    const input = [note("01B"), note("01A")];
    chronological(input);
    expect(input.map((n) => n.id)).toEqual(["01B", "01A"]);
  });

  it("is stable for an already-ordered, single, or empty list", () => {
    expect(chronological([]).length).toBe(0);
    expect(chronological([note("01A")]).map((n) => n.id)).toEqual(["01A"]);
    expect(chronological([note("01A"), note("01B")]).map((n) => n.id)).toEqual([
      "01A",
      "01B",
    ]);
  });
});

describe("formatNoteStamp (local-time, the journal's diary register)", () => {
  it("renders date and zero-padded 24h time", () => {
    expect(formatNoteStamp(ts(2026, 6, 4, 14, 2))).toBe("4 Jun 2026, 14:02");
  });

  it("pads single-digit hours and minutes", () => {
    expect(formatNoteStamp(ts(2026, 1, 9, 9, 5))).toBe("9 Jan 2026, 09:05");
  });

  it("handles midnight", () => {
    expect(formatNoteStamp(ts(2026, 12, 31, 0, 0))).toBe("31 Dec 2026, 00:00");
  });
});
