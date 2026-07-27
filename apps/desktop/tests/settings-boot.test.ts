import { describe, expect, it, vi } from "vitest";
import {
  SettingsBootController,
  type SettingsBootReads,
  type SettingsBootSink,
  type SettingsBootState,
} from "../src/lib/settings/boot";
import type {
  AppSettings,
  PreviewCacheStatsDto,
  RootDto,
  RuntimeStatus,
} from "../src/lib/types/dto";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

const roots = [{ rootId: "root-new" }] as RootDto[];
const runtime = { tierEffective: 1, models: [] } as unknown as RuntimeStatus;
const settings = {
  stackDisplay: "jpeg",
  previewCacheBudgetBytes: 20 * 1024 ** 3,
} as AppSettings;
const cache = {
  fullBytes: 0,
  fullFiles: 0,
  totalBytes: 0,
  budgetBytes: 20 * 1024 ** 3,
} as PreviewCacheStatsDto;

function harness(reads: SettingsBootReads) {
  const applied = {
    roots: [] as RootDto[][],
    runtime: [] as RuntimeStatus[],
    settings: [] as AppSettings[],
    cache: [] as PreviewCacheStatsDto[],
    states: [] as SettingsBootState[],
  };
  const sink: SettingsBootSink = {
    roots: (value) => applied.roots.push(value),
    runtime: (value) => applied.runtime.push(value),
    settings: (value) => applied.settings.push(value),
    cache: (value) => applied.cache.push(value),
    state: (value) => applied.states.push(value),
  };
  const controller = new SettingsBootController(reads, sink);
  controller.listenersReady();
  return { controller, applied };
}

describe("SettingsBootController", () => {
  it("starts all four cold reads concurrently and settles partial failure as degraded", async () => {
    const pendingRoots = deferred<RootDto[]>();
    const pendingRuntime = deferred<RuntimeStatus>();
    const pendingSettings = deferred<AppSettings>();
    const pendingCache = deferred<PreviewCacheStatsDto>();
    const reads = {
      roots: vi.fn(() => pendingRoots.promise),
      runtime: vi.fn(() => pendingRuntime.promise),
      settings: vi.fn(() => pendingSettings.promise),
      cache: vi.fn(() => pendingCache.promise),
    };
    const { controller, applied } = harness(reads);

    const refresh = controller.refresh();
    await Promise.resolve();
    expect(reads.roots).toHaveBeenCalledOnce();
    expect(reads.runtime).toHaveBeenCalledOnce();
    expect(reads.settings).toHaveBeenCalledOnce();
    expect(reads.cache).toHaveBeenCalledOnce();

    pendingRoots.resolve(roots);
    pendingRuntime.resolve(runtime);
    pendingSettings.reject(new Error("settings locked"));
    pendingCache.reject(new Error("cache volume asleep"));
    await expect(refresh).resolves.toBeUndefined();

    expect(applied.roots).toEqual([roots]);
    expect(applied.runtime).toEqual([runtime]);
    expect(applied.settings).toEqual([]);
    expect(applied.cache).toEqual([]);
    expect(applied.states.at(-1)).toMatchObject({
      phase: "degraded",
      hasSnapshot: true,
      issues: [
        { source: "settings", message: "settings locked" },
        { source: "cache", message: "cache volume asleep" },
      ],
    });
  });

  it("reports an all-source failure as fatal, then recovers cleanly on retry", async () => {
    let healthy = false;
    const reads: SettingsBootReads = {
      roots: () => (healthy ? Promise.resolve(roots) : Promise.reject(new Error("roots"))),
      runtime: () =>
        healthy ? Promise.resolve(runtime) : Promise.reject(new Error("runtime")),
      settings: () => {
        if (!healthy) throw new Error("settings");
        return Promise.resolve(settings);
      },
      cache: () => (healthy ? Promise.resolve(cache) : Promise.reject(new Error("cache"))),
    };
    const { controller, applied } = harness(reads);

    await expect(controller.refresh()).resolves.toBeUndefined();
    expect(applied.states.at(-1)).toMatchObject({
      phase: "fatal",
      hasSnapshot: false,
    });

    healthy = true;
    await controller.refresh();
    expect(applied.states.at(-1)).toMatchObject({
      phase: "ready",
      issues: [],
      hasSnapshot: true,
      attempt: 2,
    });
    expect(applied.roots).toEqual([roots]);
    expect(applied.runtime).toEqual([runtime]);
    expect(applied.settings).toEqual([settings]);
    expect(applied.cache).toEqual([cache]);
  });

  it("keeps successful snapshots when a later retry fails", async () => {
    let failOptional = false;
    const reads: SettingsBootReads = {
      roots: async () => roots,
      runtime: async () => runtime,
      settings: () =>
        failOptional ? Promise.reject(new Error("settings retry")) : Promise.resolve(settings),
      cache: () =>
        failOptional ? Promise.reject(new Error("cache retry")) : Promise.resolve(cache),
    };
    const { controller, applied } = harness(reads);

    await controller.refresh();
    failOptional = true;
    await controller.refresh();

    expect(applied.settings).toEqual([settings]);
    expect(applied.cache).toEqual([cache]);
    expect(applied.states.at(-1)).toMatchObject({
      phase: "degraded",
      hasSnapshot: true,
    });
  });

  it("never lets an older cold response overwrite a newer live event", async () => {
    const coldRuntime = deferred<RuntimeStatus>();
    const reads: SettingsBootReads = {
      roots: async () => roots,
      runtime: () => coldRuntime.promise,
      settings: async () => settings,
      cache: async () => cache,
    };
    const { controller, applied } = harness(reads);
    const liveRuntime = {
      tierEffective: 2,
      models: [],
    } as unknown as RuntimeStatus;

    const refresh = controller.refresh();
    await Promise.resolve();
    controller.liveRuntime(liveRuntime);
    coldRuntime.resolve(runtime);
    await refresh;

    expect(applied.runtime).toEqual([liveRuntime]);
    expect(applied.states.at(-1)).toMatchObject({
      phase: "ready",
      issues: [],
    });
  });

  it("treats failed event subscriptions as retryable boot health", async () => {
    const { controller, applied } = harness({
      roots: async () => roots,
      runtime: async () => runtime,
      settings: async () => settings,
      cache: async () => cache,
    });
    await controller.refresh();

    controller.listenersFailed(new Error("runtime-status listener unavailable"));
    expect(applied.states.at(-1)).toMatchObject({
      phase: "degraded",
      hasSnapshot: true,
      issues: [
        {
          source: "events",
          message: "runtime-status listener unavailable",
        },
      ],
    });

    controller.listenersReady();
    expect(applied.states.at(-1)).toMatchObject({
      phase: "ready",
      issues: [],
    });
  });
});
