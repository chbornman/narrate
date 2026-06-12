/**
 * The header jobs pill copy (logic/jobs.ts — BACKLOG "Header shows
 * background jobs"): pure IngestStatus → the quiet register. Everything
 * the library digests in the background (ingest, preview rebuilds,
 * doctor re-pends, model backfills) flows through ingest_passes, so the
 * pass breakdown IS the complete jobs story.
 */
import { describe, expect, it } from "vitest";
import { jobsRemaining, jobsTitle } from "../src/lib/logic/jobs";
import type { IngestStatus } from "../src/lib/types/dto";

const status = (passes: IngestStatus["passes"]): IngestStatus => ({
  running: passes.length > 0,
  done: 0,
  total: 0,
  errors: 0,
  passes,
});

describe("jobs.ts — the background-jobs register", () => {
  it("sums remaining work across pass kinds; empty = 0 (the pill hides)", () => {
    expect(jobsRemaining(status([]))).toBe(0);
    expect(
      jobsRemaining(
        status([
          { name: "hash", remaining: 12 },
          { name: "preview", remaining: 480 },
        ]),
      ),
    ).toBe(492);
  });

  it("the hover title names kinds in reviewer words with their counts", () => {
    const title = jobsTitle(
      status([
        { name: "hash", remaining: 12 },
        { name: "preview", remaining: 480 },
        { name: "image-embedding", remaining: 1032 },
      ]),
    );
    expect(title).toBe(
      `Still digesting — hashing 12 · building previews 480 · embedding images ${(1032).toLocaleString()}`,
    );
  });

  it("an unknown pass name passes through verbatim (future passes surface themselves)", () => {
    expect(jobsTitle(status([{ name: "palette", remaining: 3 }]))).toBe(
      "Still digesting — palette 3",
    );
  });
});
