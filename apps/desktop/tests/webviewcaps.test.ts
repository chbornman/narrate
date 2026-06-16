/**
 * WKWebView capability probe (logic/webviewcaps.ts) — the GATING feature
 * detection for the visualizer-off-main-thread and off-main-thread thumbnail
 * decode work. jsdom exposes none of these APIs and a real WebKit getContext,
 * so each case stubs the relevant globals present/absent and asserts the
 * structured verdict. The probe must NEVER throw, so a throwing getContext is
 * exercised too.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  logWebviewCaps,
  probeWebGpuAdapter,
  probeWebviewCaps,
  summarizeWebviewCaps,
} from "../src/lib/logic/webviewcaps";

// Snapshot the globals we mutate so each test starts clean.
const ORIGINAL = {
  OffscreenCanvas: globalThis.OffscreenCanvas,
  Worker: globalThis.Worker,
  createImageBitmap: globalThis.createImageBitmap,
};

/** Install an OffscreenCanvas stub whose getContext answers per the map; a
 * context name absent from the map returns null (unsupported). */
function stubOffscreenCanvas(contexts: Record<string, boolean>): void {
  class FakeOffscreen {
    constructor(_w: number, _h: number) {}
    getContext(kind: string): object | null {
      return contexts[kind] ? {} : null;
    }
  }
  // @ts-expect-error — assigning a test double onto the global.
  globalThis.OffscreenCanvas = FakeOffscreen;
}

/** Force HTMLCanvasElement.getContext for the throwaway-canvas probes. The
 * returned function decides webgl / webgl2 / experimental-webgl support. */
function stubCanvasGetContext(impl: (kind: string) => object | null): void {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
    impl as unknown as typeof HTMLCanvasElement.prototype.getContext,
  );
}

beforeEach(() => {
  // Start every case with the supporting APIs ABSENT (jsdom's real baseline),
  // so a test opts in to exactly what it needs.
  // @ts-expect-error — deleting an optional global for the absent-case baseline.
  delete globalThis.OffscreenCanvas;
  // @ts-expect-error — same.
  delete globalThis.Worker;
  // @ts-expect-error — same.
  delete globalThis.createImageBitmap;
  // Clear any WebGPU stub between cases.
  delete (navigator as Navigator & { gpu?: unknown }).gpu;
});

afterEach(() => {
  vi.restoreAllMocks();
  globalThis.OffscreenCanvas = ORIGINAL.OffscreenCanvas;
  globalThis.Worker = ORIGINAL.Worker;
  globalThis.createImageBitmap = ORIGINAL.createImageBitmap;
  // Clear the WebGPU stub.
  delete (navigator as Navigator & { gpu?: unknown }).gpu;
});

