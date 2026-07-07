/**
 * Download progress detail (audit D5): turn the runtime-status stream's
 * cumulative byte counts into the settings row's "1.2 / 3.9 GB · 8 MB/s"
 * line. Pure module so the smoothing and formatting are unit-testable;
 * SettingsApp feeds it one sample per runtime-status event.
 *
 * WHY frontend-side: the backend already emits coalesced cumulative bytes
 * on the existing `runtime-status` channel; a rate needs only successive
 * samples of that stream, so computing it here avoids growing the DTO and
 * keeps the event plumbing untouched.
 */

/** Binary units, matching the backend's byte sums and the cache readout. */
const GB = 1024 * 1024 * 1024;
const MB = 1024 * 1024;
const KB = 1024;

/** Last sample plus the smoothed throughput derived from the stream so
 * far. `bytesPerSec` is null until two comparable samples exist. */
export interface RateState {
  /** Sample timestamp, ms (Date.now()). */
  t: number;
  /** Cumulative downloaded bytes at `t`. */
  bytes: number;
  /** Exponentially smoothed throughput, or null when unknowable. */
  bytesPerSec: number | null;
}

/**
 * EWMA weight for the newest interval. Status events arrive irregularly
 * (coalesced bus bursts), so a plain last-interval rate flickers between
 * extremes; 0.3 settles within a few events while still tracking a real
 * throughput change inside ~10 s.
 */
const SMOOTHING = 0.3;

/**
 * Fold one (time, cumulativeBytes) sample into the rate state. A rewound
 * counter (bytes going DOWN happens when a checksum retry discards a part
 * file) resets the estimate rather than emitting a nonsense negative
 * rate. A non-advancing clock (a command return and a pump event landing
 * the same millisecond) keeps the previous estimate instead of dividing
 * by zero. Equal byte counts are honest zero intervals (a stall) and
 * decay the rate toward zero.
 */
export function updateRate(prev: RateState | null, t: number, bytes: number): RateState {
  if (prev === null || bytes < prev.bytes) {
    return { t, bytes, bytesPerSec: null };
  }
  if (t <= prev.t) {
    return prev;
  }
  const instant = ((bytes - prev.bytes) / (t - prev.t)) * 1000;
  const smoothed =
    prev.bytesPerSec === null
      ? instant
      : prev.bytesPerSec + SMOOTHING * (instant - prev.bytesPerSec);
  return { t, bytes, bytesPerSec: smoothed };
}

/**
 * "1.2 / 3.9 GB" — both numbers share the TOTAL's unit so the pair reads
 * as one fraction (mixed units like "800 MB / 3.9 GB" make the eye do the
 * conversion). GB gets one decimal; MB is whole.
 */
export function formatSizePair(downloaded: number, total: number): string {
  const gb = total >= GB;
  const fmt = (b: number) => (gb ? (b / GB).toFixed(1) : Math.round(b / MB).toString());
  return `${fmt(downloaded)} / ${fmt(total)} ${gb ? "GB" : "MB"}`;
}

/** "8 MB/s" style throughput; KB/s below a MB/s so a throttled or dying
 * transfer still shows a live number instead of "0 MB/s". */
export function formatRate(bytesPerSec: number): string {
  if (bytesPerSec >= MB) return `${(bytesPerSec / MB).toFixed(1)} MB/s`;
  return `${Math.max(1, Math.round(bytesPerSec / KB))} KB/s`;
}

/**
 * The full detail line for a downloading row: sizes always, throughput
 * only once it is known and nonzero. Middot separator, never an em-dash
 * (user-visible copy rule).
 */
export function progressDetail(
  downloaded: number,
  total: number,
  bytesPerSec: number | null,
): string {
  const sizes = formatSizePair(downloaded, total);
  if (bytesPerSec === null || bytesPerSec <= 0) return sizes;
  return `${sizes} · ${formatRate(bytesPerSec)}`;
}
