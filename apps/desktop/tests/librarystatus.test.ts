/**
 * The Library-status model (logic/librarystatus.ts — BACKLOG "digest
 * visibility", founder June 2026): pure (IngestStatus + runtime) → the
 * header-center indicator's view. Covers the canonical stage mapping +
 * ordering, settled vs working, the waiting-on assembly (offline +
 * downloading + degraded embedder), ETA derivation (rate 0 -> null, rate > 0
 * -> remaining/rate), the human formatters, and the unknown-pass fallthrough.
 */
import { describe, expect, it } from "vitest";
import {
  libraryStatusModel,
  formatEta,
  formatRate,
  formatCount,
  type LibraryStatusInput,
} from "../src/lib/logic/librarystatus";
import type { IngestStatus, ModelRowDto, RuntimeStatus } from "../src/lib/types/dto";

type Pass = IngestStatus["passes"][number];

/** A pass row with sensible defaults; override what a case cares about. */
const pass = (over: Partial<Pass> & { name: string }): Pass => ({
  remaining: 0,
  done: 0,
  total: 0,
  ratePerSec: 0,
  ...over,
});

const ingest = (over: Partial<IngestStatus> = {}): IngestStatus => ({
  running: false,
  done: 0,
  total: 0,
  errors: 0,
  passes: [],
  scanning: false,
  discovered: 0,
  offlineVolumes: [],
  ...over,
});

const input = (over: Partial<LibraryStatusInput> = {}): LibraryStatusInput => ({
  ingest: ingest(),
  runtime: null,
  ...over,
});

const model = (over: Partial<ModelRowDto> = {}): ModelRowDto => ({
  id: "dfn5b",
  role: "embedder",
  state: "installed",
  totalBytes: 1_000,
  downloadedBytes: 1_000,
  licenseName: "Apache-2.0",
  licenseUrl: "https://example.test",
  acceptanceRequired: false,
  accepted: true,
  error: null,
  retryHint: null,
  ...over,
});

const runtime = (over: Partial<RuntimeStatus> = {}): RuntimeStatus => ({
  asrReady: false,
  llmReady: false,
  asrBlocked: null,
  llmBlocked: null,
  clipReady: true,
  textEmbedderReady: true,
  tierDetected: 1,
  tierEffective: 1,
  tierOverriddenAbove: false,
  consent: "download",
  consentOfferBytes: 0,
  models: [],
  instanceLockHeld: true,
  ...over,
});

describe("settled vs working", () => {
  it("nothing running/scanning/downloading -> settled, the calm headline", () => {
    const m = libraryStatusModel(input());
    expect(m.settled).toBe(true);
    expect(m.headline).toBe("Library settled");
    expect(m.stages).toEqual([]);
    expect(m.current).toBeNull();
    expect(m.etaSecs).toBeNull();
  });

  it("a pass with remaining units -> working", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({
          passes: [pass({ name: "preview", remaining: 40, done: 60, total: 100, ratePerSec: 5 })],
        }),
      }),
    );
    expect(m.settled).toBe(false);
    expect(m.headline).toBe("Library is working");
    expect(m.current?.id).toBe("preview");
  });

  it("a scanning walk -> working with the discover stage, even with no passes yet", () => {
    const m = libraryStatusModel(
      input({ ingest: ingest({ scanning: true, running: true, discovered: 312 }) }),
    );
    expect(m.settled).toBe(false);
    const discover = m.stages.find((s) => s.id === "discover");
    expect(discover?.label).toBe("Discovering");
    expect(discover?.done).toBe(312);
    expect(discover?.state).toBe("working");
  });
});

