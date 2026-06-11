/**
 * The one-time model-consent card (UI §9.1.3 / RUNTIME §10.2–10.3), pure
 * logic: shown ONCE, after the first root is added, as a quiet panel —
 * never a modal gate. One decision: Download now / Later / Never (Never
 * remembered; Later re-offers from settings only). The disk budget is the
 * LIVE manifest sum (§5.4); gated models list their licenses with
 * required acceptances (§5.3). Skipping changes nothing about journaling.
 */
import type { ModelRowDto, RuntimeStatus } from "../types/dto";

export interface ConsentCardModel {
  tierDetected: number;
  tierEffective: number;
  /** Σ total_bytes of the offered set — the §5.4 live sum. */
  totalBytes: number;
  /** Offered models with their license display + acceptance state. */
  models: ModelRowDto[];
  /** "Download now" enables only once every acceptance_required offer
   * carries its recorded acceptance (§13.7: zero bytes move before). */
  downloadReady: boolean;
}

export function consentCard(
  status: RuntimeStatus | null,
  hasRoots: boolean,
): ConsentCardModel | null {
  if (status === null) return null;
  // §9.1.3: after the first root is added — never before.
  if (!hasRoots) return null;
  // One-time: any prior decision (download/later/never) retires the card;
  // Later re-offers from Settings → Models only (§10.3).
  if (status.consent !== "undecided") return null;
  // Tier 0 is whole: nothing is offered below the floor, so no card,
  // no download prompt, ever (§1.4/§13.3).
  if (status.tierEffective < 1) return null;
  const offered = status.models.filter((m) => m.state !== "not-offered");
  if (offered.length === 0) return null;
  return {
    tierDetected: status.tierDetected,
    tierEffective: status.tierEffective,
    totalBytes: status.consentOfferBytes,
    models: offered,
    downloadReady: offered.every((m) => !m.acceptanceRequired || m.accepted),
  };
}

/** "~9.5 GB" — the consent copy's number, from the live sum. */
export function formatGb(bytes: number): string {
  const gb = bytes / 1_000_000_000;
  if (gb >= 10) return `~${Math.round(gb)} GB`;
  return `~${(Math.round(gb * 10) / 10).toFixed(1)} GB`;
}
