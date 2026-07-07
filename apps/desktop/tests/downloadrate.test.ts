/**
 * Download progress detail (logic/downloadrate.ts — audit D5): the rate
 * smoothing over irregular runtime-status samples and the "1.2 / 3.9 GB
 * · 8 MB/s" formatting the settings row renders.
 */
import { describe, expect, it } from "vitest";
import {
  formatRate,
  formatSizePair,
  progressDetail,
  updateRate,
  type RateState,
} from "../src/lib/logic/downloadrate";

const GB = 1024 * 1024 * 1024;
const MB = 1024 * 1024;

describe("updateRate", () => {
  it("first sample has no rate (nothing to compare against)", () => {
    const s = updateRate(null, 1000, 0);
    expect(s.bytesPerSec).toBeNull();
    expect(s.bytes).toBe(0);
  });

  it("second sample yields the interval rate", () => {
    let s = updateRate(null, 0, 0);
    s = updateRate(s, 1000, 8 * MB); // 8 MB over 1 s
    expect(s.bytesPerSec).toBeCloseTo(8 * MB);
  });

  it("smooths toward a new rate instead of jumping", () => {
    let s = updateRate(null, 0, 0);
    s = updateRate(s, 1000, 10 * MB); // 10 MB/s established
    s = updateRate(s, 2000, 10 * MB + 2 * MB); // instant drops to 2 MB/s
    // EWMA: between the old 10 and the instant 2, closer to 10.
    expect(s.bytesPerSec).toBeGreaterThan(2 * MB);
    expect(s.bytesPerSec).toBeLessThan(10 * MB);
  });

  it("a stall (equal bytes) decays the rate toward zero", () => {
    let s = updateRate(null, 0, 0);
    s = updateRate(s, 1000, 10 * MB);
    const before = s.bytesPerSec ?? 0;
    s = updateRate(s, 2000, 10 * MB);
    expect(s.bytesPerSec).not.toBeNull();
    expect(s.bytesPerSec ?? 0).toBeLessThan(before);
  });

  it("a rewound counter (checksum retry discarded a part) resets, never negative", () => {
    let s: RateState | null = updateRate(null, 0, 0);
    s = updateRate(s, 1000, 10 * MB);
    s = updateRate(s, 2000, 4 * MB); // went DOWN
    expect(s.bytesPerSec).toBeNull();
  });

  it("a non-advancing clock keeps the previous estimate (no divide by zero)", () => {
    let s = updateRate(null, 0, 0);
    s = updateRate(s, 1000, 10 * MB);
    const kept = updateRate(s, 1000, 11 * MB); // duplicate-ms snapshot
    expect(kept).toEqual(s);
  });
});

describe("formatting", () => {
  it("size pair shares the total's unit", () => {
    expect(formatSizePair(1.2 * GB, 3.9 * GB)).toBe("1.2 / 3.9 GB");
    // Below a GB total, both numbers speak MB.
    expect(formatSizePair(50 * MB, 300 * MB)).toBe("50 / 300 MB");
    // Mixed magnitudes still share the unit (the WHY of the pair form).
    expect(formatSizePair(200 * MB, 13.4 * GB)).toBe("0.2 / 13.4 GB");
  });

  it("rate reads MB/s above a MB, KB/s below", () => {
    expect(formatRate(8.04 * MB)).toBe("8.0 MB/s");
    expect(formatRate(512 * 1024)).toBe("512 KB/s");
    // A crawling transfer still shows life, not "0 KB/s".
    expect(formatRate(10)).toBe("1 KB/s");
  });

  it("detail line appends the rate only when known", () => {
    expect(progressDetail(1.2 * GB, 3.9 * GB, 8 * MB)).toBe("1.2 / 3.9 GB · 8.0 MB/s");
    expect(progressDetail(1.2 * GB, 3.9 * GB, null)).toBe("1.2 / 3.9 GB");
    expect(progressDetail(1.2 * GB, 3.9 * GB, 0)).toBe("1.2 / 3.9 GB");
  });

  it("copy contains no em-dash (user-visible copy rule)", () => {
    const line = progressDetail(1.2 * GB, 3.9 * GB, 8 * MB);
    expect(line.includes("—")).toBe(false);
  });
});
