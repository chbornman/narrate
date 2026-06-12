/**
 * The M-key two-gesture mic machine (logic/michold.ts; CAPTURE §6.4 —
 * founder ruling, June 2026): tap toggles, press-and-hold is push-to-talk.
 * Time is a parameter (the confirmhold.ts pattern) — no fake timers.
 */
import { describe, expect, it } from "vitest";
import {
  MIC_HOLD_IDLE,
  MIC_HOLD_MS,
  micBlur,
  micDown,
  micUp,
} from "../src/lib/logic/michold";

describe("from DISARMED (the gesture that owns push-to-talk)", () => {
  it("press arms IMMEDIATELY — both gestures want sound flowing from the keydown", () => {
    const { state, intent } = micDown(MIC_HOLD_IDLE, { armed: false, now: 1000 });
    expect(intent).toBe("arm");
    expect(state).toEqual({ pressedAt: 1000, armedOnPress: true });
  });

  it("tap (release before the threshold) keeps the arm — that IS toggle-on", () => {
    const down = micDown(MIC_HOLD_IDLE, { armed: false, now: 1000 });
    const up = micUp(down.state, 1000 + MIC_HOLD_MS - 1);
    expect(up.intent).toBe("none");
    expect(up.state).toEqual(MIC_HOLD_IDLE);
  });

  it("hold (release at/after the threshold) is PTT: release disarms and ships", () => {
    const down = micDown(MIC_HOLD_IDLE, { armed: false, now: 1000 });
    const up = micUp(down.state, 1000 + MIC_HOLD_MS);
    expect(up.intent).toBe("disarm");
    expect(up.state).toEqual(MIC_HOLD_IDLE);
  });
});

describe("from ARMED (toggled on earlier)", () => {
  it("press does nothing — the release decides", () => {
    const { state, intent } = micDown(MIC_HOLD_IDLE, { armed: true, now: 1000 });
    expect(intent).toBe("none");
    expect(state).toEqual({ pressedAt: 1000, armedOnPress: false });
  });

  it("tap disarms — the toggle half", () => {
    const down = micDown(MIC_HOLD_IDLE, { armed: true, now: 1000 });
    const up = micUp(down.state, 1100);
    expect(up.intent).toBe("disarm");
  });

  it("hold is INERT: PTT only applies from disarmed — an absent-minded hold must not tear down a deliberate toggle", () => {
    const down = micDown(MIC_HOLD_IDLE, { armed: true, now: 1000 });
    const up = micUp(down.state, 1000 + MIC_HOLD_MS + 500);
    expect(up.intent).toBe("none");
    expect(up.state).toEqual(MIC_HOLD_IDLE);
  });
});

describe("auto-repeat (holding M fires repeated keydowns)", () => {
  it("a press while a gesture is in flight is absorbed and keeps the ORIGINAL clock", () => {
    const first = micDown(MIC_HOLD_IDLE, { armed: false, now: 1000 });
    const repeat = micDown(first.state, { armed: true, now: 1200 });
    expect(repeat.intent).toBe("none");
    expect(repeat.state).toEqual(first.state); // pressedAt 1000 survives
    // …so the threshold still measures from the FIRST press: this is a hold.
    expect(micUp(repeat.state, 1000 + MIC_HOLD_MS).intent).toBe("disarm");
  });
});

describe("stray releases and window loss (a hold must never wedge)", () => {
  it("keyup with no gesture in flight (press suppressed while typing) no-ops", () => {
    expect(micUp(MIC_HOLD_IDLE, 5000)).toEqual({
      state: MIC_HOLD_IDLE,
      intent: "none",
    });
  });

  it("blur mid-gesture from disarmed disarms — the hot mic THIS gesture opened must close", () => {
    const down = micDown(MIC_HOLD_IDLE, { armed: false, now: 1000 });
    expect(micBlur(down.state)).toEqual({ state: MIC_HOLD_IDLE, intent: "disarm" });
  });

  it("blur mid-gesture from armed leaves the mic alone — that state predates the press", () => {
    const down = micDown(MIC_HOLD_IDLE, { armed: true, now: 1000 });
    expect(micBlur(down.state)).toEqual({ state: MIC_HOLD_IDLE, intent: "none" });
  });

  it("blur with no gesture in flight no-ops", () => {
    expect(micBlur(MIC_HOLD_IDLE)).toEqual({ state: MIC_HOLD_IDLE, intent: "none" });
  });
});
