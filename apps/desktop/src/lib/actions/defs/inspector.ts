/**
 * Inspector rows — STAGE C OWNS THIS FILE (featureset §3, D2: the journal
 * panel ships in P4.2; the P3.2 "J does not exist" keymap guard retires
 * here, tested in inspector-keys.test.ts, never by editing existing
 * keymap blocks).
 *
 * One def carries both tabs: `I` opens Metadata, `J` opens Journal, and
 * pressing the OPEN tab's key closes the panel (toggle symmetry; Esc
 * closes it from Grid — escape layer 12). The same def's options seat the
 * Metadata/Journal items on the thumb menu (the frozen SEAT_ORDER slot
 * `open-inspector` renders them as an Inspector ▸ radio submenu).
 *
 * The journal ROW verbs (Correct / Retract / Redact… / show-retracted)
 * are deliberately NOT registry rows: they are per-row hover affordances
 * over per-row data (UI §8.3), not context-gated chords or seatable menu
 * items — their availability is pure row logic (logic/journal.ts
 * rowActions, tested), and JournalTab dispatches their Actions directly.
 *
 * Select-from-note (BACKLOG polish) IS a registry row: it is seatable
 * (the journal-row context menu renders it) and pointer-only — the row's
 * quiet "Select" affordance dispatches the same Action.
 */
import type { ActionDef } from "../types";

const TABS = ["metadata", "journal"] as const;
const TAB_LABELS: Record<(typeof TABS)[number], string> = {
  metadata: "Metadata",
  journal: "Journal",
};

export const INSPECTOR_DEFS: ActionDef[] = [
  {
    id: "open-inspector",
    verb: "Inspector",
    label: "Inspector - I Metadata · J Journal (Esc closes)",
    keys: [
      { key: "i", arg: "metadata" },
      { key: "j", arg: "journal" },
    ],
    scope: "global", // per-image truth is reachable from Grid and Look
    group: "panels",
    seats: ["thumb"],
    available: () => true,
    toAction: (ctx, arg) => {
      const tab = arg as (typeof TABS)[number];
      // Toggle: the open tab's own key closes (I/J round-trip).
      return ctx.inspectorOpen === tab
        ? { kind: "close-inspector" }
        : { kind: "open-inspector", tab };
    },
    options: (ctx) =>
      TABS.map((tab) => ({
        arg: tab,
        label: TAB_LABELS[tab],
        checked: ctx.inspectorOpen === tab,
      })),
  },
  {
    id: "select-journal-targets",
    verb: "Select in grid",
    label: "Select this entry's images in the grid (jump home)",
    keys: [], // pointer-only: the row affordance + the journal-row seat
    scope: "inspector",
    group: "panels",
    seats: ["journal-row"],
    available: (ctx) => ctx.inspectorOpen === "journal",
    // The seat arg is the entry's ordered target list (event_targets
    // .position — CAPTURE §3); redacted stubs carry none and decline.
    toAction: (_ctx, arg) => {
      const targets = Array.isArray(arg) ? (arg as string[]) : [];
      return targets.length > 0
        ? { kind: "select-journal-targets", targets }
        : null;
    },
  },
];
