/**
 * Look navigation (logic/looknav.ts): navigation set = entry selection
 * (featureset §2), R member flips over entries (featureset §5), and the
 * EXHAUSTIVE Space tap-vs-hold table — the packet's most fragile
 * interaction (Appendix B): tap closes at fit (registry row, not this
 * machine), hold pans while zoomed, clean tap closes while zoomed.
 */
import { describe, expect, it } from "vitest";
import {
  SPACE_IDLE,
  displayedHash,
  navigationSet,
  spaceDown,
  spaceHeldNext,
  spacePanned,
  spaceUp,
  toEntry,
  toggleFlip,
} from "../src/lib/logic/looknav";
import type { DisplayUnit } from "../src/lib/types/display";
import type { GridItem } from "../src/lib/types/dto";

const item = (hash: string): GridItem => ({
  hash,
  fileName: `${hash}.jpg`,
  relPath: `${hash}.jpg`,
  captureTs: null,
  addedTs: "2026-02-01T00:00:00Z",
  hasJournal: false,
  offline: false,
  rating: null,
});

const unit = (primary: string, alt: string | null = null): DisplayUnit => ({
  primary: item(primary),
  alt: alt === null ? null : item(alt),
});

// Five grid cells; "c" is a collapsed pair (JPEG displayed, RAW hidden).
const UNITS = [unit("a"), unit("b"), unit("c", "c-raw"), unit("d"), unit("e")];

describe("toEntry (the frozen DisplayUnit → LookEntry seam)", () => {
  it("maps a lone unit and a pair", () => {
    expect(toEntry(unit("a"))).toEqual({ display: "a", alt: null });
    expect(toEntry(unit("c", "c-raw"))).toEqual({ display: "c", alt: "c-raw" });
  });
});

describe("navigationSet — the set is the entry selection (featureset §2)", () => {
  it("multi-selection: cycles within it, in GRID order regardless of click order", () => {
    const nav = navigationSet(UNITS, ["d", "b"], "b"); // clicked d first
    expect(nav?.order.map((e) => e.display)).toEqual(["b", "d"]);
    expect(nav?.index).toBe(0);
    expect(navigationSet(UNITS, ["d", "b"], "d")?.index).toBe(1);
  });

  it("single-image entry cycles the whole folder", () => {
    const nav = navigationSet(UNITS, ["b"], "b");
    expect(nav?.order.map((e) => e.display)).toEqual(["a", "b", "c", "d", "e"]);
    expect(nav?.index).toBe(1);
  });

  it("no selection cycles the folder", () => {
    expect(navigationSet(UNITS, [], "c")?.index).toBe(2);
    expect(navigationSet(UNITS, [], "c")?.order).toHaveLength(5);
  });

  it("entering OUTSIDE the selection cycles the folder (scope narrowed to the viewed image anyway — CAPTURE §3)", () => {
    const nav = navigationSet(UNITS, ["b", "d"], "e");
    expect(nav?.order).toHaveLength(5);
    expect(nav?.index).toBe(4);
  });

  it("pair units carry their alt into the entry (R flips it later)", () => {
    const nav = navigationSet(UNITS, ["b", "c"], "c");
    expect(nav?.order).toEqual([
      { display: "b", alt: null },
      { display: "c", alt: "c-raw" },
    ]);
  });

  it("stale selection hashes are ignored; an unknown entry is null", () => {
    const nav = navigationSet(UNITS, ["b", "gone", "d"], "d");
    expect(nav?.order.map((e) => e.display)).toEqual(["b", "d"]);
    expect(navigationSet(UNITS, [], "gone")).toBeNull();
  });
});

describe("member flips (R)", () => {
  const pair = { display: "c", alt: "c-raw" };
  const lone = { display: "a", alt: null };

  it("flip shows the alt; flip again restores; the input set is untouched", () => {
    const empty: ReadonlySet<string> = new Set();
    const flipped = toggleFlip(empty, pair);
    expect(displayedHash(pair, flipped)).toBe("c-raw");
    expect(displayedHash(pair, toggleFlip(flipped, pair))).toBe("c");
    expect(empty.size).toBe(0); // immutable in, new set out
  });

  it("lone images no-op quietly", () => {
    const flips: ReadonlySet<string> = new Set();
    expect(toggleFlip(flips, lone)).toBe(flips);
    expect(displayedHash(lone, new Set(["a"]))).toBe("a"); // alt-less: never flips
  });
});

// ---------------------------------------------------------------------------
// Space tap-vs-hold — exhaustive over the triple role (Appendix B)
// ---------------------------------------------------------------------------

const fresh = { atFit: false, repeat: false };
const pencilOff = { pencilOn: false };
const pencilOn = { pencilOn: true };

describe("Space at fit", () => {
  it("never engages the machine — the registry's look-close row owns the tap", () => {
    const s = spaceDown(SPACE_IDLE, { atFit: true, repeat: false });
    expect(s.held).toBe(false);
    expect(spaceUp(s, pencilOff).outcome).toBe("none");
  });
});

describe("Space while zoomed — clean tap closes", () => {
  it("down engages the hold; an untouched keyup closes", () => {
    const held = spaceDown(SPACE_IDLE, fresh);
    expect(held).toEqual({ held: true, panned: false });
    const { state, outcome } = spaceUp(held, pencilOff);
    expect(outcome).toBe("close");
    expect(state).toEqual(SPACE_IDLE);
  });
});

