/**
 * The header jobs pill (BACKLOG "Header shows background jobs") — pure
 * IngestStatus → quiet copy. ONE register for "the library is still
 * digesting": initial ingest, preview rebuilds, doctor re-pends, and the
 * M3 embedding/caption backfills ALL flow through ingest_passes, so
 * surfacing the pass breakdown covers every background job by
 * construction. The visible pill is one dim word; count and kind live in
 * the hover title — never a progress-bar circus (founder, June 2026).
 */
import type { IngestStatus } from "../types/dto";

/** Queue-spelled pass names → reviewer words. Unknown names pass through
 * verbatim so a future pass surfaces itself without touching this file.
 * Exported: the station's digest rows (logic/station.ts) speak the same
 * words — one vocabulary for "still digesting", never two. */
export const KIND_LABELS: Record<string, string> = {
  hash: "hashing",
  exif: "reading metadata",
  preview: "building previews",
  "full-raw-decode": "decoding raws",
  "image-embedding": "embedding images",
  "text-embedding": "embedding notes",
  caption: "captioning",
};

/** Work units still queued across every pass kind; 0 = the pill hides. */
export function jobsRemaining(status: IngestStatus): number {
  return status.passes.reduce((n, p) => n + p.remaining, 0);
}

/** The one-line hover breakdown, e.g.
 * "Still digesting — hashing 12 · building previews 480". */
export function jobsTitle(status: IngestStatus): string {
  const parts = status.passes.map(
    (p) => `${KIND_LABELS[p.name] ?? p.name} ${p.remaining.toLocaleString()}`,
  );
  return `Still digesting - ${parts.join(" · ")}`;
}

/** The offline-volume warning's hover detail (founder: warn + pause). Names
 * each disconnected drive and how many photos it hides, e.g.
 * "Reconnect to ingest: Field SSD (412 photos), Archive (18 photos)". Pure so
 * the copy pins in a unit test. No em-dashes (gate: check:emdash). */
export function offlineWarningTitle(
  volumes: IngestStatus["offlineVolumes"],
): string {
  if (volumes.length === 0) return "";
  const parts = volumes.map(
    (v) =>
      `${v.label} (${v.images.toLocaleString()} ${v.images === 1 ? "photo" : "photos"})`,
  );
  return `Reconnect to ingest: ${parts.join(", ")}`;
}
