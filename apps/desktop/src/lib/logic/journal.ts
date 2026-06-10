/**
 * Journal display states (UI §8.2, featureset §3/D2) — pure logic over
 * JournalEntryDto[] so JournalTab/JournalEntry stay thin renderings:
 * session grouping (newest session first, rows chronological within),
 * revision folding ("edited" affordance over text the backend already
 * folded), the retracted toggle, redacted stubs, per-row action
 * availability (EVENTS §3.4–3.6 target rules), and the sanctioned-toast
 * copy. The M4 per-image timeline is a rendering upgrade over these same
 * groups — nothing here changes (M4-proofing).
 *
 * The stroke kind is the M2a seat: rows exist in the union and render as
 * minimal stubs until the pencil packet brings micro-previews.
 */
import type { JournalEntryDto, RedactReportDto } from "../types/dto";

// ---- session grouping -------------------------------------------------------

export interface SessionGroup {
  sessionId: string;
  /** Divider date, e.g. "4 Jun 2026" (UI §8.2: date + time range). */
  dateLabel: string;
  /** "14:02" or "14:02–14:09". */
  timeRange: string;
  /** Chronological within the session (log order = ULID order). */
  rows: JournalEntryDto[];
  /** Rows hidden behind the "show retracted" toggle. */
  retractedCount: number;
}

/** ULID order IS log order (EVENTS §1.3): lexicographic id compare. */
function byId(a: JournalEntryDto, b: JournalEntryDto): number {
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/**
 * Group entries under session dividers: NEWEST session first, rows
 * ascending within each (the §8.2 sketch: 14:02 before 14:09 under the
 * newest divider). Input order is not trusted.
 */
export function sessionGroups(entries: JournalEntryDto[]): SessionGroup[] {
  const sorted = [...entries].sort(byId);
  const bySession = new Map<string, JournalEntryDto[]>();
  for (const e of sorted) {
    const rows = bySession.get(e.sessionId);
    if (rows === undefined) bySession.set(e.sessionId, [e]);
    else rows.push(e);
  }
  const groups: SessionGroup[] = [];
  for (const [sessionId, rows] of bySession) {
    groups.push({
      sessionId,
      dateLabel: formatSessionDate(rows[0].ts),
      timeRange: sessionTimeRange(rows),
      rows,
      retractedCount: rows.filter((r) => r.retracted).length,
    });
  }
  // Newest session first = greatest last-row id first.
  groups.sort((a, b) => {
    const la = a.rows[a.rows.length - 1].id;
    const lb = b.rows[b.rows.length - 1].id;
    return la < lb ? 1 : la > lb ? -1 : 0;
  });
  return groups;
}

/** Rows the panel shows: retracted hidden unless toggled (UI §8.2). */
export function visibleRows(
  rows: JournalEntryDto[],
  showRetracted: boolean,
): JournalEntryDto[] {
  return showRetracted ? rows : rows.filter((r) => !r.retracted);
}

/** The toggle affordance copy: "show 1 retracted" / "hide 2 retracted". */
export function retractedToggleLabel(count: number, shown: boolean): string {
  return `${shown ? "hide" : "show"} ${count} retracted`;
}

// ---- per-row states & actions ------------------------------------------------

/** The three quiet hover actions (UI §8.3), gated by EVENTS target rules. */
export interface RowActions {
  /** Inline revision — remarks only (EVENTS §3.4: revisions target
   * remarks/revisions; other kinds are rejected at append). */
  correct: boolean;
  /** Tombstone — any live content row (EVENTS §3.5); never twice. */
  retract: boolean;
  /** The one modal — remarks and strokes (EVENTS §3.6: ratings hold no
   * scrubable content — retract instead). A RETRACTED remark/stroke stays
   * redactable: its content still exists in the log and sidecars. */
  redact: boolean;
}

export function rowActions(e: JournalEntryDto): RowActions {
  if (e.kind === "redacted") return { correct: false, retract: false, redact: false };
  if (e.retracted)
    return { correct: false, retract: false, redact: e.kind !== "rating" };
  return {
    correct: e.kind === "remark",
    retract: true,
    redact: e.kind !== "rating",
  };
}

/** Rating row copy (C6: 0 is an explicit clear, distinct from unrated). */
export function ratingLine(value: number | null): string {
  if (value === null || value === 0) return "rating cleared";
  return `rating set to ${value}`;
}

// ---- timestamp formatting (local time — the journal reads as a diary) --------

const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const pad = (n: number): string => String(n).padStart(2, "0");

/** "14:02" — row timestamps (UI §8.2 sketch). */
export function formatTime(ts: string): string {
  const d = new Date(ts);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** "4 Jun 2026" — session divider date. */
export function formatSessionDate(ts: string): string {
  const d = new Date(ts);
  return `${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
}

/** "14:02–14:09", collapsing to "14:02" for single-moment sessions. */
export function sessionTimeRange(rows: JournalEntryDto[]): string {
  const first = formatTime(rows[0].ts);
  const last = formatTime(rows[rows.length - 1].ts);
  return first === last ? first : `${first}–${last}`;
}

// ---- sanctioned-toast copy (UI §7.5 — exactly three triggers) -----------------

/** Toast 1: retraction committed; the Undo RE-STATES (DECISIONS E4). */
export const RETRACTED_TOAST_TEXT = "Retracted";

/** Toast 2: redaction completed — report-driven, appending the offline
 * pending clause when sidecars wait on unmounted volumes (UI §7.5/§8.4). */
export function redactionToastText(report: RedactReportDto): string {
  const base = "Redacted from journal, sidecars, and indexes";
  const n = report.offlinePending.length;
  if (n === 0) return base;
  return `${base} — ${n} offline sidecar${n === 1 ? "" : "s"} pending`;
}

/** Toast 3: a pending offline-volume redaction completing on mount. */
export function offlineRedactionToastText(volumeLabel: string): string {
  return `Redaction completed on "${volumeLabel}"`;
}
