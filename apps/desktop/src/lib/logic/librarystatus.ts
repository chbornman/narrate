/**
 * The Library-status model (BACKLOG "digest visibility", founder June 2026):
 * ONE pure function over the SAME evented state the shell already holds —
 * `IngestStatus` (now with per-pass done/total/ratePerSec) plus the runtime
 * snapshot for downloads/degraded — into the header-center indicator's view.
 *
 * Why a header indicator (and not the bottom-right Station): the digest is
 * a LIBRARY-WIDE truth ("is my catalog still being built?"), not a capture
 * state. It belongs in the title bar where the eye rests, leaving the Station
 * to be the capture organ alone (mic/search/pencil/scope).
 *
 * Everything here is PURE so the stage mapping, the settled/working split,
 * the waiting-on assembly, the ETA derivation, and the human formatters are
 * all unit-testable with no DOM. LibraryStatus.svelte renders it verbatim.
 */
import type { EmbedderSlot, IngestStatus, RuntimeStatus } from "../types/dto";

// ---------------------------------------------------------------------------
// Canonical stages (founder order): discover -> hash -> meta -> preview ->
// embed. The backend emits queue-spelled pass names; we map each onto its
// canonical stage. A pass we cannot place lands in a TRAILING generic stage
// (never dropped — a future pass surfaces itself without touching this file).
// ---------------------------------------------------------------------------

/** Canonical stage ids, in the order they run. `discover` has no pass name —
 * it is driven by the walk (scanning/discovered on IngestStatus); the rest
 * map from pass names. `other` is the trailing catch-all for unmapped passes. */
export type StageId = "discover" | "hash" | "meta" | "preview" | "embed" | "other";

/** The canonical stage order the header lists top-to-bottom. */
const STAGE_ORDER: StageId[] = ["discover", "hash", "meta", "preview", "embed"];

/** Pass name -> canonical stage. The names are the backend's queue spellings
 * (ingest_passes.pass_name): hash · exif · preview · image-embedding ·
 * text-embedding (plus full-raw-decode / caption / future passes, which fall
 * through to the trailing `other` stage by construction). */
const PASS_STAGE: Record<string, StageId> = {
  hash: "hash",
  exif: "meta",
  preview: "preview",
  "image-embedding": "embed",
  "text-embedding": "embed",
};

/** Human label per stage (the indicator's row + headline copy). No
 * em-dashes anywhere (gate: check:emdash). */
const STAGE_LABEL: Record<StageId, string> = {
  discover: "Discovering",
  hash: "Hashing",
  meta: "Reading metadata",
  preview: "Building previews",
  embed: "Embedding for search",
  other: "Finishing up",
};

export type StageState = "done" | "working" | "pending";

export interface LibraryStage {
  id: StageId;
  label: string;
  done: number;
  total: number;
  /** 0..1; 0 when total is unknown (e.g. discovery still sizing). */
  fraction: number;
  state: StageState;
  /** Smoothed items/sec for this stage; 0 when unknown or paused. */
  ratePerSec: number;
  /** remaining / ratePerSec when the rate is positive; null otherwise (rate
   * 0 = unknown or paused, so an ETA would be a guess — we show none). */
  etaSecs: number | null;
}

/** One reason the library is held up (offline drive, a downloading model, a
 * loading-or-failed embedder). Rendered in the "Waiting on" section,
 * top-most first. */
export interface WaitingReason {
  /** Stable key for the {#each} so a reason can fade in/out cleanly. */
  id: string;
  text: string;
  /** True for a DEGRADED reason (a failed embedder lane): a real fault, not a
   * transient wait. The component styles these as the error register and the
   * top one flips the collapsed pill to amber-blocked. Absent/false = an
   * ordinary transient wait (offline/downloading/building). */
  degraded?: boolean;
  /** Full, untruncated detail (a failed lane's error) for a `title` attr; the
   * `text` itself is clamped short for the pill/row. Absent when there is no
   * extra detail beyond `text`. */
  detail?: string;
}