describe("Space while zoomed — hold pans", () => {
  it("a pan during the hold makes the keyup a no-op", () => {
    let s = spaceDown(SPACE_IDLE, fresh);
    s = spacePanned(s);
    expect(s).toEqual({ held: true, panned: true });
    const { state, outcome } = spaceUp(s, pencilOff);
    expect(outcome).toBe("none");
    expect(state).toEqual(SPACE_IDLE);
  });

  it("repeated pans are idempotent", () => {
    let s = spacePanned(spaceDown(SPACE_IDLE, fresh));
    const again = spacePanned(s);
    expect(again).toEqual(s);
  });
});

describe("Space auto-repeat", () => {
  it("repeats during a hold preserve the panned flag (no surprise close)", () => {
    let s = spacePanned(spaceDown(SPACE_IDLE, fresh));
    s = spaceDown(s, { atFit: false, repeat: true }); // OS key repeat
    expect(s.panned).toBe(true);
    expect(spaceUp(s, pencilOff).outcome).toBe("none");
  });

  it("a repeat arriving on idle never engages — its keyup can never close", () => {
    // The hold began before Look had the key (e.g. mid-hold surface
    // change): treating it as a fresh tap would close on release.
    const s = spaceDown(SPACE_IDLE, { atFit: false, repeat: true });
    expect(s.held).toBe(false);
    expect(spaceUp(s, pencilOff).outcome).toBe("none");
  });
});

describe("Space stray events", () => {
  it("keyup on idle is a no-op", () => {
    const { state, outcome } = spaceUp(SPACE_IDLE, pencilOff);
    expect(outcome).toBe("none");
    expect(state).toBe(SPACE_IDLE);
  });

  it("a pan with no hold engaged changes nothing", () => {
    expect(spacePanned(SPACE_IDLE)).toBe(SPACE_IDLE);
  });

  it("a second non-repeat down while held (missed keyup) keeps the hold", () => {
    const held = spacePanned(spaceDown(SPACE_IDLE, fresh));
    expect(spaceDown(held, fresh)).toBe(held);
  });
});

describe("Space across cycles", () => {
  it("the machine resets cleanly between holds", () => {
    // Cycle 1: clean tap → close.
    let r = spaceUp(spaceDown(SPACE_IDLE, fresh), pencilOff);
    expect(r.outcome).toBe("close");
    // Cycle 2 from the returned state: panned hold → none.
    r = spaceUp(spacePanned(spaceDown(r.state, fresh)), pencilOff);
    expect(r.outcome).toBe("none");
    // Cycle 3: clean again → close.
    expect(spaceUp(spaceDown(r.state, fresh), pencilOff).outcome).toBe("close");
  });
});

describe("spaceHeldNext — the shared Space-held slice fact (one tracker, ownership-gated)", () => {
  // LookStage is the ONLY writer (through its stageOwnsRawKeys gate);
  // PencilOverlay consumes ui.look.spaceHeld to yield the pointer. Both
  // raw pipelines therefore share one gate and CANNOT desync.

  it("engages only while the stage owns the raw keys — exactly like the pan machine", () => {
    expect(spaceHeldNext(false, "down", true)).toBe(true);
    // The pan machine engages under the same owned down — agreement is
    // the point (the overlay used to track Space ungated).
    expect(spaceDown(SPACE_IDLE, fresh).held).toBe(true);
  });

  it("a Space under the cheatsheet or an open menu never makes the overlay yield", () => {
    // Stage does not own the keys (cheatsheet/menu/search/rail): the pan
    // machine never sees the down, so the slice fact must stay false too.
    expect(spaceHeldNext(false, "down", false)).toBe(false);
  });

  it("a menu opening MID-hold cannot wedge it: keyup/blur release unconditionally", () => {
    expect(spaceHeldNext(true, "up", false)).toBe(false);
    expect(spaceHeldNext(true, "blur", false)).toBe(false);
    // …while an unowned auto-repeat does not release an engaged hold
    // (release belongs to keyup, mirroring spaceDown's repeat rule).
    expect(spaceHeldNext(true, "down", false)).toBe(true);
  });

  it("stray releases on idle stay idle", () => {
    expect(spaceHeldNext(false, "up", true)).toBe(false);
    expect(spaceHeldNext(false, "blur", false)).toBe(false);
  });
});

describe("Space with the pencil ON (UI §11: Pan — never close)", () => {
  it("a clean zoomed tap resolves to NOTHING — Look stays open", () => {
    // The look-close def's `!ctx.pencilMode` gate covers fit; this is the
    // zoomed raw-pipeline half: a Space tap released without dragging
    // must not destroy pencil mode.
    const { state, outcome } = spaceUp(spaceDown(SPACE_IDLE, fresh), pencilOn);
    expect(outcome).toBe("none");
    expect(state).toEqual(SPACE_IDLE); // resolved, never wedged
  });

  it("mid-pen-down the stage sees no pan, yet the tap still cannot close", () => {
    // During a draw the overlay holds pointer capture, so spacePanned is
    // never reached — without the gate every mid-mark tap looks clean.
    const held = spaceDown(SPACE_IDLE, fresh);
    expect(held.panned).toBe(false); // exactly the dangerous shape
    expect(spaceUp(held, pencilOn).outcome).toBe("none");
  });

  it("a panned hold stays a no-op too; pencil OFF keeps the clean-tap close", () => {
    expect(spaceUp(spacePanned(spaceDown(SPACE_IDLE, fresh)), pencilOn).outcome).toBe(
      "none",
    );
    expect(spaceUp(spaceDown(SPACE_IDLE, fresh), pencilOff).outcome).toBe("close");
  });
});
