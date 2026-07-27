/**
 * Local structured performance intake.
 *
 * Labels are closed unions shared with the Rust collector. The wrapper records
 * only journey, phase, duration, timestamp, and success/error: never command
 * arguments, query text, paths, ids, or error messages.
 */

export const PERFORMANCE_SCHEMA_VERSION = 1 as const;
export const MAX_PERFORMANCE_BATCH = 256;
export const MAX_PERFORMANCE_DURATION_MS = 3_600_000;
export const MAX_PERFORMANCE_ITEM_COUNT = 10_000_000;
export const MAX_PERFORMANCE_BYTES = 2 ** 50;
export const PERFORMANCE_JOURNEYS = [
  "startup",
  "library-open",
  "root-add",
  "folder-open",
  "grid",
  "graph",
  "filter",
  "journal",
  "capture",
  "settings",
  "backup-restore",
  "search",
  "preview",
  "look",
  "model-runtime",
  "app-update",
  "shutdown",
  "ipc",
] as const;
export const PERFORMANCE_PHASES = [
  "total",
  "queue",
  "invoke",
  "read",
  "write",
  "scan",
  "decode",
  "render",
  "download",
  "verify",
  "load",
  "reconcile",
  "first-paint",
  "layout",
  "settle",
  "cache-lookup",
  "resize",
  "encode",
  "serve",
  "filter",
] as const;

export type PerformanceJourney = (typeof PERFORMANCE_JOURNEYS)[number];
export type PerformancePhase = (typeof PERFORMANCE_PHASES)[number];
export type PerformanceCacheStatus = "none" | "hit" | "miss" | "stale";

export interface PerformanceSample {
  schemaVersion: typeof PERFORMANCE_SCHEMA_VERSION;
  journey: PerformanceJourney;
  phase: PerformancePhase;
  durationMs: number;
  ok: boolean;
  observedAtMs: number;
  itemCount?: number;
  bytes?: number;
  cacheStatus?: PerformanceCacheStatus;
}

export interface PerformanceIngestReport {
  accepted: number;
  persisted: boolean;
  sinkError: string | null;
}

export interface PerformanceSeries {
  source: "frontend" | "backend";
  journey: PerformanceJourney;
  phase: PerformancePhase;
  count: number;
  errors: number;
  retained: number;
  p50Ms: number | null;
  p95Ms: number | null;
  p99Ms: number | null;
  maxMs: number;
  totalItems: number;
  totalBytes: number;
}

export interface PerformanceSnapshot {
  schemaVersion: typeof PERFORMANCE_SCHEMA_VERSION;
  appVersion: string;
  os: string;
  arch: string;
  runId: string;
  series: PerformanceSeries[];
  retainedSamples: number;
  sinkError: string | null;
  rotatedLogs: number;
  logPath: string;
}

const journeySet = new Set<string>(PERFORMANCE_JOURNEYS);
const phaseSet = new Set<string>(PERFORMANCE_PHASES);
const cacheStatusSet = new Set<string>(["none", "hit", "miss", "stale"]);
const requiredSampleKeys = [
  "schemaVersion",
  "journey",
  "phase",
  "durationMs",
  "ok",
  "observedAtMs",
] as const;
const sampleKeys = new Set([
  ...requiredSampleKeys,
  "itemCount",
  "bytes",
  "cacheStatus",
]);

/** Strict wire validation. Unknown fields are rejected so future callers
 * cannot accidentally smuggle high-cardinality data into the local log. */