describe("probeWebviewCaps — synchronous feature detection", () => {
  it("reports every capability false on a bare environment (jsdom baseline)", () => {
    stubCanvasGetContext(() => null);
    const caps = probeWebviewCaps();
    expect(caps.offscreenCanvas2d).toBe(false);
    expect(caps.offscreenCanvasWebgl).toBe(false);
    expect(caps.workers).toBe(false);
    expect(caps.createImageBitmap).toBe(false);
    expect(caps.webgl).toBe(false);
    expect(caps.webgl2).toBe(false);
    expect(caps.webgpuApi).toBe(false);
    // userAgent always present in jsdom.
    expect(typeof caps.userAgent).toBe("string");
  });

  it("reports a fully-capable modern WKWebView (Safari 17+ shape)", () => {
    stubOffscreenCanvas({ "2d": true, webgl: true });
    // @ts-expect-error — Worker double.
    globalThis.Worker = class {};
    globalThis.createImageBitmap = (() =>
      Promise.resolve({})) as unknown as typeof createImageBitmap;
    stubCanvasGetContext((kind) => (kind === "webgl" || kind === "webgl2" ? {} : null));
    (navigator as Navigator & { gpu?: unknown }).gpu = { requestAdapter: async () => ({}) };

    const caps = probeWebviewCaps();
    expect(caps.offscreenCanvas2d).toBe(true);
    expect(caps.offscreenCanvasWebgl).toBe(true);
    expect(caps.workers).toBe(true);
    expect(caps.createImageBitmap).toBe(true);
    expect(caps.webgl).toBe(true);
    expect(caps.webgl2).toBe(true);
    expect(caps.webgpuApi).toBe(true);
  });

  it("distinguishes OffscreenCanvas 2d from webgl context support", () => {
    stubOffscreenCanvas({ "2d": true }); // webgl unsupported off the offscreen
    const caps = probeWebviewCaps();
    expect(caps.offscreenCanvas2d).toBe(true);
    expect(caps.offscreenCanvasWebgl).toBe(false);
  });

  it("treats a throwing OffscreenCanvas getContext as unsupported, never throwing", () => {
    class Throwing {
      constructor(_w: number, _h: number) {}
      getContext(): never {
        throw new Error("context backend exploded");
      }
    }
    // @ts-expect-error — test double.
    globalThis.OffscreenCanvas = Throwing;
    stubCanvasGetContext(() => null); // keep the throwaway-canvas probes quiet
    expect(() => probeWebviewCaps()).not.toThrow();
    const caps = probeWebviewCaps();
    expect(caps.offscreenCanvas2d).toBe(false);
    expect(caps.offscreenCanvasWebgl).toBe(false);
  });

  it("accepts the prefixed experimental-webgl name as WebGL support", () => {
    // A webview that only exposes the legacy prefixed name.
    stubCanvasGetContext((kind) => (kind === "experimental-webgl" ? {} : null));
    const caps = probeWebviewCaps();
    expect(caps.webgl).toBe(true);
    expect(caps.webgl2).toBe(false);
  });

  it("detects WebGL2 without WebGL1 falling over", () => {
    stubCanvasGetContext((kind) => (kind === "webgl" || kind === "webgl2" ? {} : null));
    const caps = probeWebviewCaps();
    expect(caps.webgl).toBe(true);
    expect(caps.webgl2).toBe(true);
  });

  it("reports webgpuApi present when navigator.gpu exists (presence only)", () => {
    stubCanvasGetContext(() => null);
    (navigator as Navigator & { gpu?: unknown }).gpu = {};
    expect(probeWebviewCaps().webgpuApi).toBe(true);
  });
});

describe("probeWebGpuAdapter — the async definitive WebGPU check", () => {
  it("is false when navigator.gpu is absent", async () => {
    await expect(probeWebGpuAdapter()).resolves.toBe(false);
  });

  it("is false when requestAdapter resolves null (flagged off / denied)", async () => {
    (navigator as Navigator & { gpu?: unknown }).gpu = { requestAdapter: async () => null };
    await expect(probeWebGpuAdapter()).resolves.toBe(false);
  });

  it("is true when an adapter is granted", async () => {
    (navigator as Navigator & { gpu?: unknown }).gpu = { requestAdapter: async () => ({}) };
    await expect(probeWebGpuAdapter()).resolves.toBe(true);
  });

  it("swallows a throwing requestAdapter and returns false", async () => {
    (navigator as Navigator & { gpu?: unknown }).gpu = {
      requestAdapter: async () => {
        throw new Error("no GPU");
      },
    };
    await expect(probeWebGpuAdapter()).resolves.toBe(false);
  });
});

describe("summarizeWebviewCaps / logWebviewCaps", () => {
  it("renders a one-line yes/no summary with the UA", () => {
    const line = summarizeWebviewCaps({
      offscreenCanvas2d: true,
      offscreenCanvasWebgl: false,
      workers: true,
      createImageBitmap: true,
      webgl: true,
      webgl2: true,
      webgpuApi: false,
      userAgent: "TestUA/1.0",
    });
    expect(line).toContain("offscreen2d=yes");
    expect(line).toContain("offscreenWebgl=no");
    expect(line).toContain("webgpuApi=no");
    expect(line).toContain('ua="TestUA/1.0"');
  });

  it("logWebviewCaps emits console.info once and returns the result", () => {
    stubCanvasGetContext(() => null);
    const spy = vi.spyOn(console, "info").mockImplementation(() => {});
    const caps = logWebviewCaps();
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy.mock.calls[0]?.[0]).toContain("[webview-caps]");
    expect(caps).toHaveProperty("userAgent");
  });
});
