/**
 * srcHash (ipc/urls.ts) — the recycled-<img> guard's pure piece (BACKLOG:
 * pixel flash). Thumb.svelte marks `loadedHash` only when the element's
 * currentSrc actually names the cell's hash: on a recycled pool slot,
 * complete/naturalWidth (and an in-flight load event) can still describe
 * the PREVIOUS occupant's bitmap, and an unguarded mark flashed the old
 * pixels for a frame on fast scroll.
 */
import { describe, expect, it } from "vitest";
import { fullDecodeUrl, srcHash, thumbUrl } from "../src/lib/ipc/urls";

const HASH = "ab".repeat(32);
const OTHER = "cd".repeat(32);

describe("srcHash — which hash a preview URL names", () => {
  it("round-trips the bare thumb URL", () => {
    expect(srcHash(thumbUrl(HASH))).toBe(HASH);
  });

  it("strips the retry/ping cache-busting query (Thumb's r= and p= params)", () => {
    expect(srcHash(`${thumbUrl(HASH)}?r=3`)).toBe(HASH);
    expect(srcHash(`${thumbUrl(HASH)}?r=3&p=7`)).toBe(HASH);
  });

  it("strips the full-decode cache-bust token (LookStage's ?v= retry)", () => {
    // The /full-decode rung re-fetches past the immutable-cached 404 by
    // bumping ?v=; srcHash must still resolve the bare hash.
    expect(srcHash(`${fullDecodeUrl(HASH)}?v=3`)).toBe(HASH);
  });

  it("distinguishes the previous occupant's URL from the new hash", () => {
    // The recycled-slot case: the element still holds the OLD src.
    expect(srcHash(thumbUrl(OTHER))).not.toBe(HASH);
  });

  it("an empty currentSrc (nothing loaded yet) never matches a real hash", () => {
    expect(srcHash("")).toBe("");
  });
});
