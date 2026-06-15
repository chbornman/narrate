/**
 * The What's-Happening Station model (logic/station.ts — founder-confirmed
 * June 2026): pure StationInput → { pulsing, seats, activities }. Seats are
 * registry-action click targets; activities are the read-only hover rows;
 * `pulsing` is the founder's exact list — ingest/scan, a model download,
 * streaming speech. Plus transitionPops, the generalized note-pop move.
 */
import { describe, expect, it } from "vitest";
import {
  borderState,
  missingModels,
  stationModel,
  transitionPops,
  type StationInput,
} from "../src/lib/logic/station";
import type { ModelRowDto, RuntimeStatus } from "../src/lib/types/dto";

const base: StationInput = {
  ingest: { running: false, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
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
  retryHint: null,
  ...over,
});

const runtime = (models: ModelRowDto[]): RuntimeStatus => ({
  asrReady: false,
  llmReady: false,
  asrBlocked: null,
  llmBlocked: null,
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
      ingest: { running: true, done: 1, total: 2, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
    });
    expect(m.seats.map((s) => [s.id, s.actionId])).toEqual([
      ["mic", "mic-press"],
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
      ingest: { running: true, done: 1240, total: 48377, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
    });
    expect(m.pulsing).toBe(true);
    const row = m.activities.find((a) => a.kind === "ingest");
    expect(row?.text).toBe("Indexing - 1,240 of 48,377");
    expect(row?.fraction).toBeCloseTo(1240 / 48377);
    expect(row?.hint).toBeUndefined();
  });

  it("unsized discovery says scanning honestly; errors surface as a quiet hint", () => {
    const scanning = stationModel({
      ...base,
      ingest: { running: true, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
    });
    expect(scanning.activities[0].text).toBe("Indexing - scanning…");
    const errored = stationModel({
      ...base,
      ingest: { running: true, done: 5, total: 10, errors: 2, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
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
        scanning: false,
        discovered: 0, offlineVolumes: []
      },
    });
    expect(m.activities.map((a) => a.text)).toEqual([
      "Building previews - 480 remaining",
      "Embedding images - 1,200 remaining",
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
    expect(row?.text).toBe("Downloading dfn5b - 62%");
    expect(row?.hint).toBe("retrying, attempt 2");
    expect(row?.fraction).toBeCloseTo(0.62);
  });

  it("a failed row is honest, hints at Settings, and does NOT pulse", () => {
    const m = stationModel({
      ...base,
      runtime: runtime([model({ state: "failed", error: null })]),
    });
    const row = m.activities.find((a) => a.kind === "download-failed");
    expect(row?.text).toBe("Download failed - dfn5b");
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
    expect(speaking.activities.map((a) => a.text)).toEqual(["Listening - speech detected"]);
  });

  it("degraded = the muted-mic glyph + the §7.3 line, even before asrReady", () => {
    const m = stationModel({ ...base, micState: "disarmedError" });
    expect(m.seats[0].icon).toBe("mic-off");
    expect(m.seats[0].tone).toBe("dim");
    expect(m.activities[0].text).toBe(
      "Voice capture unavailable - typed notes and pencil still work.",
    );
  });
});

describe("the streaming utterance (§5.4)", () => {
  it("names where words land and pulses while in flight", () => {
    const m = stationModel({ ...base, streaming: { kind: "multi", count: 3 } });
    expect(m.pulsing).toBe(true);
    expect(m.activities.map((a) => a.text)).toEqual(["Capturing - words land on ● 3"]);
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
        scanning: false,
        discovered: 0, offlineVolumes: []
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
      ingest: { running: true, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
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

// ---------------------------------------------------------------------------
// Station 2.0 (DESIGN-STATION.md): the border-priority resolver, the
// transient icons (active/retire), missing-model detection + surfacing, and
// the count/progress mapping.
// ---------------------------------------------------------------------------

describe("borderState — the collapsed pill's priority resolver", () => {
  it("mic armed beats everything (red)", () => {
    expect(borderState({ micArmed: true, hasError: true, working: true })).toBe("mic");
  });
  it("error beats working when the mic is not armed (amber)", () => {
    expect(borderState({ micArmed: false, hasError: true, working: true })).toBe("error");
  });
  it("working when only background work is live (cool hue)", () => {
    expect(borderState({ micArmed: false, hasError: false, working: true })).toBe("working");
  });
  it("idle = no border", () => {
    expect(borderState({ micArmed: false, hasError: false, working: false })).toBe("none");
  });

  it("the full priority ladder, mic > error > working > idle", () => {
    const ladder: Array<[Parameters<typeof borderState>[0], string]> = [
      [{ micArmed: true, hasError: false, working: false }, "mic"],
      [{ micArmed: false, hasError: true, working: false }, "error"],
      [{ micArmed: false, hasError: false, working: true }, "working"],
      [{ micArmed: false, hasError: false, working: false }, "none"],
    ];
    for (const [input, want] of ladder) expect(borderState(input)).toBe(want);
  });
});

describe("the border resolves end-to-end through stationModel", () => {
  it("mic armed paints the mic border even while ingest works (mic wins)", () => {
    const m = stationModel({
      ...base,
      asrReady: true,
      micState: "armedIdle",
      ingest: { running: true, done: 1, total: 9, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
    });
    expect(m.border).toBe("mic");
  });

  it("a needed-but-missing model paints amber over working", () => {
    const m = stationModel({
      ...base,
      // a still-queued non-embed pass = working; the missing CLIP = error
      ingest: {
        running: false,
        done: 0,
        total: 0,
        errors: 0,
        passes: [{ name: "preview", remaining: 40 }],
        scanning: false,
        discovered: 0, offlineVolumes: []
      },
      runtime: runtime([model({ id: "dfn5b", role: "embedder", state: "not-downloaded" })]),
    });
    expect(m.border).toBe("error");
  });

  it("a failed download is the error border", () => {
    const m = stationModel({ ...base, runtime: runtime([model({ state: "failed" })]) });
    expect(m.border).toBe("error");
  });

  it("only background work → the working border; settled → none", () => {
    const working = stationModel({
      ...base,
      ingest: { running: true, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
    });
    expect(working.border).toBe("working");
    expect(stationModel(base).border).toBe("none");
  });
});

describe("missingModels — a feature's model is absent", () => {
  it("backend dark (null runtime) surfaces nothing", () => {
    expect(missingModels(null)).toEqual([]);
  });

  it("an installed or downloading needed model is NOT missing", () => {
    expect(
      missingModels(runtime([model({ role: "embedder", state: "installed" })])),
    ).toEqual([]);
    expect(
      missingModels(runtime([model({ role: "embedder", state: "downloading" })])),
    ).toEqual([]);
  });

  it("a not-downloaded CLIP embedder surfaces with its feature + size", () => {
    const out = missingModels(
      runtime([
        model({
          id: "dfn5b",
          role: "embedder",
          state: "not-downloaded",
          totalBytes: 1_200_000_000,
          acceptanceRequired: true,
          accepted: false,
        }),
      ]),
    );
    expect(out).toHaveLength(1);
    expect(out[0].id).toBe("dfn5b");
    expect(out[0].feature).toBe("Semantic search needs the CLIP model");
    expect(out[0].sizeLabel).toBe("1.2 GB");
    // gates on a license the user has not accepted: the prompt offers Accept.
    expect(out[0].needsLicense).toBe(true);
  });

  it("an accepted-license model needs no license step", () => {
    const out = missingModels(
      runtime([model({ role: "embedder", state: "failed", acceptanceRequired: true, accepted: true })]),
    );
    expect(out[0].needsLicense).toBe(false);
  });

  it("roles the app does not rely on are never 'missing' (e.g. *-alt, llm)", () => {
    expect(
      missingModels(
        runtime([
          model({ role: "text-embedder-alt", state: "not-downloaded" }),
          model({ role: "llm", state: "not-downloaded" }),
        ]),
      ),
    ).toEqual([]);
  });

  it("MB-scale models read in MB, not a 0.0 GB", () => {
    const out = missingModels(
      runtime([model({ role: "text-embedder", state: "not-downloaded", totalBytes: 316_000_000 })]),
    );
    expect(out[0].sizeLabel).toBe("316 MB");
    expect(out[0].feature).toBe("Semantic search needs the text model");
  });
});

describe("transient icons — active while live, then retire", () => {
  it("a settled library has no transients", () => {
    expect(stationModel(base).transients).toEqual([]);
  });

  it("ingest = the work icon with a count badge + a 0..1 arc fraction", () => {
    const m = stationModel({
      ...base,
      ingest: { running: true, done: 1240, total: 5000, errors: 0, passes: [], scanning: false, discovered: 5000, offlineVolumes: [] },
    });
    const work = m.transients.find((t) => t.id === "work");
    expect(work?.icon).toBe("loader");
    expect(work?.count).toBe(5000); // the sized total
    expect(work?.fraction).toBeCloseTo(1240 / 5000);
  });

  it("unsized discovery shows the discovered count, no arc", () => {
    const m = stationModel({
      ...base,
      ingest: { running: true, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 312, offlineVolumes: [] },
    });
    const work = m.transients.find((t) => t.id === "work");
    expect(work?.count).toBe(312);
    expect(work?.fraction).toBeUndefined();
  });

  it("embedding passes get their OWN icon, separate from ingest/digest", () => {
    const m = stationModel({
      ...base,
      ingest: {
        running: false,
        done: 0,
        total: 0,
        errors: 0,
        passes: [
          { name: "preview", remaining: 40 },
          { name: "image-embedding", remaining: 800 },
          { name: "text-embedding", remaining: 200 },
        ],
        scanning: false,
        discovered: 0, offlineVolumes: []
      },
    });
    const ids = m.transients.map((t) => t.id);
    expect(ids).toContain("work"); // the non-embed digest backlog
    const embed = m.transients.find((t) => t.id === "embed");
    expect(embed?.icon).toBe("sparkles");
    expect(embed?.count).toBe(1000); // image + text embedding rolled up
    // the work icon's badge counts only the non-embed backlog (preview: 40)
    expect(m.transients.find((t) => t.id === "work")?.count).toBe(40);
  });

  it("a download = the download icon with the aggregate byte arc", () => {
    const m = stationModel({
      ...base,
      runtime: runtime([
        model({ state: "downloading", totalBytes: 1000, downloadedBytes: 250 }),
        model({ id: "b", state: "downloading", totalBytes: 1000, downloadedBytes: 250 }),
      ]),
    });
    const dl = m.transients.find((t) => t.id === "download");
    expect(dl?.icon).toBe("download");
    expect(dl?.fraction).toBeCloseTo(0.25); // (250+250)/(1000+1000)
  });

  it("a missing model = the amber warning transient", () => {
    const m = stationModel({
      ...base,
      runtime: runtime([model({ role: "embedder", state: "not-downloaded" })]),
    });
    const warn = m.transients.find((t) => t.id === "missing");
    expect(warn?.icon).toBe("triangle-alert");
    expect(warn?.warn).toBe(true);
    expect(m.missingModels).toHaveLength(1);
  });

  it("retire: once installed, the missing transient is gone", () => {
    const m = stationModel({
      ...base,
      runtime: runtime([model({ role: "embedder", state: "installed" })]),
    });
    expect(m.transients.find((t) => t.id === "missing")).toBeUndefined();
    expect(m.missingModels).toEqual([]);
  });
});

describe("every transient is its own click target (founder, June 13 2026)", () => {
  // The whole live set in one snapshot: ingest (work) + an embedding backlog
  // (embed) + an active download + a needed-but-missing model (missing).
  const live = stationModel({
    ...base,
    ingest: {
      running: true,
      done: 10,
      total: 100,
      errors: 0,
      passes: [{ name: "image-embedding", remaining: 50 }],
      scanning: false,
      discovered: 100, offlineVolumes: []
    },
    runtime: runtime([
      model({ id: "dfn5b", role: "embedder", state: "downloading", totalBytes: 1000, downloadedBytes: 250 }),
      model({ id: "txt", role: "text-embedder", state: "not-downloaded" }),
    ]),
  });

  it("work, embed, download, and missing all carry a registry actionId", () => {
    // No transient may be left as read-only chrome — each is clickable.
    for (const t of live.transients) expect(t.actionId).toBe("toggle-station-detail");
    expect(new Set(live.transients.map((t) => t.id))).toEqual(
      new Set(["work", "embed", "download", "missing"]),
    );
  });

  it("each transient's verb pins the detail panel open (where its row resolves)", () => {
    // The verb is the SAME registry row the info seat uses — one action table,
    // zero new verbs (the missing-model prompt lives in that detail panel).
    const infoVerb = stationModel({
      ...base,
      ingest: { running: true, done: 0, total: 0, errors: 0, passes: [], scanning: false, discovered: 0, offlineVolumes: [] },
    }).seats.find((s) => s.id === "info")?.actionId;
    expect(infoVerb).toBe("toggle-station-detail");
    for (const t of live.transients) expect(t.actionId).toBe(infoVerb);
  });

  it("titles invite the click and never name a key (resolved from the registry)", () => {
    for (const t of live.transients) {
      expect(t.title).toMatch(/click/i);
      expect(t.title).not.toMatch(/\bM\b|Space|Ctrl|⌘/);
      // No em-dashes in user-visible copy (gate: check:emdash).
      expect(t.title).not.toContain("—");
    }
  });
});
