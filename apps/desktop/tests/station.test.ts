/**
 * The What's-Happening Station model (logic/station.ts — founder-confirmed
 * June 2026): pure StationInput → { pulsing, seats, activities }. Seats are
 * registry-action click targets; activities are the read-only hover rows;
 * `pulsing` is the founder's exact list — ingest/scan, a model download,
 * streaming speech. Plus transitionPops, the generalized note-pop move.
 */
import { describe, expect, it } from "vitest";
import {
  stationModel,
  transitionPops,
  type StationInput,
} from "../src/lib/logic/station";
import type { ModelRowDto, RuntimeStatus } from "../src/lib/types/dto";

const base: StationInput = {
  ingest: { running: false, done: 0, total: 0, errors: 0, passes: [] },
  runtime: null,
  micState: "disarmed",
  asrReady: false,
  streaming: null,
};

const model = (over: Partial<ModelRowDto> = {}): ModelRowDto => ({
  id: "dfn5b",
  role: "image-embedding",
  state: "installed",
  totalBytes: 1_000,
  downloadedBytes: 1_000,
  licenseName: "Apache-2.0",
  licenseUrl: "https://example.test",
  acceptanceRequired: false,
  accepted: true,
  error: null,
  ...over,
});

const runtime = (models: ModelRowDto[]): RuntimeStatus => ({
  asrReady: false,
  llmReady: false,
  clipReady: false,
  textEmbedderReady: false,
  tierDetected: 1,
  tierEffective: 1,
  tierOverriddenAbove: false,
  consent: "download",
  consentOfferBytes: 0,
  models,
  instanceLockHeld: true,
});

describe("the quiet default", () => {
  it("settled library: no activities, no pulse, just the always-on seats", () => {
    const m = stationModel(base);
    expect(m.pulsing).toBe(false);
    expect(m.activities).toEqual([]);
    // No mic before ASR exists (§8.3 existence gate); no info dot with
    // nothing to tell — the search and note seats are the floor.
    expect(m.seats.map((s) => s.id)).toEqual(["search", "note"]);
  });

  it("every seat carries a registry action id — the one-action-table rule", () => {
    const m = stationModel({
      ...base,
      asrReady: true,
      ingest: { running: true, done: 1, total: 2, errors: 0, passes: [] },
    });
    expect(m.seats.map((s) => [s.id, s.actionId])).toEqual([
      ["mic", "toggle-mic"],
      ["search", "open-search"],
      ["info", "toggle-station-detail"],
      ["note", "summon-note"],
    ]);
  });
});

describe("ingest and digest activities", () => {
  it("sized ingest carries locale counts and a 0..1 fraction; the station pulses", () => {
    const m = stationModel({
      ...base,
      ingest: { running: true, done: 1240, total: 48377, errors: 0, passes: [] },
    });
    expect(m.pulsing).toBe(true);
    const row = m.activities.find((a) => a.kind === "ingest");
    expect(row?.text).toBe("Indexing — 1,240 of 48,377");
    expect(row?.fraction).toBeCloseTo(1240 / 48377);
    expect(row?.hint).toBeUndefined();
  });

  it("unsized discovery says scanning honestly; errors surface as a quiet hint", () => {
    const scanning = stationModel({
      ...base,
      ingest: { running: true, done: 0, total: 0, errors: 0, passes: [] },
    });
    expect(scanning.activities[0].text).toBe("Indexing — scanning…");
    const errored = stationModel({
      ...base,
      ingest: { running: true, done: 5, total: 10, errors: 2, passes: [] },
    });
    expect(errored.activities[0].hint).toBe("2 errors");
  });

  it("digest rows speak jobs.ts's reviewer words, one per still-queued pass", () => {
    const m = stationModel({
      ...base,
      ingest: {
        running: false,
        done: 0,
        total: 0,
        errors: 0,
        passes: [
          { name: "preview", remaining: 480 },
          { name: "image-embedding", remaining: 1200 },
          { name: "settled", remaining: 0 }, // drained: no row
        ],
      },
    });
    expect(m.activities.map((a) => a.text)).toEqual([
      "Building previews — 480 remaining",
      "Embedding images — 1,200 remaining",
    ]);
    // Queued digestion alone is not the founder's pulse list.
    expect(m.pulsing).toBe(false);
  });
});

describe("download activities (RUNTIME §5.2 rows)", () => {
  it("a downloading row carries percent + fraction and pulses; error rides as the retry hint", () => {
    const m = stationModel({
      ...base,
      runtime: runtime([
        model({ state: "downloading", totalBytes: 1000, downloadedBytes: 620, error: "retrying, attempt 2" }),
      ]),
    });
    expect(m.pulsing).toBe(true);
    const row = m.activities.find((a) => a.kind === "download");
    expect(row?.text).toBe("Downloading dfn5b — 62%");
    expect(row?.hint).toBe("retrying, attempt 2");
    expect(row?.fraction).toBeCloseTo(0.62);
  });

  it("a failed row is honest, hints at Settings, and does NOT pulse", () => {
    const m = stationModel({
      ...base,
      runtime: runtime([model({ state: "failed", error: null })]),
    });
    const row = m.activities.find((a) => a.kind === "download-failed");
    expect(row?.text).toBe("Download failed — dfn5b");
    expect(row?.hint).toBe("retry from Settings");
    expect(m.pulsing).toBe(false);
  });

  it("installed/not-downloaded rows leave no activity", () => {
    const m = stationModel({
      ...base,
      runtime: runtime([model(), model({ id: "other", state: "not-downloaded" })]),
    });
    expect(m.activities).toEqual([]);
  });
});

