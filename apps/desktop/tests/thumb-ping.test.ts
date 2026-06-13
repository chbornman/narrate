/**
 * Thumb cache-bust on `previews-changed` (founder dogfood, June 2026).
 *
 * The `photoproof://` protocol serves content-addressed artifacts with an
 * `immutable` cache header, so after a clear deletes the bytes the webview
 * keeps painting its cached copy for the SAME stable URL until restart. The
 * only live cache-bust is the `?p=<seq>` query param Thumb appends when a
 * `previews-changed` ping applies to it. These tests prove:
 *   (a) a per-hash ping bumps `?p=` ONLY on the named cell (the heal path);
 *   (b) a GLOBAL ping (empty `hashes` — the manual cache clear's signal)
 *       bumps `?p=` on EVERY cell, even one already loaded.
 */
import { describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/svelte";

// Thumb pulls `ui` (app state) for the signal-hint popover gate; the IPC
// layer it reaches must not touch a real backend under jsdom.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import Thumb from "../src/lib/components/grid/Thumb.svelte";
import { thumbUrl } from "../src/lib/ipc/urls";

const HASH = "ab".repeat(32);

// The full prop surface a grid cell needs; the test only varies previewReady
// and previewPing, so everything else is an inert default.
function props(over: Record<string, unknown> = {}) {
  return {
    hash: HASH,
    previewReady: false,
    previewPing: { seq: 0, hashes: new Set<string>() },
    hasJournal: false,
    offline: false,
    stack: "solo" as const,
    cellInfo: "none" as const,
    fileName: "a.jpg",
    rating: null,
    selected: false,
    active: false,
    size: 128,
    infoStrip: 0,
    intensity: 0,
    onpointerselect: () => {},
    onopen: () => {},
    onstacktoggle: () => {},
    oncontextmenu: () => {},
    ...over,
  };
}

function imgSrc(container: HTMLElement): string | null {
  return container.querySelector("img")?.getAttribute("src") ?? null;
}

describe("Thumb previews-changed cache-bust", () => {
  it("a GLOBAL ping (empty hashes) bumps ?p= so the cell re-requests", async () => {
    // previewReady=false: the <img> stays unmounted until the global ping
    // flips it ready AND bumps the cache-bust param — exactly the clear path.
    const { container, rerender } = render(Thumb, props());
    expect(container.querySelector("img")).toBeNull();

    await rerender(props({ previewPing: { seq: 5, hashes: new Set<string>() } }));
    const src = imgSrc(container);
    expect(src).not.toBeNull();
    expect(src).toBe(`${thumbUrl(HASH)}?p=5`);
  });

  it("a per-hash ping bumps ?p= only when it names this cell", async () => {
    const { container, rerender } = render(
      Thumb,
      props({ previewReady: true }),
    );
    // Bare URL until a ping applies.
    expect(imgSrc(container)).toBe(thumbUrl(HASH));

    // A ping for a DIFFERENT hash must not touch this cell.
    await rerender(
      props({
        previewReady: true,
        previewPing: { seq: 3, hashes: new Set(["cd".repeat(32)]) },
      }),
    );
    expect(imgSrc(container)).toBe(thumbUrl(HASH));

    // A ping naming THIS hash bumps the param.
    await rerender(
      props({
        previewReady: true,
        previewPing: { seq: 4, hashes: new Set([HASH]) },
      }),
    );
    expect(imgSrc(container)).toBe(`${thumbUrl(HASH)}?p=4`);
  });
});
