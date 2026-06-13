/**
 * Stage B registry rows (actions/defs/look.ts) through the keymap
 * interpreter. The pre-existing Look block in keymap.test.ts (Z, ±,
 * Ctrl+0/1, F, reserved P/E/V/O) is never edited; this file carries the
 * NEW rows and the Stage B gating residue. The Space-at-fit close rows
 * are GONE (June 12 2026: Space is the microphone key; Esc is the only
 * keyboard close) — the residue block below pins that down.
 */
import { describe, expect, it } from "vitest";
import { dispatch, type KeyContext, type KeyInput } from "../src/lib/logic/keymap";

const look: KeyContext = {
  viewMode: "look",
  searchOpen: false,
  inputFocused: false,
  searchInputFocused: false,
  hasSelection: false,
  railOpen: false,
  debugEnabled: false,
  asrReady: false,
};

const key = (key: string, mods: Partial<KeyInput> = {}): KeyInput => ({
  key,
  ctrlOrMeta: false,
  shift: false,
  ...mods,
});

describe("R — flip the displayed stack member (featureset §5)", () => {
  it("dispatches flip-stack-member in Look (case-insensitive)", () => {
    expect(dispatch(key("r"), look)).toEqual({ kind: "flip-stack-member" });
    expect(dispatch(key("R", { shift: true }), look)).toEqual({
      kind: "flip-stack-member",
    });
  });

  it("stays live on a lone image (the slice no-ops; activeIsPair tracks the GRID and goes stale across ←/→)", () => {
    expect(dispatch(key("r"), { ...look, activeIsPair: false })).toEqual({
      kind: "flip-stack-member",
    });
  });

  it("is suppressed while a text input is focused (§11)", () => {
    expect(dispatch(key("r"), { ...look, inputFocused: true })).toBeNull();
  });

  it("does not fire on the grid surface from this row (Grid's R is Stage A's)", () => {
    // Scope eligibility only — no assertion about what Grid binds to "r".
    const action = dispatch(key("r"), { ...look, viewMode: "grid" });
    expect(action === null || action.kind === "flip-stack-member").toBe(true);
    if (action !== null) expect(action.kind).not.toBe("look-nav");
  });
});

describe("rail-focus gating (enabled lives on the defs, not in components)", () => {
  const railFocused = { ...look, railOpen: true, railFocused: true };

  it.each(["z", "f", "r"])("bare key %j yields to the rail", (k) => {
    expect(dispatch(key(k), railFocused)).toBeNull();
  });

  it("←/→ route to the rail's owner, not look-nav, while the rail has focus", () => {
    const action = dispatch(key("ArrowLeft"), railFocused);
    expect(action?.kind).not.toBe("look-nav");
  });
});

describe("zoom chord residue", () => {
  it("Shift+= dispatches zoom-step in (the layouts where + needs Shift)", () => {
    expect(dispatch(key("=", { shift: true }), look)).toEqual({
      kind: "zoom-step",
      delta: 1,
    });
  });

  it("zoom keys are suppressed while typing a note over Look (§11)", () => {
    const typing = { ...look, inputFocused: true };
    expect(dispatch(key("z"), typing)).toBeNull();
    expect(dispatch(key("-"), typing)).toBeNull();
  });

  it("Z does not exist on the grid surface (scope eligibility)", () => {
    expect(dispatch(key("z"), { ...look, viewMode: "grid" })).toBeNull();
  });
});

describe("Space in Look is the microphone, never close (June 12 2026 ruling)", () => {
  // The triple-role Space (close at fit / pan zoomed / pencil pan) is
  // retired wholesale: the look-close row and the looknav.ts hold machine
  // are deleted, and Space belongs to the global mic-press row.
  it("Space dispatches the mic when ASR is ready, nothing otherwise", () => {
    expect(dispatch(key(" "), look)).toBeNull();
    expect(dispatch(key(" "), { ...look, asrReady: true })).toEqual({
      kind: "mic-press",
    });
  });

  it("the mic does NOT yield to rail focus — a Zoom call's mute key works anywhere", () => {
    // railFocused does not gate the GLOBAL mic row; only typing
    // suppresses it (§11).
    expect(
      dispatch(key(" "), { ...look, asrReady: true, railOpen: true, railFocused: true }),
    ).toEqual({ kind: "mic-press" });
  });

  it("Escape still closes regardless of zoom (Esc is sacred, §0)", () => {
    expect(dispatch(key("Escape"), look)).toEqual({ kind: "escape" });
  });
});