export function parsePerformanceSample(value: unknown): PerformanceSample {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("performance sample must be an object");
  }
  const record = value as Record<string, unknown>;
  if (
    requiredSampleKeys.some(
      (key) => !Object.prototype.hasOwnProperty.call(record, key),
    ) ||
    Object.keys(record).some((key) => !sampleKeys.has(key))
  ) {
    throw new TypeError("performance sample has missing or unknown fields");
  }
  if (record.schemaVersion !== PERFORMANCE_SCHEMA_VERSION) {
    throw new TypeError("performance sample has an unsupported schema");
  }
  if (typeof record.journey !== "string" || !journeySet.has(record.journey)) {
    throw new TypeError("performance sample has an unknown journey");
  }
  if (typeof record.phase !== "string" || !phaseSet.has(record.phase)) {
    throw new TypeError("performance sample has an unknown phase");
  }
  if (
    typeof record.durationMs !== "number" ||
    !Number.isFinite(record.durationMs) ||
    record.durationMs < 0 ||
    record.durationMs > MAX_PERFORMANCE_DURATION_MS
  ) {
    throw new TypeError("performance duration is outside the accepted range");
  }
  if (typeof record.ok !== "boolean") {
    throw new TypeError("performance sample ok must be boolean");
  }
  if (
    typeof record.observedAtMs !== "number" ||
    !Number.isSafeInteger(record.observedAtMs) ||
    record.observedAtMs <= 0
  ) {
    throw new TypeError("performance observedAtMs must be a positive safe integer");
  }
  if (
    Object.prototype.hasOwnProperty.call(record, "itemCount") &&
    (typeof record.itemCount !== "number" ||
      !Number.isSafeInteger(record.itemCount) ||
      record.itemCount < 0 ||
      record.itemCount > MAX_PERFORMANCE_ITEM_COUNT)
  ) {
    throw new TypeError("performance itemCount is outside the accepted range");
  }
  if (
    Object.prototype.hasOwnProperty.call(record, "bytes") &&
    (typeof record.bytes !== "number" ||
      !Number.isSafeInteger(record.bytes) ||
      record.bytes < 0 ||
      record.bytes > MAX_PERFORMANCE_BYTES)
  ) {
    throw new TypeError("performance bytes is outside the accepted range");
  }
  if (
    Object.prototype.hasOwnProperty.call(record, "cacheStatus") &&
    (typeof record.cacheStatus !== "string" ||
      !cacheStatusSet.has(record.cacheStatus))
  ) {
    throw new TypeError("performance cacheStatus is unknown");
  }
  return record as unknown as PerformanceSample;
}

export function parsePerformanceBatch(value: unknown): PerformanceSample[] {
  if (
    !Array.isArray(value) ||
    value.length === 0 ||
    value.length > MAX_PERFORMANCE_BATCH
  ) {
    throw new TypeError(
      `performance batch must contain 1 to ${MAX_PERFORMANCE_BATCH} samples`,
    );
  }
  return value.map(parsePerformanceSample);
}

export type PerformanceSink = (
  samples: PerformanceSample[],
) => Promise<PerformanceIngestReport | void>;

export interface PerformanceBatcherOptions {
  maxBatch?: number;
  maxQueue?: number;
  flushIntervalMs?: number;
  setTimer?: typeof setTimeout;
  clearTimer?: typeof clearTimeout;
}

/**
 * Bounded, best-effort batcher. Sink failure is retained as status and the
 * failed batch is requeued within the same hard bound; it never rejects into
 * the user journey that was being measured.
 */
export class PerformanceBatcher {
  readonly maxBatch: number;
  readonly maxQueue: number;
  readonly flushIntervalMs: number;
  dropped = 0;
  lastSinkError: string | null = null;

  private readonly sink: PerformanceSink;
  private readonly setTimer: typeof setTimeout;
  private readonly clearTimer: typeof clearTimeout;
  private queue: PerformanceSample[] = [];
  private timer: ReturnType<typeof setTimeout> | null = null;
  private flushing: Promise<void> | null = null;

  constructor(sink: PerformanceSink, options: PerformanceBatcherOptions = {}) {
    this.sink = sink;
    this.maxBatch = Math.min(
      MAX_PERFORMANCE_BATCH,
      Math.max(1, Math.floor(options.maxBatch ?? 64)),
    );
    this.maxQueue = Math.max(this.maxBatch, Math.floor(options.maxQueue ?? 1_024));
    this.flushIntervalMs = Math.max(50, Math.floor(options.flushIntervalMs ?? 1_000));
    this.setTimer = options.setTimer ?? setTimeout;
    this.clearTimer = options.clearTimer ?? clearTimeout;
  }

  get queued(): number {
    return this.queue.length;
  }

  enqueue(sample: PerformanceSample): boolean {
    let valid: PerformanceSample;
    try {
      valid = parsePerformanceSample(sample);
    } catch {
      this.dropped += 1;
      return false;
    }
    if (this.queue.length === this.maxQueue) {
      this.queue.shift();
      this.dropped += 1;
    }
    this.queue.push(valid);
    if (this.queue.length >= this.maxBatch) {
      void this.flush();
    } else {
      this.schedule();
    }
    return true;
  }

