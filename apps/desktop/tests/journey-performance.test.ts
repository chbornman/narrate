/**
 * A26 deterministic main-journey regression gates.
 *
 * Installed webviews record end-to-end IPC, first-paint, worker, render, and
 * settle timings through the structured performance monitor. These pure
 * headless workloads defend the two CPU transforms beneath those journeys:
 *
 * - the grid's Diversify fold must stay O(images) with O(1) set membership;
 * - the graph's closed-form initial layout must stay O(images x topics).
 *
 * Generous committed p99 ceilings absorb CI/JIT variation while still making
 * an accidental quadratic implementation fail by a wide margin.
 */
import { describe, expect, it } from "vitest";
import baselines from "../../../tuning-baselines.json";
import { filterDiversify } from "../src/lib/logic/diversify";
import {
  ringAnchors,
  type ImageNode,
} from "../src/lib/logic/forcegraph";
import { computeStaticLayout } from "../src/lib/logic/layout";

const config = baselines.frontend_journeys;

function p99(samples: number[]): number {
  const ordered = [...samples].sort((a, b) => a - b);
  return ordered[Math.round((ordered.length - 1) * 0.99)];
}

function hashes(count: number): string[] {
  return Array.from(
    { length: count },
    (_, i) => `${i.toString(16).padStart(8, "0")}${"ab".repeat(28)}`,
  );
}

describe("main frontend journey budgets", () => {
  it("filters a 20k-image grid within the committed p99", () => {
    const all = hashes(config.graph_items).map((hash) => ({ hash }));
    const hidden = new Set(all.filter((_, i) => i % 3 === 0).map((item) => item.hash));

    // Warm the JIT and Set lookup path before measuring the installed steady
    // interaction represented by a slider move.
    filterDiversify(all, hidden);
    const samples: number[] = [];
    let shown = 0;
    for (let i = 0; i < config.iterations; i++) {
      const started = performance.now();
      const result = filterDiversify(all, hidden);
      samples.push(performance.now() - started);
      shown = result.length;
    }

    expect(shown).toBe(config.graph_items - hidden.size);
    const observed = p99(samples);
    expect(
      observed,
      `Diversify fold p99 ${observed.toFixed(2)} ms exceeded ${config.diversify_filter_p99_ms} ms`,
    ).toBeLessThan(config.diversify_filter_p99_ms);
  });

  it("lays out a 20k-node topic graph within the committed p99", () => {
    const topicCount = 8;
    const anchors = ringAnchors(topicCount, 320);
    const templates: ImageNode[] = hashes(config.graph_items).map((hash, i) => ({
      hash,
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: Array.from(
        { length: topicCount },
        (_, topic) => ((i * 17 + topic * 31) % 101) / 100,
      ),
    }));

    computeStaticLayout(structuredClone(templates), anchors, { ringRadius: 320 });
    const samples: number[] = [];
    let checksum = 0;
    for (let i = 0; i < config.iterations; i++) {
      const nodes = structuredClone(templates);
      const started = performance.now();
      computeStaticLayout(nodes, anchors, { ringRadius: 320 });
      samples.push(performance.now() - started);
      checksum += nodes[0].x + nodes[nodes.length - 1].y;
    }

    expect(Number.isFinite(checksum)).toBe(true);
    const observed = p99(samples);
    expect(
      observed,
      `graph layout p99 ${observed.toFixed(2)} ms exceeded ${config.graph_static_layout_p99_ms} ms`,
    ).toBeLessThan(config.graph_static_layout_p99_ms);
  });
});