export interface LibraryStatusModel {
  /** Nothing running, scanning, or downloading — the calm state. */
  settled: boolean;
  /** "Library settled" vs "Library is working" — the collapsed headline. */
  headline: string;
  /** The canonical stages, in order; trailing `other` appended when present. */
  stages: LibraryStage[];
  /** The single stage the collapsed pill foregrounds (the first one still
   * working, else the first pending) — null when settled. */
  current: LibraryStage | null;
  /** Blocking reasons (offline / downloading / loading / failed embedder),
   * top-most first. A failed embedder lane is `degraded: true`. */
  waitingOn: WaitingReason[];
  /** True when any waiting-on reason is degraded (a failed embedder lane):
   * the library is in a real DEGRADED state worth the amber register, not
   * merely waiting. The collapsed pill folds this into its blocked register. */
  degraded: boolean;
  /** Total ingest error count (the expanded panel's errors row). */
  errors: number;
  /** Overall ETA across working+pending stages; null when nothing is sized
   * or everything is paused. */
  etaSecs: number | null;
}

export interface LibraryStatusInput {
  ingest: IngestStatus;
  /** Latest RUNTIME snapshot; null = backend dark (tests/dev) -> no downloads
   * and no degraded-embedder signal. */
  runtime: RuntimeStatus | null;
}

// ---------------------------------------------------------------------------
// Human formatters (pure) — short, glanceable copy. No em-dashes.
// ---------------------------------------------------------------------------

/** "~6m" / "~30s" / "~2h" from a seconds estimate; null in -> "" (caller
 * decides whether to show it at all). Rounds to a friendly unit so a noisy
 * rate does not jitter the digit every tick. */
export function formatEta(secs: number | null): string {
  if (secs === null || !Number.isFinite(secs) || secs <= 0) return "";
  if (secs < 60) return `~${Math.max(1, Math.round(secs))}s`;
  if (secs < 3600) return `~${Math.round(secs / 60)}m`;
  return `~${Math.round(secs / 3600)}h`;
}

/** "12/s" / "1.2k/s" from an items-per-second rate; "" when 0/unknown. The
 * rate is the honest throughput readout next to a stage's ETA. */
export function formatRate(ratePerSec: number): string {
  if (!Number.isFinite(ratePerSec) || ratePerSec <= 0) return "";
  if (ratePerSec >= 1000) {
    const k = ratePerSec / 1000;
    // one decimal under 10k so "1.2k/s" reads; whole thousands above
    return k >= 10 ? `${Math.round(k)}k/s` : `${(Math.round(k * 10) / 10).toFixed(1)}k/s`;
  }
  if (ratePerSec >= 10) return `${Math.round(ratePerSec)}/s`;
  // sub-10 keeps one decimal so a slow stage still shows motion ("0.4/s")
  return `${(Math.round(ratePerSec * 10) / 10).toFixed(1)}/s`;
}

/** "240 / 5,000" locale-grouped progress copy for a stage row. */
export function formatCount(done: number, total: number): string {
  return `${done.toLocaleString()} / ${total.toLocaleString()}`;
}

// ---------------------------------------------------------------------------
// Stage assembly
// ---------------------------------------------------------------------------

/** ETA for one stage: remaining / rate when the rate is positive; null when
 * the rate is 0 (unknown or paused) so we never fabricate a countdown. */
function stageEta(remaining: number, ratePerSec: number): number | null {
  return ratePerSec > 0 ? remaining / ratePerSec : null;
}

/** Build the discover stage from the walk fields. Discovery has no pass row
 * (counters only appear at hash time), so its "progress" is the live walk:
 * working while `scanning`, with `discovered` as its done count (total is
 * unknown mid-walk, so fraction stays 0). Settled walks produce no discover
 * stage at all (returned null) — there is nothing to discover. */
function discoverStage(ing: IngestStatus): LibraryStage | null {
  if (!ing.scanning) return null;
  return {
    id: "discover",
    label: STAGE_LABEL.discover,
    done: ing.discovered,
    total: 0, // a walk in flight has no known total until it finishes
    fraction: 0,
    state: "working",
    ratePerSec: 0, // the backend does not rate the walk; no ETA for it
    etaSecs: null,
  };
}

