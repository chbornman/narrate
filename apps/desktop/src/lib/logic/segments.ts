/**
 * The segmented indicator strip (featureset §4, M2b-proofed): pure
 * (SegmentInput, MODES) → Segment[]. Fixed ordering reserves every future
 * seat by construction:
 *
 *   ingest hairline · scope (THE pulse target) · n-of-m (Look) ·
 *   mode segments (auto-advance now; pencil/mic ids reserved) ·
 *   query-residue seat (M3 — never emitted in P4.2)
 *
 * The indicator renders this output verbatim and is EXEMPT from
 * Tab lights-out (coordinator ruling: the indicator is capture-state
 * truth; future mic evidence lives there — it never hides).
 */
import type { ActionContext, ModeDef, SegmentTone } from "../actions/types";
import { MODES } from "../actions/modes";
import { scopeLabel } from "./scope";

export interface SegmentInput {
  ingest: { running: boolean; done: number; total: number };
  scope: { kind: string; count: number };
  /** 0-based position within the Look navigation set; null outside Look. */
  lookPosition: { index: number; total: number } | null;
  /** CAPTURE §5.4/§11: an in-flight utterance's BOUND scope. `differs` =
   * bound ≠ current selection (the caller compares full target lists);
   * only then does the tether render — "selection is now B, but what
   * you're saying lands on A". Optional: absent before M2b wiring. */
  streaming?: { kind: string; count: number; differs: boolean } | null;
  ctx: ActionContext;
}

export interface Segment {
  id:
    | "ingest"
    | "scope"
    | "position"
    | `mode:${ModeDef["id"]}`
    | "query-residue";
  text: string;
  title: string;
  /** The scope segment is the pulse target (UI §7.4). */
  pulse?: boolean;
  /** Ingest hairline fill, 0..1. */
  fraction?: number;
  /** Quiet rendering hint (UI §7.3): dimmed / breathing. */
  tone?: SegmentTone;
}

export function segments(
  input: SegmentInput,
  modes: readonly ModeDef[] = MODES,
): Segment[] {
  const out: Segment[] = [];

  if (input.ingest.running) {
    const fraction =
      input.ingest.total > 0
        ? Math.min(1, input.ingest.done / input.ingest.total)
        : 0;
    out.push({
      id: "ingest",
      text: "",
      title: `Indexing — ${input.ingest.done.toLocaleString()} of ${input.ingest.total.toLocaleString()}`,
      fraction,
    });
  }

  // Scope: always present; `● 0` never renders — no selection is session
  // scope (UI §7.2). While an utterance bound to a DIFFERENT scope is
  // still streaming, the segment shows that bound scope with the tether
  // (UI §7.3 `● 1 ⇠ 🎙`) and reverts at finalization; the indicator never
  // re-binds anything itself (§5.4 — the distinction is mandatory).
  const streaming = input.streaming ?? null;
  if (streaming !== null && streaming.differs) {
    out.push({
      id: "scope",
      text: `● ${scopeLabel(streaming.kind, streaming.count)} ⇠`,
      title: `Words in flight land on the earlier selection — the live selection is ● ${scopeLabel(input.scope.kind, input.scope.count)}`,
      pulse: true,
    });
  } else {
    out.push({
      id: "scope",
      text: `● ${scopeLabel(input.scope.kind, input.scope.count)}`,
      title:
        input.scope.kind === "session"
          ? "Words land on this session"
          : `Speaking about ${input.scope.count}`,
      pulse: true,
    });
  }

  if (input.lookPosition !== null && input.lookPosition.total > 0) {
    out.push({
      id: "position",
      text: `${input.lookPosition.index + 1} of ${input.lookPosition.total}`,
      title: "Position in the navigation set",
    });
  }

  // Modes, in MODES order — the mic seat is reserved by this ordering
  // (recording state will live here, never in a toast — featureset §4).
  for (const mode of modes) {
    const seg = mode.segment(input.ctx);
    if (seg !== null) out.push({ id: `mode:${mode.id}`, ...seg });
  }

  // M3 reserved seat: active-query residue with one-key clear renders here
  // (id "query-residue"); never emitted in P4.2.
  return out;
}