describe("stage mapping + ordering", () => {
  it("maps pass names onto canonical stages in the founder order", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({
          // deliberately out of order in the input
          passes: [
            pass({ name: "image-embedding", remaining: 5, done: 5, total: 10, ratePerSec: 1 }),
            pass({ name: "preview", remaining: 5, done: 5, total: 10, ratePerSec: 1 }),
            pass({ name: "exif", remaining: 5, done: 5, total: 10, ratePerSec: 1 }),
            pass({ name: "hash", remaining: 5, done: 5, total: 10, ratePerSec: 1 }),
          ],
        }),
      }),
    );
    // hash -> meta(exif) -> preview -> embed, regardless of input order.
    expect(m.stages.map((s) => s.id)).toEqual(["hash", "meta", "preview", "embed"]);
    expect(m.stages.map((s) => s.label)).toEqual([
      "Hashing",
      "Reading metadata",
      "Building previews",
      "Embedding for search",
    ]);
  });

  it("rolls image+text embedding into ONE embed stage, max rate, summed counts", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({
          passes: [
            pass({ name: "image-embedding", remaining: 800, done: 200, total: 1000, ratePerSec: 4 }),
            pass({ name: "text-embedding", remaining: 100, done: 100, total: 200, ratePerSec: 9 }),
          ],
        }),
      }),
    );
    const embed = m.stages.find((s) => s.id === "embed");
    expect(embed?.done).toBe(300); // 200 + 100
    expect(embed?.total).toBe(1200); // 1000 + 200
    expect(embed?.ratePerSec).toBe(9); // MAX, not sum
    expect(embed?.fraction).toBeCloseTo(300 / 1200);
  });

  it("an UNKNOWN pass name lands in a TRAILING generic stage, never dropped", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({
          passes: [
            pass({ name: "hash", remaining: 1, done: 1, total: 2, ratePerSec: 1 }),
            pass({ name: "palette", remaining: 3, done: 0, total: 3, ratePerSec: 1 }),
            pass({ name: "full-raw-decode", remaining: 2, done: 0, total: 2, ratePerSec: 1 }),
          ],
        }),
      }),
    );
    const ids = m.stages.map((s) => s.id);
    expect(ids).toContain("other");
    // the trailing stage is LAST (after the canonical ones)
    expect(ids[ids.length - 1]).toBe("other");
    // both unmapped passes rolled into it (3 + 2 remaining), nothing dropped
    const other = m.stages.find((s) => s.id === "other");
    expect(other?.done).toBe(0);
    expect(other?.total).toBe(5);
  });
});

describe("per-stage state", () => {
  it("no remaining -> done; remaining + rate -> working; remaining + frozen rate -> pending", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({
          passes: [
            pass({ name: "hash", remaining: 0, done: 10, total: 10, ratePerSec: 0 }),
            pass({ name: "exif", remaining: 5, done: 5, total: 10, ratePerSec: 3 }),
            pass({ name: "preview", remaining: 5, done: 5, total: 10, ratePerSec: 0 }),
          ],
        }),
      }),
    );
    expect(m.stages.find((s) => s.id === "hash")?.state).toBe("done");
    expect(m.stages.find((s) => s.id === "meta")?.state).toBe("working");
    expect(m.stages.find((s) => s.id === "preview")?.state).toBe("pending");
    // current = first working
    expect(m.current?.id).toBe("meta");
  });
});

describe("ETA derivation", () => {
  it("rate 0 -> null etaSecs (paused/unknown shows no countdown)", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({
          passes: [pass({ name: "preview", remaining: 100, done: 0, total: 100, ratePerSec: 0 })],
        }),
      }),
    );
    expect(m.stages[0].etaSecs).toBeNull();
    expect(m.etaSecs).toBeNull();
  });

  it("rate > 0 -> remaining / rate", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({
          passes: [pass({ name: "preview", remaining: 120, done: 0, total: 120, ratePerSec: 4 })],
        }),
      }),
    );
    expect(m.stages[0].etaSecs).toBe(30); // 120 / 4
    expect(m.etaSecs).toBe(30);
  });

  it("overall ETA sums the working+pending stages' ETAs (series), ignoring paused", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({
          passes: [
            pass({ name: "hash", remaining: 0, done: 10, total: 10, ratePerSec: 5 }), // done: ignored
            pass({ name: "exif", remaining: 40, done: 0, total: 40, ratePerSec: 4 }), // 10s
            pass({ name: "preview", remaining: 60, done: 0, total: 60, ratePerSec: 2 }), // 30s
            pass({ name: "image-embedding", remaining: 9, done: 0, total: 9, ratePerSec: 0 }), // paused: null
          ],
        }),
      }),
    );
    expect(m.etaSecs).toBe(40); // 10 + 30; the paused embed contributes nothing
  });
});