/** Roll the per-pass rows that map to ONE canonical stage into a single
 * stage: summed done/total/remaining, the MAX rate across its passes (the
 * embed stage is image+text passes draining together — the faster one sets
 * the felt pace), and a state derived from whether any unit is left. */
function rollPasses(
  id: StageId,
  passes: IngestStatus["passes"],
): LibraryStage | null {
  if (passes.length === 0) return null;
  let done = 0;
  let total = 0;
  let remaining = 0;
  let rate = 0;
  for (const p of passes) {
    done += p.done;
    total += p.total;
    remaining += p.remaining;
    // MAX, not sum: parallel passes in one stage share a felt throughput;
    // summing would overstate the rate and undercut the ETA.
    if (p.ratePerSec > rate) rate = p.ratePerSec;
  }
  // A stage with no remaining work is done; with work AND a positive rate it
  // is actively working; with work but a frozen rate it is pending (queued or
  // paused) — the ETA stays null so a paused stage shows no countdown.
  const state: StageState =
    remaining <= 0 ? "done" : rate > 0 ? "working" : "pending";
  const fraction = total > 0 ? Math.min(1, done / total) : 0;
  return {
    id,
    label: STAGE_LABEL[id],
    done,
    total,
    fraction,
    state,
    ratePerSec: rate,
    etaSecs: stageEta(remaining, rate),
  };
}

/** Max chars of a failed lane's error to inline into the pill/row text. The
 * full error rides a `title` attr (WaitingReason.detail); this keeps the
 * visible copy glanceable instead of wrapping a stack trace into the header. */
const EMBEDDER_ERROR_CLAMP = 60;

/** Clamp an embedder error for the pill/row: first line only, trimmed, capped
 * at ~60 chars with an ASCII ellipsis (no em-dash). Empty/blank -> "". */
function clampEmbedderError(error: string | null): string {
  if (error === null) return "";
  const firstLine = error.split("\n")[0].trim();
  if (firstLine === "") return "";
  if (firstLine.length <= EMBEDDER_ERROR_CLAMP) return firstLine;
  return `${firstLine.slice(0, EMBEDDER_ERROR_CLAMP - 3).trimEnd()}...`;
}

/** The HONEST per-role embedder signal (replaces the interim CLIP-only
 * `embedderLoading` boolean): turn one lane's slot state into a waiting-on
 * reason, or null when the lane needs no row.
 *   · building -> a transient "loading" row (e.g. "image embedder loading").
 *   · failed   -> a DEGRADED row ("image search unavailable") with the clamped
 *     error inlined and the full error on `detail`; this lane is broken, not
 *     warming up.
 *   · ready/idle -> null. idle means the lane is simply not configured/active
 *     (the M1 text lane sits idle when unlit) — we do NOT nag, which is exactly
 *     what the old CLIP-only hack worked around the wrong way. */
function embedderReason(
  role: "clip" | "text",
  slot: EmbedderSlot,
): WaitingReason | null {
  const loadingText =
    role === "clip" ? "image embedder loading" : "text embedder loading";
  const failedText =
    role === "clip" ? "image search unavailable" : "text search unavailable";
  if (slot.state === "building") {
    return { id: `embedder-loading:${role}`, text: loadingText };
  }
  if (slot.state === "failed") {
    const short = clampEmbedderError(slot.error);
    // Inline the clamped error after the headline when present; the full,
    // untruncated error rides `detail` for the component's title attr.
    const text = short === "" ? failedText : `${failedText}: ${short}`;
    const reason: WaitingReason = {
      id: `embedder-failed:${role}`,
      text,
      degraded: true,
    };
    if (slot.error !== null && slot.error.trim() !== "") {
      reason.detail = `${failedText}: ${slot.error.trim()}`;
    }
    return reason;
  }
  // ready / idle -> no row.
  return null;
}

