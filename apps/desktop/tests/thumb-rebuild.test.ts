/**
 * Clear-previews cache-bust + rebuild, the MULTI-cell gaps thumb-ping.test.ts
 * leaves open. That suite proves one rendered cell reacts to a global vs a
 * per-hash ping; here we pin the "Rebuild all previews" grid-wide behavior the
 * design hangs on:
 *   - a GLOBAL ping (empty hashes) bumps `?p=` on EVERY rendered cell at once
 *     (the manual clear / rebuild signal), past the immutable cache;
 *   - a per-hash ping bumps ONLY the named cell among several (the heal path),
 *     leaving its neighbors on their bare URLs;
 *   - a global ping flips an UNREADY cell ready AND a READY cell re-requests
 *     (the immutable-cache bust ignores `loaded` only for the global signal);
 *   - the seq is what makes the URL novel, so a later global ping supersedes an
 *     earlier one's param on the same cell.
 *
 * Same harness as thumb-ping.test.ts: the protocol IPC is inert under jsdom and
 * we read each cell's <img src> back out.
 */
import { describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/svelte";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import Thumb from "../src/lib/components/grid/Thumb.svelte";
import { thumbUrl } from "../src/lib/ipc/urls";

const A = "aa".repeat(32);
const B = "bb".repeat(32);

function props(hash: string, over: Record<string, unknown> = {}) {
  return {
    hash,
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
    // Above the micro-tier cutoff (urls.ts tierForCell) so this suite keeps
    // pinning the THUMB-tier URLs; tier selection has its own suite
    // (thumb-tier.test.ts).
    size: 240,
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

describe("Rebuild all previews: a global ping bumps every cell", () => {
  it("two distinct cells BOTH re-request on one empty-hashes ping", async () => {
    // Two independently-rendered cells (the grid's recycled <img> pool, modeled
    // as two mounts) holding different hashes.
    const cellA = render(Thumb, props(A));
    const cellB = render(Thumb, props(B));
    expect(imgSrc(cellA.container)).toBe(thumbUrl(A));
    expect(imgSrc(cellB.container)).toBe(thumbUrl(B));

    // ONE global ping (the manual clear / rebuild-all signal) — both cells bump
    // their cache-bust param to the SAME seq, each on its own URL.
    const ping = { seq: 7, hashes: new Set<string>() };
    await cellA.rerender(props(A, { previewPing: ping }));
    await cellB.rerender(props(B, { previewPing: ping }));
    expect(imgSrc(cellA.container)).toBe(`${thumbUrl(A)}?p=7`);
    expect(imgSrc(cellB.container)).toBe(`${thumbUrl(B)}?p=7`);
  });

  it("a global ping re-requests an already-LOADED cell (immutable-cache bust)", async () => {
    // previewReady=true so the <img> is mounted with the bare URL from the
    // start (its bytes are "cached" by the immutable protocol header). A global
    // ping must still bump it — the whole reason the param exists.
    const { container, rerender } = render(Thumb, props(A, { previewReady: true }));
    expect(imgSrc(container)).toBe(thumbUrl(A));
    await rerender(props(A, { previewPing: { seq: 2, hashes: new Set<string>() } }));
    expect(imgSrc(container)).toBe(`${thumbUrl(A)}?p=2`);
  });

  it("a global ping flips an UNREADY cell ready and bumps it in one step", async () => {
    // previewReady=false: no <img> until the global ping both flips `ready`
    // (pingSeq > 0) and stamps the cache-bust param.
    const { container, rerender } = render(Thumb, props(A, { previewReady: false }));
    expect(container.querySelector("img")).toBeNull();
    await rerender(
      props(A, { previewReady: false, previewPing: { seq: 4, hashes: new Set<string>() } }),
    );
    expect(imgSrc(container)).toBe(`${thumbUrl(A)}?p=4`);
  });
});

describe("a per-hash ping bumps only its named cell among several", () => {
  it("a ping naming A leaves B untouched", async () => {
    const cellA = render(Thumb, props(A));
    const cellB = render(Thumb, props(B));

    // A ping that names ONLY A (the per-hash heal): A bumps, B stays bare.
    const ping = { seq: 9, hashes: new Set([A]) };
    await cellA.rerender(props(A, { previewPing: ping }));
    await cellB.rerender(props(B, { previewPing: ping }));
    expect(imgSrc(cellA.container)).toBe(`${thumbUrl(A)}?p=9`);
    expect(imgSrc(cellB.container)).toBe(thumbUrl(B));
  });
});

describe("the seq makes the URL novel: a later global ping supersedes", () => {
  it("a second global ping rewrites the param to its newer seq", async () => {
    const { container, rerender } = render(Thumb, props(A));
    await rerender(props(A, { previewPing: { seq: 3, hashes: new Set<string>() } }));
    expect(imgSrc(container)).toBe(`${thumbUrl(A)}?p=3`);
    // A later clear emits a higher seq; the URL must move to it (a stale
    // already-404'd URL would otherwise keep painting the deleted bytes).
    await rerender(props(A, { previewPing: { seq: 11, hashes: new Set<string>() } }));
    expect(imgSrc(container)).toBe(`${thumbUrl(A)}?p=11`);
  });
});
