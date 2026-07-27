/**
 * Grid decode tier by display size (AUDIT 2026-07-07 F3, T3c). CONTRACT
 * PINNED: ipc/urls.ts tierForCell maps the two smallest THUMB_STEPS
 * (96/160 targets) to the 96px micro artifact and everything larger to
 * the 512px thumb, with the cutoff at the 160/240 midpoint so the
 * integer-column snap's stretch of a 160-target cell (toward ~200px)
 * keeps one tier per slider step at grid-like widths. Thumb.svelte fetches
 * previewTierUrl(hash, size); a regression back to always-thumb re-opens
 * the ~28x decoded-pixel overshoot at zoomed-out sizes.
 */
import { describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/svelte";
import { tick } from "svelte";

// Same inert-IPC harness as thumb-ping.test.ts: Thumb pulls `ui` (app
// state), whose IPC layer must not reach a real backend under jsdom.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import Thumb from "../src/lib/components/grid/Thumb.svelte";
import {
  MICRO_TIER_MAX_CELL_PX,
  microUrl,
  previewTierUrl,
  srcHash,
  thumbUrl,
  tierForCell,
} from "../src/lib/ipc/urls";
import { snap } from "../src/lib/logic/gridlayout";
import { THUMB_STEPS } from "../src/lib/logic/sort";

const HASH = "ab".repeat(32);

describe("tierForCell — display size picks the decode tier", () => {
  it("the two smallest THUMB_STEPS are micro; all larger steps are thumb", () => {
    expect(tierForCell(THUMB_STEPS[0])).toBe("micro"); // 96
    expect(tierForCell(THUMB_STEPS[1])).toBe("micro"); // 160
    for (const step of THUMB_STEPS.slice(2)) {
      expect(tierForCell(step), `step ${step}`).toBe("thumb");
    }
  });

  it("the cutoff sits between the 160 and 240 steps", () => {
    expect(MICRO_TIER_MAX_CELL_PX).toBeGreaterThan(THUMB_STEPS[1]);
    expect(MICRO_TIER_MAX_CELL_PX).toBeLessThan(THUMB_STEPS[2]);
    expect(tierForCell(MICRO_TIER_MAX_CELL_PX)).toBe("micro");
    expect(tierForCell(MICRO_TIER_MAX_CELL_PX + 0.01)).toBe("thumb");
  });

  it("column-snap stretch never flips the tier within a slider step at grid-like widths", () => {
    // The grid passes the SNAPPED cell size (geom.cell), not the raw step
    // target — sweep container widths and assert every snapped cell for a
    // 96/160 target still maps micro, and for 240+ still thumb. Scoped to
    // >= 4 columns: the snap's worst stretch is ~(step/2 + gap)/cols, so
    // in a degenerate 2-3 column sliver a 160-target can render at 202px
    // (or a 240-target at 198px) and the tier follows the ACTUAL size —
    // which is the contract: tier tracks what is really on screen.
    for (let w = 480; w <= 2560; w += 61) {
      for (const [i, target] of THUMB_STEPS.entries()) {
        const g = snap(w, target, 8, 10);
        if (g.cols < 4) continue;
        expect(tierForCell(g.cell), `w=${w} target=${target} cell=${g.cell}`).toBe(
          i <= 1 ? "micro" : "thumb",
        );
      }
    }
  });
});

describe("previewTierUrl — the URL Thumb.svelte fetches", () => {
  it("routes micro sizes to /micro and larger to /thumb", () => {
    expect(previewTierUrl(HASH, 96)).toBe(microUrl(HASH));
    expect(previewTierUrl(HASH, 160)).toBe(microUrl(HASH));
    expect(previewTierUrl(HASH, 240)).toBe(thumbUrl(HASH));
    expect(previewTierUrl(HASH, 512)).toBe(thumbUrl(HASH));
  });

  it("both tiers keep the hash as the last path segment (srcHash recycled-img guard)", () => {
    // Thumb's stale-pixel guard compares srcHash(currentSrc) === hash; a
    // tier switch on the SAME hash must keep matching so the loaded
    // bitmap holds through a zoom-boundary flip with no flash of empty.
    expect(srcHash(previewTierUrl(HASH, 96))).toBe(HASH);
    expect(srcHash(previewTierUrl(HASH, 320))).toBe(HASH);
    expect(srcHash(`${previewTierUrl(HASH, 96)}?r=2&p=5`)).toBe(HASH);
  });
});

// ---- rendered Thumb: the micro-miss fallback + heal (F3's 404 story) -------

// previewReady only promises the THUMB artifact exists; the micro tier may
// not be regenerated yet. Pin the component's answer: a micro 404 falls back
// to the thumb tier at once (no retry-budget burn), and a previews-changed
// ping naming the hash returns the cell to the micro tier with a novel URL.
function props(over: Record<string, unknown> = {}) {
  return {
    hash: HASH,
    previewReady: true,
    previewPing: { seq: 0, hashes: new Set<string>() },
    hasJournal: false,
    offline: false,
    stack: "solo" as const,
    cellInfo: "none" as const,
    fileName: "a.jpg",
    rating: null,
    selected: false,
    active: false,
    size: 128, // micro-tier cell
    infoStrip: 0,
    intensity: 0,
    onpointerselect: () => {},
    onopen: () => {},
    onstacktoggle: () => {},
    oncontextmenu: () => {},
    ...over,
  };
}

function imgEl(container: HTMLElement): HTMLImageElement {
  const img = container.querySelector("img");
  expect(img).not.toBeNull();
  return img as HTMLImageElement;
}

describe("Thumb at a micro-tier size (rendered)", () => {
  it("requests the micro artifact, not the 512px thumb", () => {
    const { container } = render(Thumb, props());
    expect(imgEl(container).getAttribute("src")).toBe(microUrl(HASH));
  });

  it("a micro 404 falls back to the thumb tier immediately, then a ping heals it back", async () => {
    const { container, rerender } = render(Thumb, props());
    const img = imgEl(container);

    // The micro artifact is missing (regen not run yet): the protocol
    // answers 404 and the <img> errors.
    img.dispatchEvent(new Event("error"));
    await tick();
    expect(img.getAttribute("src")).toBe(thumbUrl(HASH));

    // The regen lands and pings this hash: back to the size-appropriate
    // micro tier, on a NOVEL (?p=) URL past the immutable cache.
    await rerender(props({ previewPing: { seq: 6, hashes: new Set([HASH]) } }));
    expect(img.getAttribute("src")).toBe(`${microUrl(HASH)}?p=6`);
  });

  it("above the cutoff the same cell asks for the thumb tier", () => {
    const { container } = render(Thumb, props({ size: 320 }));
    expect(imgEl(container).getAttribute("src")).toBe(thumbUrl(HASH));
  });

  it("keeps the true viewport eager/high and mounted overscan lazy/low", () => {
    const visible = render(Thumb, props({ highPriority: true }));
    expect(imgEl(visible.container).getAttribute("loading")).toBe("eager");
    expect(imgEl(visible.container).getAttribute("fetchpriority")).toBe("high");

    const overscan = render(Thumb, props({ highPriority: false }));
    expect(imgEl(overscan.container).getAttribute("loading")).toBe("lazy");
    expect(imgEl(overscan.container).getAttribute("fetchpriority")).toBe("low");
  });
});
