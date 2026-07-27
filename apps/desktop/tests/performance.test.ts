import { describe, expect, it, vi } from "vitest";
import {
  MAX_PERFORMANCE_BATCH,
  PERFORMANCE_SCHEMA_VERSION,
  PerformanceBatcher,
  monitoredInvoke,
  parsePerformanceBatch,
  parsePerformanceSample,
  type PerformanceSample,
  type RawInvoke,
} from "../src/lib/performance";

function sample(overrides: Partial<PerformanceSample> = {}): PerformanceSample {
  return {
    schemaVersion: PERFORMANCE_SCHEMA_VERSION,
    journey: "search",
    phase: "total",
    durationMs: 12,
    ok: true,
    observedAtMs: 1,
    ...overrides,
  };
}

describe("performance wire validation", () => {
  it("accepts only the versioned low-cardinality shape", () => {
    expect(parsePerformanceSample(sample())).toEqual(sample());
    expect(
      parsePerformanceSample(
        sample({ itemCount: 12, bytes: 4096, cacheStatus: "hit" }),
      ),
    ).toMatchObject({ itemCount: 12, bytes: 4096, cacheStatus: "hit" });
    expect(() =>
      parsePerformanceSample({ ...sample(), command: "search" }),
    ).toThrow(/unknown fields/);
    expect(() =>
      parsePerformanceSample(sample({ journey: "user-query" as "search" })),
    ).toThrow(/unknown journey/);
    expect(() =>
      parsePerformanceSample(sample({ durationMs: Number.POSITIVE_INFINITY })),
    ).toThrow(/duration/);
    expect(() =>
      parsePerformanceSample(sample({ itemCount: -1 })),
    ).toThrow(/itemCount/);
    expect(() =>
      parsePerformanceSample(sample({ bytes: Number.MAX_SAFE_INTEGER })),
    ).toThrow(/bytes/);
    expect(() =>
      parsePerformanceSample(
        sample({ cacheStatus: "warm" as PerformanceSample["cacheStatus"] }),
      ),
    ).toThrow(/cacheStatus/);
    expect(() =>
      parsePerformanceBatch(Array.from({ length: MAX_PERFORMANCE_BATCH + 1 }, () => sample())),
    ).toThrow(/1 to 256/);
  });
});

describe("PerformanceBatcher", () => {
  it("batches samples and bounds queue retention", async () => {
    const received: PerformanceSample[][] = [];
    const batcher = new PerformanceBatcher(
      async (samples) => {
        received.push(samples);
      },
      { maxBatch: 3, maxQueue: 3, flushIntervalMs: 60_000 },
    );
    batcher.enqueue(sample({ durationMs: 1 }));
    batcher.enqueue(sample({ durationMs: 2 }));
    // Reaching maxBatch starts an asynchronous flush. Add enough while it is
    // active to exercise the bounded pending queue independently.
    batcher.enqueue(sample({ durationMs: 3 }));
    batcher.enqueue(sample({ durationMs: 4 }));
    batcher.enqueue(sample({ durationMs: 5 }));
    batcher.enqueue(sample({ durationMs: 6 }));
    batcher.enqueue(sample({ durationMs: 7 }));
    expect(batcher.queued).toBe(3);
    expect(batcher.dropped).toBe(1);
    await batcher.flush();
    await batcher.flush();
    expect(received.map((batch) => batch.map((item) => item.durationMs))).toEqual([
      [1, 2, 3],
      [5, 6, 7],
    ]);
  });

  it("requeues a failed sink without rejecting the measured journey", async () => {
    let fail = true;
    const received: number[][] = [];
    const batcher = new PerformanceBatcher(
      async (samples) => {
        if (fail) throw new Error("disk unavailable");
        received.push(samples.map((item) => item.durationMs));
      },
      { maxBatch: 2, maxQueue: 4, flushIntervalMs: 60_000 },
    );
    batcher.enqueue(sample({ durationMs: 8 }));
    await batcher.flush();
    expect(batcher.lastSinkError).toBe("disk unavailable");
    expect(batcher.queued).toBe(1);
    fail = false;
    await batcher.flush();
    expect(received).toEqual([[8]]);
    expect(batcher.lastSinkError).toBeNull();
  });

  it("surfaces a graceful backend sink error without double-counting", async () => {
    const batcher = new PerformanceBatcher(
      async (samples) => ({
        accepted: samples.length,
        persisted: false,
        sinkError: "performance directory is read-only",
      }),
      { maxBatch: 2, flushIntervalMs: 60_000 },
    );
    batcher.enqueue(sample({ durationMs: 9 }));
    await batcher.flush();
    expect(batcher.queued).toBe(0);
    expect(batcher.lastSinkError).toBe("performance directory is read-only");
  });

  it("flushes on its timer", async () => {
    vi.useFakeTimers();
    const sink = vi.fn(async () => {});
    const batcher = new PerformanceBatcher(sink, { flushIntervalMs: 50 });
    batcher.enqueue(sample());
    await vi.advanceTimersByTimeAsync(50);
    expect(sink).toHaveBeenCalledOnce();
    vi.useRealTimers();
  });
});

