/**
 * Full-folder snapshot transform guardrails.
 *
 * These are deliberately the pure, deterministic transforms the grid pays
 * after an IPC folder snapshot arrives: sort, item/scope hash projections,
 * RAW+JPEG stack construction, and unit-hash projection. They do not claim to
 * measure IPC serialization, JSON parsing, Svelte invalidation, DOM work,
 * image decode, GPU upload, or paint. Installed journey receipts cover those.
 *
 * Ordinary CI runs 20k and 100k. The bounded 250k founder tier is:
 *
 *   PHOTOPROOF_SCALE_TIER=founder bunx vitest run \
 *     tests/catalog-snapshot-scale.test.ts
 */
import { describe, expect, it } from "vitest";
import baselines from "../../../tuning-baselines.json";
import { sortItems } from "../src/lib/logic/sort";
import { buildUnits } from "../src/lib/logic/stacks";
import type { GridItem } from "../src/lib/types/dto";

const config = baselines.frontend_catalog_snapshots;
const founderTier = process.env.PHOTOPROOF_SCALE_TIER === "founder";

function p99(samples: number[]): number {
  const ordered = [...samples].sort((a, b) => a - b);
  return ordered[Math.round((ordered.length - 1) * 0.99)];
}

function catalogItems(count: number): GridItem[] {
  return Array.from({ length: count }, (_, index) => {
    const ordinal = index + 1;
    const pair = Math.floor(index / 2);
    const extension = index % 2 === 0 ? "jpg" : "cr3";
    const fileName = `IMG_${pair.toString().padStart(8, "0")}.${extension}`;
    return {
      hash: ordinal.toString(16).padStart(64, "0"),
      fileName,
      relPath: `2025/session-${pair % 32}/${fileName}`,
      rootId: "scale-root",
      captureTs: `2025-${String((index % 12) + 1).padStart(2, "0")}-${String((index % 28) + 1).padStart(2, "0")}T${String(index % 24).padStart(2, "0")}:00:00Z`,
      addedTs: `2026-01-01T00:${String(index % 60).padStart(2, "0")}:00Z`,
      hasJournal: index % 7 === 0,
      rating: index % 6,
      offline: false,
      previewReady: index % 8 !== 0,
    };
  });
}

function transformSnapshot(items: GridItem[]): number {
  const sorted = sortItems(items, "capture-desc");
  const itemHashes = sorted.map((item) => item.hash);
  const scopeHashes = items.map((item) => item.hash);
  const stackModel = buildUnits(sorted, {
    globalCollapsed: true,
    overrides: new Set(),
    flips: new Set(),
    display: "jpeg",
  });
  const unitHashes = stackModel.units.map((unit) => unit.primary.hash);
  return itemHashes.length + scopeHashes.length + unitHashes.length;
}

function measure(
  count: number,
  iterations: number,
  budgetMs: number,
): void {
  const items = catalogItems(count);
  const expectedChecksum = count * 2 + count / 2;

  expect(transformSnapshot(items)).toBe(expectedChecksum);
  const samples: number[] = [];
  let checksum = 0;
  for (let iteration = 0; iteration < iterations; iteration++) {
    const started = performance.now();
    checksum += transformSnapshot(items);
    samples.push(performance.now() - started);
  }

  expect(checksum).toBe(expectedChecksum * iterations);
  const observed = p99(samples);
  expect(
    observed,
    `${count.toLocaleString()}-item snapshot transforms p99 ${observed.toFixed(2)} ms exceeded ${budgetMs} ms`,
  ).toBeLessThan(budgetMs);
}

describe("catalog snapshot scale guardrails", () => {
  it("transforms a 20k full snapshot inside the portable CI ceiling", () => {
    measure(
      config.items_20k,
      config.iterations_20k,
      config.p99_20k_ms,
    );
  });

  it("transforms a 100k full snapshot inside the portable CI ceiling", () => {
    measure(
      config.items_100k,
      config.iterations_100k,
      config.p99_100k_ms,
    );
  });

  it.skipIf(!founderTier)(
    "transforms a 250k full snapshot inside the founder-machine ceiling",
    () => {
      measure(
        config.items_250k,
        config.iterations_250k,
        config.p99_250k_ms,
      );
    },
  );
});
