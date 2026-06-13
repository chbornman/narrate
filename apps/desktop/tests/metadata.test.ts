/**
 * Metadata tab rows (logic/metadata.ts — featureset §3, K16): the fixed
 * labeled row set over ImageMetadataDto, unknown values dimmed as "—",
 * copyable flags on hash/path only, and the value formatters (bytes,
 * orientation, preview/backfill state).
 */
import { describe, expect, it } from "vitest";
import {
  formatBytes,
  metadataRows,
  orientationLabel,
  previewLine,
} from "../src/lib/logic/metadata";
import type { ImageMetadataDto } from "../src/lib/types/dto";

const meta = (over: Partial<ImageMetadataDto> = {}): ImageMetadataDto => ({
  hash: "ab".repeat(32),
  fileName: "IMG_4471.jpg",
  relPath: "iceland/IMG_4471.jpg",
  absPath: "/mnt/vol/iceland/IMG_4471.jpg",
  byteSize: 25_480_000,
  format: "jpeg",
  pixelWidth: 6000,
  pixelHeight: 4000,
  orientation: 1,
  captureTs: new Date(2026, 3, 12, 9, 31).toISOString(),
  cameraMake: "Fujifilm",
  cameraModel: "X-T5",
  lensModel: "XF 35mm F1.4",
  focalLengthMm: 35,
  iso: 400,
  fNumber: 2.8,
  exposureTime: "1/250",
  gps: "64.14160° N, 21.92740° W",
  previewSource: "embedded",
  previewPending: false,
  firstIngestedAt: new Date(2026, 5, 1, 8, 0).toISOString(),
  ...over,
});

const byLabel = (m: ImageMetadataDto) =>
  new Map(metadataRows(m).map((r) => [r.label, r]));

describe("the row set (featureset §3 enumeration)", () => {
  it("renders every field labeled and formatted", () => {
    const rows = byLabel(meta());
    expect(rows.get("Captured")?.value).toBe("12 Apr 2026 09:31");
    expect(rows.get("Camera")?.value).toBe("Fujifilm X-T5");
    expect(rows.get("Lens")?.value).toBe("XF 35mm F1.4");
    expect(rows.get("Focal length")?.value).toBe("35 mm");
    expect(rows.get("Exposure")?.value).toBe("1/250 s");
    expect(rows.get("Aperture")?.value).toBe("f/2.8");
    expect(rows.get("ISO")?.value).toBe("400");
    expect(rows.get("Dimensions")?.value).toBe("6000 × 4000");
    expect(rows.get("Orientation")?.value).toBe("Normal");
    expect(rows.get("Location")?.value).toBe("64.14160° N, 21.92740° W");
    expect(rows.get("File")?.value).toBe("IMG_4471.jpg");
    expect(rows.get("Size")?.value).toBe("24.3 MB");
    expect(rows.get("Format")?.value).toBe("JPEG");
    expect(rows.get("Preview")?.value).toBe("Embedded preview");
  });

  it("row order is fixed (stable layout)", () => {
    expect(metadataRows(meta()).map((r) => r.label)).toEqual([
      "Captured", "Camera", "Lens", "Focal length", "Exposure", "Aperture",
      "ISO", "Dimensions", "Orientation", "Location", "File", "Path",
      "Size", "Format", "Hash", "Preview", "Added",
    ]);
  });

  it("hash and path are the ONLY copyable rows (K16)", () => {
    const rows = metadataRows(meta());
    expect(rows.filter((r) => r.copyable).map((r) => r.label)).toEqual([
      "Path",
      "Hash",
    ]);
  });

  it("unknown values render as a dimmed em-dash, never hidden", () => {
    const rows = byLabel(
      meta({
        captureTs: null,
        cameraMake: null,
        cameraModel: null,
        lensModel: null,
        focalLengthMm: null,
        iso: null,
        fNumber: null,
        exposureTime: null,
        pixelWidth: null,
        gps: null,
      }),
    );
    for (const label of [
      "Captured", "Camera", "Lens", "Focal length", "Exposure",
      "Aperture", "ISO", "Dimensions", "Location",
    ]) {
      expect(rows.get(label)?.value, label).toBe("-");
      expect(rows.get(label)?.dim, label).toBe(true);
    }
  });

  it("an offline image falls back to the volume-relative path", () => {
    const rows = byLabel(meta({ absPath: null }));
    expect(rows.get("Path")?.value).toBe("iceland/IMG_4471.jpg");
    expect(rows.get("Path")?.copyable).toBe(true);
  });
});

describe("formatters", () => {
  it("formatBytes scales B → KB → MB → GB", () => {
    expect(formatBytes(820)).toBe("820 B");
    expect(formatBytes(24_576)).toBe("24 KB");
    expect(formatBytes(25_480_000)).toBe("24.3 MB");
    expect(formatBytes(3_221_225_472)).toBe("3.0 GB");
  });

  it("orientationLabel names 1–8 and passes the rest through", () => {
    expect(orientationLabel(1)).toBe("Normal");
    expect(orientationLabel(3)).toBe("Rotated 180°");
    expect(orientationLabel(6)).toBe("Rotated 90° CW");
    expect(orientationLabel(8)).toBe("Rotated 90° CCW");
    expect(orientationLabel(42)).toBe("42");
  });

  it("previewLine names the source and the backfill state", () => {
    expect(previewLine("embedded", true)).toBe("Embedded preview - full decode pending");
    expect(previewLine("full-decode", false)).toBe("Full-decode preview");
    expect(previewLine("original", false)).toBe("Preview from original");
    expect(previewLine(null, true)).toBe("No preview yet - full decode pending");
  });
});
