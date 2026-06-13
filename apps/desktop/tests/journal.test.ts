/**
 * Journal display states (logic/journal.ts — UI §8.2/§8.3, featureset §3):
 * session grouping (newest session FIRST, rows chronological within),
 * the retracted toggle, per-row action availability against the EVENTS
 * §3.4–3.6 target rules, rating copy (C6), timestamp formatting, and the
 * sanctioned-toast copy (§7.5 — report-driven offline-pending clause).
 */
import { describe, expect, it } from "vitest";
import {
  formatSessionDate,
  formatTime,
  offlineRedactionToastText,
  ratingLine,
  redactionToastText,
  RETRACTED_TOAST_TEXT,
  retractedToggleLabel,
  rowActions,
  sessionGroups,
  sessionTimeRange,
  siblingTargetsLabel,
  visibleRows,
} from "../src/lib/logic/journal";
import type { JournalEntryDto } from "../src/lib/types/dto";

/** Local-time ISO so format assertions are timezone-stable. */
const ts = (y: number, mo: number, d: number, h: number, mi: number) =>
  new Date(y, mo - 1, d, h, mi).toISOString();

const entry = (over: Partial<JournalEntryDto> & { id: string }): JournalEntryDto => ({
  sessionId: "S1",
  ts: ts(2026, 6, 4, 14, 2),
  kind: "remark",
  source: "typed",
  text: "the hand is the whole picture",
  originalText: null,
  corrected: false,
  retracted: false,
  rating: null,
  targets: ["ab".repeat(32)],
  linkedEvent: null,
  ...over,
});

describe("session grouping (UI §8.2)", () => {
  it("groups by session, newest session first, rows ascending within", () => {
    const rows = [
      entry({ id: "01A", sessionId: "OLD", ts: ts(2026, 1, 12, 9, 31) }),
      entry({ id: "01C", sessionId: "NEW", ts: ts(2026, 6, 4, 14, 2) }),
      entry({ id: "01B", sessionId: "OLD", ts: ts(2026, 1, 12, 9, 40) }),
      entry({ id: "01D", sessionId: "NEW", ts: ts(2026, 6, 4, 14, 9) }),
    ];
    const groups = sessionGroups(rows);
    expect(groups.map((g) => g.sessionId)).toEqual(["NEW", "OLD"]);
    // Chronological (= ULID/log order) within each session.
    expect(groups[0].rows.map((r) => r.id)).toEqual(["01C", "01D"]);
    expect(groups[1].rows.map((r) => r.id)).toEqual(["01A", "01B"]);
    expect(groups[0].dateLabel).toBe("4 Jun 2026");
    expect(groups[0].timeRange).toBe("14:02 - 14:09");
    expect(groups[1].dateLabel).toBe("12 Jan 2026");
  });

  it("input order is not trusted: rows sort by id (log order)", () => {
    const groups = sessionGroups([
      entry({ id: "01B" }),
      entry({ id: "01A" }),
    ]);
    expect(groups[0].rows.map((r) => r.id)).toEqual(["01A", "01B"]);
  });

  it("counts retracted rows per session for the toggle affordance", () => {
    const groups = sessionGroups([
      entry({ id: "01A" }),
      entry({ id: "01B", retracted: true }),
      entry({ id: "01C", retracted: true }),
    ]);
    expect(groups[0].retractedCount).toBe(2);
  });

  it("zero entries yield zero groups (the EmptyState's case)", () => {
    expect(sessionGroups([])).toEqual([]);
  });
});

describe("retracted toggle (UI §8.2: hidden until toggled)", () => {
  const rows = [
    entry({ id: "01A" }),
    entry({ id: "01B", retracted: true }),
  ];

  it("hides retracted rows by default and shows them when toggled", () => {
    expect(visibleRows(rows, false).map((r) => r.id)).toEqual(["01A"]);
    expect(visibleRows(rows, true).map((r) => r.id)).toEqual(["01A", "01B"]);
  });

  it("labels the affordance with count and direction", () => {
    expect(retractedToggleLabel(1, false)).toBe("show 1 retracted");
    expect(retractedToggleLabel(2, true)).toBe("hide 2 retracted");
  });
});

describe("row actions (UI §8.3 gated by EVENTS §3.4–3.6; Select on every kind except redacted stubs — founder, June 2026)", () => {
  it("live remark: select + all three", () => {
    expect(rowActions(entry({ id: "01A" }))).toEqual({
      select: true,
      correct: true,
      retract: true,
      redact: true,
    });
  });

  it("rating: select + retract (nothing to scrub, nothing to revise)", () => {
    expect(rowActions(entry({ id: "01A", kind: "rating", rating: 4 }))).toEqual({
      select: true,
      correct: false,
      retract: true,
      redact: false,
    });
  });

  it("stroke: select, retract (erase) and redact, never correct", () => {
    expect(rowActions(entry({ id: "01A", kind: "stroke", text: null }))).toEqual({
      select: true,
      correct: false,
      retract: true,
      redact: true,
    });
  });

  it("retracted remark: select + redact only (content persists in log + sidecars; retraction-of-retraction is forbidden, E4)", () => {
    expect(rowActions(entry({ id: "01A", retracted: true }))).toEqual({
      select: true,
      correct: false,
      retract: false,
      redact: true,
    });
  });

  it("retracted rating: select only (its targets are still real)", () => {
    expect(
      rowActions(entry({ id: "01A", kind: "rating", rating: 3, retracted: true })),
    ).toEqual({ select: true, correct: false, retract: false, redact: false });
  });

  it("redacted stub: nothing — content is already gone, and a stub offers no verbs even when the DTO still carries targets", () => {
    expect(
      rowActions(
        entry({ id: "01A", kind: "redacted", text: null, targets: ["ab".repeat(32)] }),
      ),
    ).toEqual({
      select: false,
      correct: false,
      retract: false,
      redact: false,
    });
  });

  it("a targetless row never offers Select (nothing to pick)", () => {
    expect(rowActions(entry({ id: "01A", targets: [] })).select).toBe(false);
  });
});