describe("the mic seat mirrors CAPTURE §6.4/§11 (the modes.ts mapping)", () => {
  it("absent until ASR exists; dimmed mic when disarmed-but-ready", () => {
    expect(stationModel(base).seats.some((s) => s.id === "mic")).toBe(false);
    const seat = stationModel({ ...base, asrReady: true }).seats[0];
    expect(seat.id).toBe("mic");
    expect(seat.icon).toBe("mic");
    expect(seat.tone).toBe("dim");
    // Seat titles never name keys — the mic key is moving; the registry
    // tooltip is the only place chords render from.
    expect(seat.title).not.toMatch(/\bM\b|Space/);
  });

  it("armed = solid with the privacy claim; speaking breathes and pulses", () => {
    const idle = stationModel({ ...base, asrReady: true, micState: "armedIdle" });
    expect(idle.seats[0].tone).toBeUndefined();
    expect(idle.seats[0].title).toContain("never written to disk");
    expect(idle.pulsing).toBe(false); // quiet listening is not "happening"
    expect(idle.activities.map((a) => a.text)).toEqual(["Listening…"]);
    const speaking = stationModel({ ...base, asrReady: true, micState: "armedSpeaking" });
    expect(speaking.seats[0].tone).toBe("live");
    expect(speaking.pulsing).toBe(true);
    expect(speaking.activities.map((a) => a.text)).toEqual(["Listening — speech detected"]);
  });

  it("degraded = the muted-mic glyph + the §7.3 line, even before asrReady", () => {
    const m = stationModel({ ...base, micState: "disarmedError" });
    expect(m.seats[0].icon).toBe("mic-off");
    expect(m.seats[0].tone).toBe("dim");
    expect(m.activities[0].text).toBe(
      "Voice capture unavailable — typed notes and pencil still work.",
    );
  });
});

describe("the streaming utterance (§5.4)", () => {
  it("names where words land and pulses while in flight", () => {
    const m = stationModel({ ...base, streaming: { kind: "multi", count: 3 } });
    expect(m.pulsing).toBe(true);
    expect(m.activities.map((a) => a.text)).toEqual(["Capturing — words land on ● 3"]);
  });
});

describe("activity ordering and the info seat", () => {
  it("fixed order — ingest · digest · downloads · mic · utterance", () => {
    const m = stationModel({
      ingest: {
        running: true,
        done: 1,
        total: 2,
        errors: 0,
        passes: [{ name: "exif", remaining: 9 }],
      },
      runtime: runtime([model({ state: "downloading", downloadedBytes: 0 })]),
      micState: "armedSpeaking",
      asrReady: true,
      streaming: { kind: "single", count: 1 },
    });
    expect(m.activities.map((a) => a.kind)).toEqual([
      "ingest",
      "digest",
      "download",
      "mic",
      "utterance",
    ]);
  });

  it("the info dot exists exactly when there is something to tell", () => {
    expect(stationModel(base).seats.some((s) => s.id === "info")).toBe(false);
    const busy = stationModel({
      ...base,
      ingest: { running: true, done: 0, total: 0, errors: 0, passes: [] },
    });
    expect(busy.seats.some((s) => s.id === "info")).toBe(true);
  });
});

describe("transitionPops — the generalized pop move", () => {
  it("a clean arm pops; a clean disarm pops; no change pops nothing", () => {
    expect(
      transitionPops({ mic: "disarmed", streaming: false }, { mic: "armedIdle", streaming: false }),
    ).toEqual(["Mic armed"]);
    expect(
      transitionPops({ mic: "armedIdle", streaming: false }, { mic: "disarmed", streaming: false }),
    ).toEqual(["Mic off"]);
    expect(
      transitionPops({ mic: "armedIdle", streaming: false }, { mic: "armedSpeaking", streaming: false }),
    ).toEqual([]);
  });

  it("an utterance finalizing pops Captured (the push-to-talk release)", () => {
    expect(
      transitionPops(
        { mic: "armedSpeaking", streaming: true },
        { mic: "armedIdle", streaming: false },
      ),
    ).toEqual(["Captured"]);
  });

  it("error states never pop — the seat and the activity row tell that story", () => {
    expect(
      transitionPops(
        { mic: "armedIdle", streaming: false },
        { mic: "disarmedError", streaming: false },
      ),
    ).toEqual([]);
  });
});
