/**
 * Affinity cache (logic/affinitycache.ts) — the keyed memo behind skipping a
 * re-run of the `topic_affinities` backend embed+scan when the same (topic-set,
 * scope, alpha) is viewed again (founder: cache affinities for performance).
 * Pure, so the hit/miss + invalidation contract pins without a backend or DOM.
 */
import { describe, expect, it } from "vitest";
import {
  affinityKey,
  AffinityCache,
  scopeKey,
  type ScopeKeyInput,
} from "../src/lib/logic/affinitycache";

const folder: ScopeKeyInput = {
  kind: "folder",
  root_id: "r1",
  folder: "iceland",
};
const collection: ScopeKeyInput = { kind: "collection", id: "c1" };
const library: ScopeKeyInput = { kind: "library" };

describe("scopeKey — stable, change-sensitive scope identity", () => {
  it("maps each scope kind to a distinct stable key", () => {
    expect(scopeKey(folder)).toBe("folder:r1/iceland");
    expect(scopeKey(collection)).toBe("collection:c1");
    expect(scopeKey(library)).toBe("library");
  });

  it("a different folder/collection is a different key", () => {
    expect(
      scopeKey({ kind: "folder", root_id: "r1", folder: "spain" }),
    ).not.toBe(scopeKey(folder));
    expect(scopeKey({ kind: "collection", id: "c2" })).not.toBe(
      scopeKey(collection),
    );
  });
});

describe("affinityKey — order-insensitive over the topic SET", () => {
  it("the same topic set in any order keys identically", () => {
    expect(affinityKey(["harbor", "dusk"], folder, 0.5)).toBe(
      affinityKey(["dusk", "harbor"], folder, 0.5),
    );
  });

  it("a different set, scope, or alpha keys differently", () => {
    const base = affinityKey(["harbor"], folder, 0.5);
    expect(affinityKey(["harbor", "dusk"], folder, 0.5)).not.toBe(base);
    expect(affinityKey(["harbor"], collection, 0.5)).not.toBe(base);
    expect(affinityKey(["harbor"], folder, 0.6)).not.toBe(base);
  });

  it("float jitter on alpha collapses to the same bucket (rounded)", () => {
    expect(affinityKey(["harbor"], folder, 0.5)).toBe(
      affinityKey(["harbor"], folder, 0.5000001),
    );
  });
});

describe("AffinityCache — hit / miss / invalidation", () => {
  it("misses on a cold key, hits after a set (same set, scope, alpha)", () => {
    const cache = new AffinityCache<string>();
    expect(cache.get(["harbor"], folder, 0.5)).toBeUndefined();
    cache.set(["harbor"], folder, 0.5, "REPORT-A");
    expect(cache.get(["harbor"], folder, 0.5)).toBe("REPORT-A");
  });

  it("a re-added (then undone) topic set is a HIT (the try/undo loop)", () => {
    const cache = new AffinityCache<string>();
    cache.set(["harbor"], folder, 0.5, "ONE");
    cache.set(["harbor", "dusk"], folder, 0.5, "TWO");
    // Undo the add: back to the single-topic set -> still cached.
    expect(cache.get(["harbor"], folder, 0.5)).toBe("ONE");
    // Re-add -> still cached, no re-fetch.
    expect(cache.get(["harbor", "dusk"], folder, 0.5)).toBe("TWO");
  });

  it("a SCOPE change invalidates the whole cache (affinities are scope-relative)", () => {
    const cache = new AffinityCache<string>();
    cache.set(["harbor"], folder, 0.5, "FOLDER");
    // Same topics + alpha, different scope: a miss, and the prior generation is
    // wiped so it cannot leak back.
    expect(cache.get(["harbor"], collection, 0.5)).toBeUndefined();
    cache.set(["harbor"], collection, 0.5, "COLLECTION");
    expect(cache.get(["harbor"], collection, 0.5)).toBe("COLLECTION");
    // Returning to the folder scope is a fresh miss (the folder entry was wiped).
    expect(cache.get(["harbor"], folder, 0.5)).toBeUndefined();
  });

  it("an ALPHA change invalidates the whole cache (every row re-blends)", () => {
    const cache = new AffinityCache<string>();
    cache.set(["harbor"], folder, 0.5, "BLEND-50");
    expect(cache.get(["harbor"], folder, 0.8)).toBeUndefined();
    cache.set(["harbor"], folder, 0.8, "BLEND-80");
    expect(cache.get(["harbor"], folder, 0.8)).toBe("BLEND-80");
    // The 0.5 entry was wiped by the alpha change.
    expect(cache.get(["harbor"], folder, 0.5)).toBeUndefined();
  });

  it("clear() drops entries but keeps the generation (no needless re-clear)", () => {
    const cache = new AffinityCache<string>();
    cache.set(["harbor"], folder, 0.5, "A");
    cache.clear();
    expect(cache.size).toBe(0);
    cache.set(["harbor"], folder, 0.5, "B");
    expect(cache.get(["harbor"], folder, 0.5)).toBe("B");
  });

  it("delete() evicts ONE entry, leaving the generation's others intact (self-heal)", () => {
    const cache = new AffinityCache<string>();
    cache.set(["harbor"], folder, 0.5, "DEGRADED");
    cache.set(["harbor", "dusk"], folder, 0.5, "GOOD");
    // The self-heal evicts only the stale (embedder-not-ready) report so the
    // next recompute refetches it; the sibling entry must survive.
    cache.delete(["harbor"], folder, 0.5);
    expect(cache.get(["harbor"], folder, 0.5)).toBeUndefined();
    expect(cache.get(["harbor", "dusk"], folder, 0.5)).toBe("GOOD");
  });

  it("delete() is a no-op on a missing key (idempotent)", () => {
    const cache = new AffinityCache<string>();
    cache.set(["harbor"], folder, 0.5, "A");
    cache.delete(["nope"], folder, 0.5);
    expect(cache.size).toBe(1);
    expect(cache.get(["harbor"], folder, 0.5)).toBe("A");
  });
});