describe("monitoredInvoke", () => {
  it("returns the raw promise unchanged and observes its settlement once", async () => {
    const samples: PerformanceSample[] = [];
    const batcher = new PerformanceBatcher(
      async (batch) => {
        samples.push(...batch);
      },
      { maxBatch: 8, flushIntervalMs: 60_000 },
    );
    const rawPromise = Promise.resolve("same");
    const raw: RawInvoke = <T>() => rawPromise as Promise<T>;
    const clock = [10, 15];
    const wrapped = monitoredInvoke(
      raw,
      batcher,
      "search",
      undefined,
      { journey: "search" },
      () => clock.shift() ?? 15,
    );

    expect(wrapped).toBe(rawPromise);
    await wrapped;
    await batcher.flush();
    expect(samples).toHaveLength(1);
    expect(samples[0]).toMatchObject({ durationMs: 5, ok: true });
  });

  it("records a synchronous raw invoke throw before rethrowing it", async () => {
    const samples: PerformanceSample[] = [];
    const batcher = new PerformanceBatcher(
      async (batch) => {
        samples.push(...batch);
      },
      { maxBatch: 8, flushIntervalMs: 60_000 },
    );
    const raw = (() => {
      throw new Error("synchronous invoke failure");
    }) as RawInvoke;
    const clock = [20, 23];

    expect(() =>
      monitoredInvoke(raw, batcher, "fails", undefined, {}, () => clock.shift() ?? 23),
    ).toThrow("synchronous invoke failure");
    await batcher.flush();
    expect(samples).toHaveLength(1);
    expect(samples[0]).toMatchObject({ durationMs: 3, ok: false });
  });

  it("records successful and failed durations without arguments or errors", async () => {
    const samples: PerformanceSample[] = [];
    const batcher = new PerformanceBatcher(
      async (batch) => {
        samples.push(...batch);
      },
      { maxBatch: 8, flushIntervalMs: 60_000 },
    );
    const raw: RawInvoke = async <T>(command: string) => {
      if (command === "fails") throw new Error("secret failure detail");
      return "ok" as T;
    };
    const successClock = [10, 25];
    await expect(
      monitoredInvoke(
        raw,
        batcher,
        "search",
        { query: "private" },
        {
          journey: "search",
          phase: "invoke",
          itemCount: 24,
          bytes: 4_096,
          cacheStatus: "hit",
        },
        () => successClock.shift() ?? 25,
      ),
    ).resolves.toBe("ok");
    const errorClock = [30, 42];
    await expect(
      monitoredInvoke(
        raw,
        batcher,
        "fails",
        { path: "/private/photo.jpg" },
        { journey: "preview", phase: "decode" },
        () => errorClock.shift() ?? 42,
      ),
    ).rejects.toThrow("secret failure detail");
    await batcher.flush();
    expect(samples.map(({ journey, phase, durationMs, ok }) => ({
      journey,
      phase,
      durationMs,
      ok,
    }))).toEqual([
      { journey: "search", phase: "invoke", durationMs: 15, ok: true },
      { journey: "preview", phase: "decode", durationMs: 12, ok: false },
    ]);
    expect(JSON.stringify(samples)).not.toContain("private");
    expect(JSON.stringify(samples)).not.toContain("secret");
    expect(samples[0]).toMatchObject({
      itemCount: 24,
      bytes: 4_096,
      cacheStatus: "hit",
    });
  });

  it("bypasses monitoring commands to prevent recursion", async () => {
    const sink = vi.fn(async () => {});
    const batcher = new PerformanceBatcher(sink);
    const raw: RawInvoke = async () => undefined as never;
    await monitoredInvoke(raw, batcher, "performance_ingest", { samples: [] });
    expect(batcher.queued).toBe(0);
    expect(sink).not.toHaveBeenCalled();
  });
});
