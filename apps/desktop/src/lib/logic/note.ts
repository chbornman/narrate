/**
 * Typed-note input state machine (spec/UI.md §6 + CAPTURE §4).
 *
 * The input is a transient: it exists only between summon and
 * submit/cancel. The scope it will write to is SNAPSHOTTED AT SUMMON TIME
 * (UI §6 acceptance: "the event targets the scope shown at summon time").
 * CAPTURE §4 binds typed notes at submit time — the two agree because any
 * scope change while the input is open CANCELS the input (selection changes
 * require pointer/keyboard interaction that dismisses the transient), so
 * summon-time scope ≡ submit-time scope. Drafts are discarded without
 * prompt (§6).
 */
import type { ScopeView } from "../types/dto";

export interface NoteState {
  open: boolean;
  /** The scope echoed by the core at summon time (placeholder + audit). */
  snapshot: ScopeView | null;
}

export const CLOSED: NoteState = { open: false, snapshot: null };

export function summon(scope: ScopeView): NoteState {
  return { open: true, snapshot: scope };
}

export function cancel(_s: NoteState): NoteState {
  return CLOSED;
}

/**
 * Any scope change while the input is open closes it (draft discarded, no
 * prompt) — this is what keeps summon-time and submit-time scope identical.
 */
export function onScopeChanged(s: NoteState, next: ScopeView): NoteState {
  if (!s.open || s.snapshot === null) return s;
  const same =
    s.snapshot.kind === next.kind &&
    s.snapshot.count === next.count &&
    s.snapshot.previewHashes.join(",") === next.previewHashes.join(",");
  return same ? s : CLOSED;
}

/** Submit closes immediately; the caller invokes `add_note` (UI §6: the
 * input vanishes immediately; the indicator pulses when the event commits).
 */
export function submit(s: NoteState): { state: NoteState; scope: ScopeView | null } {
  if (!s.open) return { state: s, scope: null };
  return { state: CLOSED, scope: s.snapshot };
}