  async flush(): Promise<void> {
    if (this.flushing !== null) return this.flushing;
    if (this.timer !== null) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
    if (this.queue.length === 0) return;
    const batch = this.queue.splice(0, this.maxBatch);
    this.flushing = (async () => {
      try {
        const report = await this.sink(batch);
        // A graceful backend sink failure still accepts the batch into bounded
        // memory and returns a report instead of rejecting. Surface that state
        // without requeueing and double-counting the accepted samples.
        this.lastSinkError = report?.sinkError ?? null;
      } catch (error) {
        this.lastSinkError = error instanceof Error ? error.message : String(error);
        const requeued = [...batch, ...this.queue];
        if (requeued.length > this.maxQueue) {
          this.dropped += requeued.length - this.maxQueue;
          requeued.length = this.maxQueue;
        }
        this.queue = requeued;
      } finally {
        this.flushing = null;
        if (this.queue.length > 0) this.schedule();
      }
    })();
    return this.flushing;
  }

  async close(): Promise<void> {
    if (this.timer !== null) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
    do {
      await this.flush();
    } while (this.queue.length > 0 && this.lastSinkError === null);
  }

  private schedule() {
    if (this.timer !== null || this.flushing !== null) return;
    this.timer = this.setTimer(() => {
      this.timer = null;
      void this.flush();
    }, this.flushIntervalMs);
  }
}

export type RawInvoke = <T>(
  command: string,
  args?: Record<string, unknown>,
) => Promise<T>;

const monitoringCommands = new Set([
  "performance_ingest",
  "performance_snapshot",
]);

function monotonicNow(): number {
  return globalThis.performance?.now() ?? Date.now();
}

export interface MonitoredInvokeLabels {
  journey?: PerformanceJourney;
  phase?: PerformancePhase;
  itemCount?: number;
  bytes?: number;
  cacheStatus?: PerformanceCacheStatus;
}

/**
 * Generic invoke timer. The raw invoke function is injected instead of
 * imported, and monitor commands bypass sampling, so sending a batch cannot
 * recursively generate another batch.
 */
export function monitoredInvoke<T>(
  rawInvoke: RawInvoke,
  batcher: PerformanceBatcher,
  command: string,
  args?: Record<string, unknown>,
  labels: MonitoredInvokeLabels = {},
  clock: () => number = monotonicNow,
): Promise<T> {
  if (monitoringCommands.has(command)) {
    return rawInvoke<T>(command, args);
  }
  const started = clock();
  const record = (ok: boolean) => {
    const durationMs = Math.max(0, clock() - started);
    const sample: PerformanceSample = {
      schemaVersion: PERFORMANCE_SCHEMA_VERSION,
      journey: labels.journey ?? "ipc",
      phase: labels.phase ?? "invoke",
      durationMs: Math.min(durationMs, MAX_PERFORMANCE_DURATION_MS),
      ok,
      observedAtMs: Date.now(),
    };
    if (labels.itemCount !== undefined) sample.itemCount = labels.itemCount;
    if (labels.bytes !== undefined) sample.bytes = labels.bytes;
    if (labels.cacheStatus !== undefined) sample.cacheStatus = labels.cacheStatus;
    batcher.enqueue(sample);
  };

  let promise: Promise<T>;
  try {
    promise = rawInvoke<T>(command, args);
  } catch (error) {
    record(false);
    throw error;
  }
  // Observe settlement without returning the chained promise. Callers receive
  // the exact raw invoke promise, preserving its identity and microtask timing.
  void promise.then(
    () => record(true),
    () => record(false),
  );
  return promise;
}

/** Build a default sink without importing or wrapping invoke inside this
 * module. Root integration must pass the unmonitored Tauri invoke function. */
export function createPerformanceBatcher(
  rawInvoke: RawInvoke,
  options: PerformanceBatcherOptions = {},
): PerformanceBatcher {
  return new PerformanceBatcher(
    (samples) =>
      rawInvoke<PerformanceIngestReport>("performance_ingest", { samples }),
    options,
  );
}