export function libraryStatusModel(
  input: LibraryStatusInput,
): LibraryStatusModel {
  const ing = input.ingest;

  // Bucket the passes by canonical stage; unmapped names collect under
  // `other` so they surface as a trailing stage rather than vanishing.
  const buckets = new Map<StageId, IngestStatus["passes"]>();
  for (const p of ing.passes) {
    const id = PASS_STAGE[p.name] ?? "other";
    const arr = buckets.get(id);
    if (arr === undefined) buckets.set(id, [p]);
    else arr.push(p);
  }

  const stages: LibraryStage[] = [];

  // discover first (walk-driven), then the canonical pass stages in order.
  const discover = discoverStage(ing);
  if (discover !== null) stages.push(discover);
  for (const id of STAGE_ORDER) {
    if (id === "discover") continue; // already handled above
    const rolled = rollPasses(id, buckets.get(id) ?? []);
    if (rolled !== null) stages.push(rolled);
  }
  // Trailing generic stage for any unmapped passes (full-raw-decode, caption,
  // future names) — appended LAST so the canonical order is never disturbed.
  const other = rollPasses("other", buckets.get("other") ?? []);
  if (other !== null) stages.push(other);

  // Waiting-on assembly, top-most first: offline drives (the hard stop),
  // then downloading models, then the per-role embedder rows (failed before
  // building).
  const waitingOn: WaitingReason[] = [];
  for (const v of ing.offlineVolumes) {
    waitingOn.push({
      id: `offline:${v.label}`,
      text: `Paused - ${v.label} offline (${v.images.toLocaleString()} ${v.images === 1 ? "photo" : "photos"})`,
    });
  }
  let downloading = false;
  for (const m of input.runtime?.models ?? []) {
    if (m.state !== "downloading") continue;
    downloading = true;
    const pct =
      m.totalBytes > 0
        ? Math.floor((m.downloadedBytes / m.totalBytes) * 100)
        : 0;
    waitingOn.push({ id: `download:${m.id}`, text: `${m.id} downloading (${pct}%)` });
  }
  // Per-role embedder rows, FAILED before BUILDING (a degraded lane is more
  // urgent than one still warming up). Each role contributes at most one row;
  // a null runtime (backend dark) contributes none.
  if (input.runtime !== null) {
    const rt = input.runtime;
    const embedderReasons = [
      embedderReason("clip", rt.clip),
      embedderReason("text", rt.textEmbedder),
    ].filter((r): r is WaitingReason => r !== null);
    // failed (degraded) first, then building, stable within each group.
    const failed = embedderReasons.filter((r) => r.degraded === true);
    const building = embedderReasons.filter((r) => r.degraded !== true);
    waitingOn.push(...failed, ...building);
  }

  // Degraded = any waiting-on reason is a failed embedder lane (a real fault,
  // not a transient wait): the collapsed pill folds this into the amber
  // blocked register and the panel styles it as an error row.
  const degraded = waitingOn.some((w) => w.degraded === true);

  // Settled = nothing running, nothing scanning, nothing downloading, no
  // un-done stage, AND nothing blocking. A blocking reason (an offline drive
  // holding library photos, a model still downloading, the embedder loading)
  // means work is PAUSED or DEGRADED, not absent — the headline reads
  // "working" while a drive is out or an embedder lane is loading/failed,
  // even when no pass row is queued yet.
  const anyWork = stages.some((s) => s.state !== "done");
  const settled =
    !ing.running &&
    !ing.scanning &&
    !downloading &&
    !anyWork &&
    waitingOn.length === 0;

  // Current stage the collapsed pill foregrounds: the first still working,
  // else the first pending (queued/paused), else null.
  const current =
    stages.find((s) => s.state === "working") ??
    stages.find((s) => s.state === "pending") ??
    null;

  // Overall ETA = the SUM of every working+pending stage's ETA (the stages
  // run in series, so the wait is additive). null when nothing is sized /
  // everything is paused (no positive-rate stage contributes).
  let overall: number | null = null;
  for (const s of stages) {
    if (s.state === "done") continue;
    if (s.etaSecs === null) continue;
    overall = (overall ?? 0) + s.etaSecs;
  }

  return {
    settled,
    headline: settled ? "Library settled" : "Library is working",
    stages,
    current,
    waitingOn,
    degraded,
    errors: ing.errors,
    etaSecs: overall,
  };
}
