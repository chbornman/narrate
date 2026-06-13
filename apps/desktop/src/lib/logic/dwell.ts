/**
 * Dwell capture — the attention/engagement heatmap's only genuinely new
 * capture (DESIGN-ATTENTION-HEATMAP.md). NOT a gaze tracker: it measures where
 * INTENT lands (what you opened / selected), capped and local. This module is
 * the PURE tracker — a small state machine over focus episodes — so the flush
 * rules (tier, blur-pause, idle, debounce) are testable without a webview. The
 * thin app.svelte.ts hook drives it from the real cross-slice flows.
 *
 * A "focus episode" = a continuous stretch where one or more images are the
 * user's focus, at a TIER:
 *   - "look": the single-image view (full weight, the strongest "I am focusing
 *     on THIS").
 *   - "grid": a grid selection / multi-select (far less; the reduced accrual is
 *     attributed to EACH selected image so a marquee crowns no single one).
 *
 * The episode ends (and flushes) on leaving Look / deselect / switch / window
 * blur (visibilitychange/blur) / a short idle. The tier RATE and the 60 s cap
 * are applied in the BACKEND (tuning is authoritative); we report only the raw
 * episode (hash, source, elapsed_ms). Elapsed is wall-clock between start and
 * flush; the backend cap + the blur-pause handle a walk-away.
 */
import type { DwellSource } from "../ipc/commands";

/** One in-flight focus episode: a tier, the focused hashes, and when it began.
 * `nowMs` is injected (Date.now() at the call site) so the tracker is pure and
 * the tests drive the clock. */
export interface DwellEpisode {
  source: DwellSource;
  hashes: readonly string[];
  startMs: number;
}

/** A finished episode ready to report: one (hash, source, elapsedMs) per
 * focused image. A multi-select episode fans out to every selected hash with
 * the SAME elapsed — the reduced grid accrual lands on each (DESIGN). */
export interface DwellFlush {
  hash: string;
  source: DwellSource;
  elapsedMs: number;
}

/** Minimum episode length worth reporting, ms. A sub-threshold flick (a stray
 * click that is instantly replaced, a momentary blur) carries no real
 * attention and would just be IPC noise; below this an episode flushes to
 * nothing. */
export const MIN_EPISODE_MS = 250;

/** Idle horizon, ms: with no input for this long the current episode is
 * considered ended (the user walked away from the keyboard) and flushes. The
 * app hook arms a timer for this; the backend's 60 s cap is the harder guard. */
export const IDLE_FLUSH_MS = 30_000;

/** Begin a focus episode. Returns the episode to hold; null when there is
 * nothing to track (no hashes), so callers can store the result directly. */
export function beginEpisode(
  source: DwellSource,
  hashes: readonly string[],
  nowMs: number,
): DwellEpisode | null {
  if (hashes.length === 0) return null;
  return { source, hashes: [...hashes], startMs: nowMs };
}

/** End an episode and produce its reports. Returns one flush per focused hash
 * (the multi-select fan-out), or an empty array when the episode was too short
 * to count (below MIN_EPISODE_MS) or the clock ran backwards. The caller clears
 * its held episode regardless — a flushed episode is done. */
export function endEpisode(
  episode: DwellEpisode | null,
  nowMs: number,
): DwellFlush[] {
  if (episode === null) return [];
  const elapsedMs = nowMs - episode.startMs;
  if (elapsedMs < MIN_EPISODE_MS) return [];
  return episode.hashes.map((hash) => ({
    hash,
    source: episode.source,
    elapsedMs,
  }));
}

/** Whether two focus targets describe the SAME episode: same tier and the same
 * set of hashes (order-independent). The app hook uses this to avoid churning
 * an episode on a re-report that did not actually change the focus (e.g. a
 * scope re-emit) — only a real change ends the old episode and begins a new
 * one, so a steady Look-open accrues one continuous span. */
export function sameFocus(
  a: { source: DwellSource; hashes: readonly string[] } | null,
  b: { source: DwellSource; hashes: readonly string[] } | null,
): boolean {
  if (a === null || b === null) return a === b;
  if (a.source !== b.source) return false;
  if (a.hashes.length !== b.hashes.length) return false;
  const set = new Set(a.hashes);
  return b.hashes.every((h) => set.has(h));
}
