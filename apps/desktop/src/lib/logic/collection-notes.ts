/**
 * Collection-note display logic (B71 — the composer UI slice over the P7.3
 * store). Pure functions over CollectionNoteDto[] so CollectionNotes.svelte
 * stays a thin rendering, mirroring logic/journal.ts.
 *
 * A collection note is about the GROUPING's intent (why these images are
 * together), a deliberately SEPARATE kind from per-image journal events.
 * Append-only: the list is shown chronologically (oldest first, the way a
 * record of intent reads as it accreted), never edited or deleted (K14).
 */
import type { CollectionNoteDto } from "../types/dto";

const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const pad = (n: number): string => String(n).padStart(2, "0");

/** "4 Jun 2026, 14:02" — local time, the way the journal reads (a diary,
 * not a log). Collection notes are sparse and intent-bearing, so each
 * carries its own full stamp rather than session dividers. */
export function formatNoteStamp(ts: string): string {
  const d = new Date(ts);
  const date = `${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
  return `${date}, ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** ULID order IS append order (the journal's byId precedent): a stable
 * lexicographic compare so the list reads oldest-first regardless of the
 * arrival order the snapshot happened to carry. */
export function chronological(notes: CollectionNoteDto[]): CollectionNoteDto[] {
  return [...notes].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}
