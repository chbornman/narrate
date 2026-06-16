/**
 * The Capture Station model (logic/station.ts — founder June 2026,
 * digest-visibility split): pure StationInput → { pulsing, seats, activities,
 * border }, CAPTURE-ONLY now. The digest/ingest/embed/download/offline/settled
 * signaling moved to the header Library-status indicator (see
 * librarystatus.test.ts); what remains here is mic + streaming-utterance state.
 * Seats are registry-action click targets; activities are the read-only hover
 * rows; `pulsing` is capture only (streaming speech / speech detected). Plus
 * transitionPops, the generalized note-pop move.
 */
import { describe, expect, it } from "vitest";
import {
  stationModel,
  transitionPops,
  type StationInput,
} from "../src/lib/logic/station";

const base: StationInput = {
  micState: "disarmed",
  asrReady: false,
  streaming: null,
};

describe("the quiet default", () => {
  it("settled library: no activities, no pulse, just the always-on seats", () => {
    const m = stationModel(base);
    expect(m.pulsing).toBe(false);
    expect(m.activities).toEqual([]);
    // No mic before ASR exists (§8.3 existence gate); no info dot with
    // nothing to tell — the search and note seats are the floor.
    expect(m.seats.map((s) => s.id)).toEqual(["search", "note"]);
    // No digest border any more — that is the header indicator's job.
    expect(m.border).toBe("none");
  });

  it("every CAPTURE seat carries a registry action id — the one-action-table rule", () => {
    const m = stationModel({
      ...base,
      asrReady: true,
      micState: "armedIdle",
    });
    expect(m.seats.map((s) => [s.id, s.actionId])).toEqual([
      ["mic", "mic-press"],
      ["search", "open-search"],
      ["info", "toggle-station-detail"],
      ["note", "summon-note"],
    ]);
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
    const idle = stationModel({
      ...base,
      asrReady: true,
      micState: "armedIdle",
    });
    expect(idle.seats[0].tone).toBeUndefined();
    expect(idle.seats[0].title).toContain("never written to disk");
    expect(idle.pulsing).toBe(false); // quiet listening is not "happening"
    expect(idle.activities.map((a) => a.text)).toEqual(["Listening…"]);
    const speaking = stationModel({
      ...base,
      asrReady: true,
      micState: "armedSpeaking",
    });
    expect(speaking.seats[0].tone).toBe("live");
    expect(speaking.pulsing).toBe(true);
    expect(speaking.activities.map((a) => a.text)).toEqual([
      "Listening - speech detected",
    ]);
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
    expect(m.activities.map((a) => a.text)).toEqual([
      "Capturing - words land on ● 3",
    ]);
  });
});

describe("activity ordering and the info seat (capture only)", () => {
  it("fixed order — mic · utterance (no digest rows here any more)", () => {
    const m = stationModel({
      micState: "armedSpeaking",
      asrReady: true,
      streaming: { kind: "single", count: 1 },
    });
    expect(m.activities.map((a) => a.kind)).toEqual(["mic", "utterance"]);
  });

  it("the info dot exists exactly when there is something to tell", () => {
    expect(stationModel(base).seats.some((s) => s.id === "info")).toBe(false);
    const busy = stationModel({
      ...base,
      asrReady: true,
      micState: "armedIdle",
    });
    expect(busy.seats.some((s) => s.id === "info")).toBe(true);
  });
});

describe("the collapsed pill border (capture only)", () => {
  it("mic armed paints the mic border; everything else is none", () => {
    expect(
      stationModel({ ...base, asrReady: true, micState: "armedIdle" }).border,
    ).toBe("mic");
    expect(
      stationModel({ ...base, asrReady: true, micState: "armedSpeaking" })
        .border,
    ).toBe("mic");
    expect(stationModel(base).border).toBe("none");
    // arming / degraded are not "armed" — no recording edge.
    expect(stationModel({ ...base, micState: "arming" }).border).toBe("none");
    expect(stationModel({ ...base, micState: "disarmedError" }).border).toBe(
      "none",
    );
  });
});

describe("transitionPops — the generalized pop move", () => {
  it("a clean arm pops; a clean disarm pops; no change pops nothing", () => {
    expect(
      transitionPops(
        { mic: "disarmed", streaming: false },
        { mic: "armedIdle", streaming: false },
      ),
    ).toEqual(["Mic armed"]);
    expect(
      transitionPops(
        { mic: "armedIdle", streaming: false },
        { mic: "disarmed", streaming: false },
      ),
    ).toEqual(["Mic off"]);
    expect(
      transitionPops(
        { mic: "armedIdle", streaming: false },
        { mic: "armedSpeaking", streaming: false },
      ),
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
