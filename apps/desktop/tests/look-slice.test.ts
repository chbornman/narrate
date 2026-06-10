/**
 * Look slice (state/look.svelte.ts, contract frozen by FOUNDATIONS):
 * session lifecycle (entry = Fit, zoom PERSISTS across ←/→ — featureset
 * §2/§8 "zoom never resets per image"), member flips, the displayed-first
 * write-scope targets (a collapsed pair is 1 cell but 2 targets — the
 * indicator truthfully reads "● 2"; DECISIONS 4), and filmstrip
 * persistence (default hidden).
 */
import { beforeEach, describe, expect, it } from "vitest";
import { LookSlice } from "../src/lib/state/look.svelte";
import type { LookEntry } from "../src/lib/types/display";

const ENTRIES: LookEntry[] = [
  { display: "a", alt: null },
  { display: "c", alt: "c-raw" }, // a collapsed RAW+JPEG pair
  { display: "e", alt: null },
];

let look: LookSlice;
beforeEach(() => {
  localStorage.clear();
  look = new LookSlice();
});

describe("open — a fresh Look session", () => {
  it("sets the navigation set and resets zoom + flips to the entry defaults", () => {
    look.zoomSession = { mode: "actual", scale: 1, centerFrac: { x: 0.5, y: 0.5 } };
    look.flips = new Set(["c"]);
    const seq = look.zoomCmd.seq;
    look.open(ENTRIES, 1);
    expect(look.order).toEqual(ENTRIES);
    expect(look.index).toBe(1);
    expect(look.zoomSession).toBeNull(); // default entry = Fit
    expect(look.flips.size).toBe(0); // flips never carry across sessions
    expect(look.atFit).toBe(true);
    expect(look.zoomCmd).toEqual({ seq: seq + 1, op: "fit" });
  });
});

describe("←/→ within the navigation set", () => {
  it("moves within bounds and refuses the edges", () => {
    look.open(ENTRIES, 1);
    expect(look.next(1)).toBe(true);
    expect(look.index).toBe(2);
    expect(look.next(1)).toBe(false); // end: stays
    expect(look.index).toBe(2);
    look.index = 0;
    expect(look.next(-1)).toBe(false);
  });

  it("NEVER resets the zoom session — persistence is the point (featureset §2)", () => {
    look.open(ENTRIES, 0);
    const session = { mode: "actual" as const, scale: 1, centerFrac: { x: 0.7, y: 0.3 } };
    look.zoomSession = session;
    look.next(1);
    expect(look.zoomSession).toEqual(session); // carryOver happens in the stage
    look.next(1);
    expect(look.zoomSession).toEqual(session);
  });
});

describe("R member flips", () => {
  it("flips the displayed hash of a pair and back", () => {
    look.open(ENTRIES, 1);
    expect(look.currentHash).toBe("c");
    look.flipMember();
    expect(look.currentHash).toBe("c-raw");
    look.flipMember();
    expect(look.currentHash).toBe("c");
  });

  it("no-ops on a lone image", () => {
    look.open(ENTRIES, 0);
    look.flipMember();
    expect(look.currentHash).toBe("a");
    expect(look.flips.size).toBe(0);
  });
});

describe("write-scope targets (the '● 2' truth — DECISIONS 4)", () => {
  it("a pair entry reports BOTH hashes, displayed member first", () => {
    look.open(ENTRIES, 1);
    expect(look.currentTargets).toEqual(["c", "c-raw"]);
  });

  it("a flip reorders the targets (displayed first stays truthful)", () => {
    look.open(ENTRIES, 1);
    look.flipMember();
    expect(look.currentTargets).toEqual(["c-raw", "c"]);
  });

  it("a lone entry reports one hash; no entry reports none", () => {
    look.open(ENTRIES, 0);
    expect(look.currentTargets).toEqual(["a"]);
    look.close();
    expect(look.currentTargets).toEqual([]);
  });
});

describe("filmstrip (F, featureset §2: default hidden)", () => {
  it("defaults hidden and persists the toggle", () => {
    look.loadPrefs();
    expect(look.filmstrip).toBe(false);
    look.toggleFilmstrip();
    expect(look.filmstrip).toBe(true);
    const fresh = new LookSlice();
    fresh.loadPrefs();
    expect(fresh.filmstrip).toBe(true);
  });
});

describe("close", () => {
  it("clears the session state", () => {
    look.open(ENTRIES, 2);
    look.zoomSession = { mode: "free", scale: 2, centerFrac: { x: 0.5, y: 0.5 } };
    look.close();
    expect(look.index).toBe(-1);
    expect(look.order).toEqual([]);
    expect(look.zoomSession).toBeNull();
    expect(look.current).toBeNull();
    expect(look.currentHash).toBeNull();
  });
});

describe("zoom command channel (keyboard → stage)", () => {
  it("bumps the sequence and carries op/delta", () => {
    const seq = look.zoomCmd.seq;
    look.sendZoom("step", -1);
    expect(look.zoomCmd).toEqual({ seq: seq + 1, op: "step", delta: -1 });
    look.sendZoom("toggle");
    expect(look.zoomCmd.seq).toBe(seq + 2);
    expect(look.zoomCmd.op).toBe("toggle");
  });
});