describe("waitingOn assembly (offline + downloading + degraded, top-most first)", () => {
  it("offline volumes -> a paused row with the label + photo count", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({ offlineVolumes: [{ label: "HomeNAS", images: 414 }] }),
      }),
    );
    expect(m.waitingOn.map((w) => w.text)).toEqual([
      "Paused - HomeNAS offline (414 photos)",
    ]);
    // one offline drive: still NOT settled (work is blocked, not absent)
    expect(m.settled).toBe(false);
  });

  it("a single photo pluralizes correctly", () => {
    const m = libraryStatusModel(
      input({ ingest: ingest({ offlineVolumes: [{ label: "Archive", images: 1 }] }) }),
    );
    expect(m.waitingOn[0].text).toBe("Paused - Archive offline (1 photo)");
  });

  it("a downloading model -> '<id> downloading (62%)'", () => {
    const m = libraryStatusModel(
      input({
        runtime: runtime({
          models: [
            model({ id: "dfn5b", state: "downloading", totalBytes: 1000, downloadedBytes: 620 }),
          ],
        }),
      }),
    );
    expect(m.waitingOn.map((w) => w.text)).toEqual(["dfn5b downloading (62%)"]);
    expect(m.settled).toBe(false); // a download keeps the library working
  });

  it("a degraded embedder (installed but not ready) -> 'embedder loading'", () => {
    const m = libraryStatusModel(
      input({
        runtime: runtime({
          clipReady: false,
          models: [model({ role: "embedder", state: "installed" })],
        }),
      }),
    );
    expect(m.waitingOn.map((w) => w.text)).toEqual(["embedder loading"]);
  });

  it("all three together, ordered offline -> downloading -> degraded", () => {
    const m = libraryStatusModel(
      input({
        ingest: ingest({ offlineVolumes: [{ label: "HomeNAS", images: 10 }] }),
        runtime: runtime({
          clipReady: false,
          models: [
            model({ id: "txt", role: "text-embedder", state: "downloading", totalBytes: 100, downloadedBytes: 50 }),
            model({ id: "clip", role: "embedder", state: "installed" }),
          ],
        }),
      }),
    );
    expect(m.waitingOn.map((w) => w.text)).toEqual([
      "Paused - HomeNAS offline (10 photos)",
      "txt downloading (50%)",
      "embedder loading",
    ]);
  });

  it("a ready embedder is not 'loading'; no waiting-on row", () => {
    const m = libraryStatusModel(
      input({
        runtime: runtime({
          clipReady: true,
          textEmbedderReady: true,
          models: [model({ role: "embedder", state: "installed" })],
        }),
      }),
    );
    expect(m.waitingOn).toEqual([]);
  });
});

describe("errors row", () => {
  it("carries the ingest error count through for the panel", () => {
    const m = libraryStatusModel(input({ ingest: ingest({ errors: 7 }) }));
    expect(m.errors).toBe(7);
  });
});

describe("the human formatters", () => {
  it("formatEta: ~30s under a minute, ~6m under an hour, ~2h above", () => {
    expect(formatEta(30)).toBe("~30s");
    expect(formatEta(90)).toBe("~2m");
    expect(formatEta(360)).toBe("~6m");
    expect(formatEta(7200)).toBe("~2h");
  });

  it("formatEta: null / non-positive -> empty (caller hides it)", () => {
    expect(formatEta(null)).toBe("");
    expect(formatEta(0)).toBe("");
    expect(formatEta(-5)).toBe("");
  });

  it("formatRate: '12/s', sub-10 keeps a decimal, thousands read as 'k/s'", () => {
    expect(formatRate(12)).toBe("12/s");
    expect(formatRate(0.4)).toBe("0.4/s");
    expect(formatRate(1200)).toBe("1.2k/s");
    expect(formatRate(15000)).toBe("15k/s");
  });

  it("formatRate: 0 / unknown -> empty", () => {
    expect(formatRate(0)).toBe("");
    expect(formatRate(-1)).toBe("");
  });

  it("formatCount: locale-grouped 'done / total'", () => {
    expect(formatCount(240, 5000)).toBe("240 / 5,000");
  });
});
