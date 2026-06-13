/**
 * Ranking-signal toggles (search-as-scope Phase 3): the pure half of the ⚙
 * "Ranking signals" popover that makes the B75 fusion weights USER-VISIBLE.
 *
 * Founder decision D2: ship ON/OFF checkboxes first (continuous weight sliders
 * and the similarity-tilt control are eval-gated / Phase 4). So each signal is
 * a single boolean: checked = its B75 default weight, unchecked = 0.0 (the
 * signal is excluded from the fusion). This module owns the signal catalogue,
 * the default state, and the on/off -> FusionWeights mapping — kept PURE (no
 * runes, no IPC) so the mapping is unit-testable and the store holds only the
 * boolean flags.
 *
 * SEMANTIC-LANE ONLY by construction: S1/S3/S4 do not exist on the lexical
 * keystroke path, so these weights are sent only with a committed semantic
 * search and can never tax the <100 ms as-you-type budget.
 */

/** The four fusion signals, in popover (and FusionWeights field) order. The
 * `label` is the user-visible copy; the `key` is the weights-payload field. */
export type SignalKey = "s1" | "s2" | "s3_each" | "s4";

export interface SignalDef {
  key: SignalKey;
  /** "S1" .. "S4" — the short code shown beside the checkbox. */
  code: string;
  /** User-visible name (no em-dashes; the check:emdash gate). */
  label: string;
  /** The B75 default weight (hybrid.rs FusionWeights::default). Applied when
   * the signal is checked; unchecked maps to 0.0. */
  defaultWeight: number;
}

/** The catalogue. `defaultWeight` MUST mirror the backend
 * `FusionWeights::default()` (s1 1.0 / s2 1.0 / s3_each 0.5 / s4 1.0, post-B75)
 * — a checked signal sends exactly that, so the default-all-checked payload is
 * byte-identical to the backend's own default fusion. */
export const SIGNALS: readonly SignalDef[] = [
  { key: "s1", code: "S1", label: "Your words (notes)", defaultWeight: 1.0 },
  { key: "s2", code: "S2", label: "Note keywords", defaultWeight: 1.0 },
  { key: "s3_each", code: "S3", label: "Derived descriptions", defaultWeight: 0.5 },
  { key: "s4", code: "S4", label: "Visual match (CLIP)", defaultWeight: 1.0 },
] as const;

/** The on/off state: one boolean per signal. The store holds exactly this. */
export type SignalToggles = Record<SignalKey, boolean>;

/** The fusion-weights payload the `search` command's `weights` arg carries. */
export interface FusionWeightsPayload {
  s1: number;
  s2: number;
  s3_each: number;
  s4: number;
}

/** All signals on — the B75 defaults, the quiet zero-config default. */
export function defaultToggles(): SignalToggles {
  return { s1: true, s2: true, s3_each: true, s4: true };
}

/** True when every signal is checked — i.e. the toggles are at their B75
 * default. The semantic search OMITS the weights payload in this case so the
 * backend takes its own `HybridOptions::default()` (today's exact behavior);
 * only a deviation from all-on ships an explicit payload. */
export function isDefault(t: SignalToggles): boolean {
  return SIGNALS.every((s) => t[s.key]);
}

/**
 * Map the on/off toggles to the FusionWeights payload: a CHECKED signal sends
 * its B75 default weight, an UNCHECKED signal sends 0.0 (excluded from the
 * fusion). PURE and total — the one place the on/off -> weight rule lives.
 */
export function togglesToWeights(t: SignalToggles): FusionWeightsPayload {
  const w = (s: SignalDef): number => (t[s.key] ? s.defaultWeight : 0.0);
  const by = (key: SignalKey): number => w(SIGNALS.find((s) => s.key === key)!);
  return { s1: by("s1"), s2: by("s2"), s3_each: by("s3_each"), s4: by("s4") };
}

/**
 * Compact signal-provenance hint for a result cell (the "show, don't just
 * tune" rule): which signals contributed to THIS image, as their short codes.
 * Input is the `DebugScores.per_signal` triples [signalId, rank, score]; the
 * backend formats the signal as its Rust enum name (e.g. "S1AnnotationChunk",
 * "S4ImageClip"), so we key off the leading "S1".."S4" and de-duplicate
 * (S3 has two sub-lists). PURE — fed by the store's resultDebug map.
 */
export function signalHint(perSignal: readonly [string, number | null, number][]): string {
  const seen = new Set<string>();
  for (const [sig] of perSignal) {
    const m = /^S[1-4]/.exec(sig);
    if (m !== null) seen.add(m[0]);
  }
  // Emit in S1..S4 order, never the arrival order, so the hint reads stably.
  return ["S1", "S2", "S3", "S4"].filter((c) => seen.has(c)).join(" ");
}