describe("sibling targets — '+N others' (BACKLOG: journal entries show sibling targets)", () => {
  const [a, b, c] = ["ab".repeat(32), "cd".repeat(32), "ef".repeat(32)];

  it("counts the OTHER targets beyond the inspected image", () => {
    expect(siblingTargetsLabel([a, b], a)).toBe("+1 other");
    expect(siblingTargetsLabel([a, b, c], a)).toBe("+2 others");
    // The inspected image's position in the order does not matter.
    expect(siblingTargetsLabel([b, a, c], a)).toBe("+2 others");
  });

  it("single-target rows and stubs say nothing", () => {
    expect(siblingTargetsLabel([a], a)).toBeNull();
    expect(siblingTargetsLabel([], a)).toBeNull(); // redacted stub
  });

  it("degrades honestly when the inspected hash is unknown or absent", () => {
    // No inspected hash (should not happen with an open panel): full count.
    expect(siblingTargetsLabel([a, b], null)).toBe("+2 others");
    // Inspected image not among the targets: every target is an "other".
    expect(siblingTargetsLabel([a, b], c)).toBe("+2 others");
  });

  it("never counts the inspected image's own pair-mate (B61: the stack badge already says it)", () => {
    // An entry minted against a collapsed pair targets BOTH members
    // (DECISIONS 4) — but the mate is the same picture, so the mark is
    // suppressed entirely…
    expect(siblingTargetsLabel([a, b], a, b)).toBeNull();
    // …and a genuinely different third image counts WITHOUT the mate.
    expect(siblingTargetsLabel([a, b, c], a, b)).toBe("+1 other");
    // Order of the pair in the target list does not matter.
    expect(siblingTargetsLabel([b, a], a, b)).toBeNull();
  });

  it("the pair-mate is ignored when the inspected hash is unknown (a mate of nothing)", () => {
    expect(siblingTargetsLabel([a, b], null, b)).toBe("+2 others");
  });

  it("solo inspected images (mate null) keep the original count", () => {
    expect(siblingTargetsLabel([a, b], a, null)).toBe("+1 other");
  });
});

describe("rating copy (C6: 0 is an explicit clear)", () => {
  it("renders value and the clear case", () => {
    expect(ratingLine(4)).toBe("rating set to 4");
    expect(ratingLine(0)).toBe("rating cleared");
    expect(ratingLine(null)).toBe("rating cleared");
  });
});

describe("timestamp formatting (local time)", () => {
  it("formats row time, divider date, and the session time range", () => {
    expect(formatTime(ts(2026, 6, 4, 14, 2))).toBe("14:02");
    expect(formatSessionDate(ts(2026, 6, 4, 14, 2))).toBe("4 Jun 2026");
    expect(
      sessionTimeRange([
        entry({ id: "01A", ts: ts(2026, 6, 4, 14, 2) }),
        entry({ id: "01B", ts: ts(2026, 6, 4, 14, 9) }),
      ]),
    ).toBe("14:02 - 14:09");
  });

  it("collapses a single-moment session to one time", () => {
    expect(sessionTimeRange([entry({ id: "01A", ts: ts(2026, 6, 4, 14, 2) })])).toBe(
      "14:02",
    );
  });
});

describe("sanctioned-toast copy (UI §7.5 — exactly three triggers)", () => {
  it("retraction toast text is the bare confirmation", () => {
    expect(RETRACTED_TOAST_TEXT).toBe("Retracted");
  });

  it("redaction toast appends the offline-pending clause from the report", () => {
    const base = { redacted: ["01A"], sidecarsUpdated: 2 };
    expect(redactionToastText({ ...base, offlinePending: [] })).toBe(
      "Redacted from journal, sidecars, and indexes",
    );
    expect(redactionToastText({ ...base, offlinePending: ["Archive2019"] })).toBe(
      "Redacted from journal, sidecars, and indexes - 1 offline sidecar pending",
    );
    expect(
      redactionToastText({ ...base, offlinePending: ["Archive2019", "Vault"] }),
    ).toBe("Redacted from journal, sidecars, and indexes - 2 offline sidecars pending");
  });

  it("offline-completion toast names the volume", () => {
    expect(offlineRedactionToastText("Archive2019")).toBe(
      'Redaction completed on "Archive2019"',
    );
  });
});
