/**
 * Topic-note display logic (the collection-notes mirror): pure functions over
 * TopicNoteDto[] so TopicNotes.svelte stays a thin rendering.
 *
 * A topic note is the user's authored text ABOUT the topic (its definition,
 * what it is for). Append-only: the list is shown chronologically (oldest
 * first, the way a record of intent reads as it accreted), never edited or
 * deleted (K14).
 */
import type { TopicNoteDto } from "../types/dto";

const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const pad = (n: number): string => String(n).padStart(2, "0");

/** "4 Jun 2026, 14:02" — local time, the way the collection note reads (a
 * diary, not a log). Each note carries its own full stamp. */
export function formatNoteStamp(ts: string): string {
  const d = new Date(ts);
  const date = `${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
  return `${date}, ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** ULID order IS append order: a stable lexicographic compare so the list
 * reads oldest-first regardless of the arrival order. */
export function chronological(notes: TopicNoteDto[]): TopicNoteDto[] {
  return [...notes].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}
