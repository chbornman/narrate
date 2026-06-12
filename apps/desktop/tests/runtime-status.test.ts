/**
 * P6.2: the RUNTIME status surfaces — the asrReady/asrUnavailable
 * reconciliation (one coherent ctx story) and the one-time consent card's
 * pure logic (UI §9.1.3 / RUNTIME §10.2–10.3).
 */
import { beforeEach, describe, expect, it } from "vitest";
import { ShellSlice } from "../src/lib/state/shell.svelte";
import { consentCard, formatGb } from "../src/lib/logic/consent";
import type { ModelRowDto, RuntimeStatus } from "../src/lib/types/dto";

const model = (over: Partial<ModelRowDto> = {}): ModelRowDto => ({
  id: "gemma-4-e4b-it-q4_k_m",
  role: "llm",
  state: "not-downloaded",
  totalBytes: 5_460_000_000,
  downloadedBytes: 0,
  licenseName: "Gemma Terms of Use",
  licenseUrl: "https://ai.google.dev/gemma/terms",
  acceptanceRequired: true,
  accepted: false,
  error: null,
  ...over,
});

const status = (over: Partial<RuntimeStatus> = {}): RuntimeStatus => ({
  asrReady: false,
  llmReady: false,
  asrBlocked: null,
  llmBlocked: null,
  clipReady: false,
  textEmbedderReady: false,
  tierDetected: 1,
  tierEffective: 1,
  tierOverriddenAbove: false,
  consent: "undecided",
  consentOfferBytes: 9_460_000_000,
  models: [model(), model({ id: "qwen3-embedding-0.6b-int8", role: "text-embedder", acceptanceRequired: false, totalBytes: 600_000_000 })],
  instanceLockHeld: true,
  ...over,
});

describe("asrReady / asrUnavailable reconciliation (P6.2 obligation)", () => {
  let shell: ShellSlice;
  beforeEach(() => {
    localStorage.clear();
    shell = new ShellSlice();
  });

  it("asrReady defaults false and goes live off the runtime-status channel", () => {
    expect(shell.asrReady).toBe(false);
    shell.onRuntimeStatus(status({ asrReady: true }));
    expect(shell.asrReady).toBe(true);
    expect(shell.runtime?.tierEffective).toBe(1);
    shell.onRuntimeStatus(status({ asrReady: false }));
    expect(shell.asrReady).toBe(false);
  });

  it("the two flags ride separate channels and tell ONE story", () => {
    // Existence gate: runtime-status. Degraded gate: indicator-state.
    shell.onRuntimeStatus(status({ asrReady: true }));
    shell.onIndicatorState({
      currentScope: { kind: "session", count: 0, previewHashes: [] },
      mic: "disarmedError",
      streamingUtterance: null,
      degraded: { asrUnavailable: true },
    });
    // Ready + unavailable = the mic EXISTS but is degraded — the quiet
    // struck-through glyph (UI §7.3); never absence, never a toast.
    expect(shell.asrReady).toBe(true);
    expect(shell.asrUnavailable).toBe(true);
    expect(shell.mic).toBe("disarmedError");
  });
});

describe("the one-time consent card (UI §9.1.3 / RUNTIME §10.2)", () => {
  it("never shows before the first root, after any decision, or below the floor", () => {
    expect(consentCard(null, true)).toBeNull();
    expect(consentCard(status(), false)).toBeNull();
    for (const consent of ["later", "never", "download"]) {
      expect(consentCard(status({ consent }), true)).toBeNull();
    }
    // Tier 0 is whole: nothing offered, no card, no download prompt ever.
    expect(consentCard(status({ tierEffective: 0 }), true)).toBeNull();
    expect(
      consentCard(status({ models: status().models.map((m) => ({ ...m, state: "not-offered" })) }), true),
    ).toBeNull();
  });

  it("shows once with the detected tier, the LIVE byte sum, and the offered licenses", () => {
    const card = consentCard(status(), true);
    expect(card).not.toBeNull();
    expect(card?.tierDetected).toBe(1);
    expect(card?.totalBytes).toBe(9_460_000_000);
    expect(card?.models.map((m) => m.id)).toEqual([
      "gemma-4-e4b-it-q4_k_m",
      "qwen3-embedding-0.6b-int8",
    ]);
  });

  it("Download enables only once every required acceptance is recorded (§13.7)", () => {
    expect(consentCard(status(), true)?.downloadReady).toBe(false);
    const accepted = status({
      models: status().models.map((m) => ({ ...m, accepted: m.acceptanceRequired })),
    });
    expect(consentCard(accepted, true)?.downloadReady).toBe(true);
    // Notice-only licenses (Apache-2.0) never gate.
    const noticeOnly = status({
      models: [model({ acceptanceRequired: false, accepted: false })],
    });
    expect(consentCard(noticeOnly, true)?.downloadReady).toBe(true);
  });

  it("formats the §5.4 budget like the spec copy (~9.5 GB)", () => {
    expect(formatGb(9_460_000_000)).toBe("~9.5 GB");
    expect(formatGb(600_000_000)).toBe("~0.6 GB");
    expect(formatGb(16_000_000_000)).toBe("~16 GB");
  });
});
