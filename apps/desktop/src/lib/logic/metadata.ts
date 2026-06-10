/**
 * Metadata tab rows (featureset §3, K16 stands: read-only display, no
 * editing, ever) — pure ImageMetadataDto → labeled rows so
 * MetadataTab.svelte stays a thin rendering. The row set is fixed (stable
 * layout); unknown values render as a dimmed "—". Copyable rows (hash,
 * path — K16's one concession to utility) carry a flag, never a behavior.
 */
import type { ImageMetadataDto } from "../types/dto";
import { formatSessionDate, formatTime } from "./journal";

export interface MetadataRow {
  label: string;
  value: string;
  /** Renders a quiet copy affordance (hash, path). */
  copyable?: boolean;
  /** Unknown/placeholder value — renders dimmed. */
  dim?: boolean;
}

const UNKNOWN = "—";

const row = (label: string, value: string | null, copyable = false): MetadataRow =>
  value === null
    ? { label, value: UNKNOWN, dim: true }
    : copyable
      ? { label, value, copyable: true }
      : { label, value };

/** "24.3 MB" — binary-ish display sizes, one decimal from MB up. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/** EXIF orientation values 1–8 (anything else displays raw). */
export function orientationLabel(orientation: number): string {
  switch (orientation) {
    case 1: return "Normal";
    case 2: return "Mirrored";
    case 3: return "Rotated 180°";
    case 4: return "Mirrored, rotated 180°";
    case 5: return "Mirrored, rotated 90° CW";
    case 6: return "Rotated 90° CW";
    case 7: return "Mirrored, rotated 90° CCW";
    case 8: return "Rotated 90° CCW";
    default: return String(orientation);
  }
}

/** "Embedded preview — full decode pending" / "Display preview ready". */
export function previewLine(source: string | null, pending: boolean): string {
  const name =
    source === "embedded"
      ? "Embedded preview"
      : source === "full-decode"
        ? "Full-decode preview"
        : source === "original"
          ? "Preview from original"
          : "No preview yet";
  return pending ? `${name} — full decode pending` : name;
}

function formatDateTime(ts: string): string {
  return `${formatSessionDate(ts)} ${formatTime(ts)}`;
}

function camera(m: ImageMetadataDto): string | null {
  const parts = [m.cameraMake, m.cameraModel].filter((p) => p !== null);
  return parts.length === 0 ? null : parts.join(" ");
}

/**
 * The complete, ordered row set (featureset §3: capture time, gear,
 * exposure, ISO, dims, orientation, GPS text, file name/path/size,
 * copyable hash, preview source + backfill state).
 */
export function metadataRows(m: ImageMetadataDto): MetadataRow[] {
  return [
    row("Captured", m.captureTs === null ? null : formatDateTime(m.captureTs)),
    row("Camera", camera(m)),
    row("Lens", m.lensModel),
    row("Focal length", m.focalLengthMm === null ? null : `${m.focalLengthMm} mm`),
    row("Exposure", m.exposureTime === null ? null : `${m.exposureTime} s`),
    row("Aperture", m.fNumber === null ? null : `f/${m.fNumber}`),
    row("ISO", m.iso === null ? null : String(m.iso)),
    row(
      "Dimensions",
      m.pixelWidth === null || m.pixelHeight === null
        ? null
        : `${m.pixelWidth} × ${m.pixelHeight}`,
    ),
    row("Orientation", orientationLabel(m.orientation)),
    row("Location", m.gps),
    row("File", m.fileName),
    // The best online absolute path; offline volumes leave only relPath.
    row("Path", m.absPath ?? m.relPath, true),
    row("Size", formatBytes(m.byteSize)),
    row("Format", m.format.toUpperCase()),
    row("Hash", m.hash, true),
    row("Preview", previewLine(m.previewSource, m.previewPending)),
    row("Added", formatDateTime(m.firstIngestedAt)),
  ];
}
