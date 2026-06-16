/**
 * WKWebView capability probe (BACKLOG: "WKWebView capability check") — the
 * runtime feature-detection that GATES two perf items: moving the
 * visualizer force-sim off the main thread into a Web Worker, and the
 * optional off-main-thread thumbnail decode via createImageBitmap. Tauri on
 * macOS renders in the SYSTEM WKWebView, so support tracks the host's
 * Safari/WebKit version (see docs/SPIKE-WKWEBVIEW.md for the version map).
 *
 * Everything here is pure, dependency-free, and defensive: each probe is
 * wrapped so a missing API or a throwing getContext can never break startup
 * or a caller. We feature-DETECT (does the API exist and can we make a
 * context) rather than user-agent sniff, because the UA only tells us the
 * WebKit version; the probe tells us what this exact webview will actually
 * do. The UA string is captured alongside the booleans for the record so a
 * live `tauri dev` run pins the host WebKit version next to the verdicts.
 */

/** The structured result of one probe pass. All synchronous; WebGPU adapter
 * acquisition is async and exposed separately via `probeWebGpuAdapter`. */
export interface WebviewCaps {
  /** OffscreenCanvas constructor exists AND yields a usable 2d context. */
  offscreenCanvas2d: boolean;
  /** OffscreenCanvas exists AND yields a usable webgl context (the path the
   * worker-side visualizer render would take). */
  offscreenCanvasWebgl: boolean;
  /** Web Workers constructor is present (the visualizer-off-main-thread floor). */
  workers: boolean;
  /** createImageBitmap is callable (the off-main-thread decode primitive). */
  createImageBitmap: boolean;
  /** A WebGL1 context can be created on a throwaway canvas. */
  webgl: boolean;
  /** A WebGL2 context can be created on a throwaway canvas. */
  webgl2: boolean;
  /** navigator.gpu exists. Presence only — an adapter request is async and
   * can still fail; use `probeWebGpuAdapter` for the definitive check. */
  webgpuApi: boolean;
  /** The webview user-agent string (carries the WebKit version), for the
   * record. Empty string when navigator is absent. */
  userAgent: string;
}

/** Run a thunk and coerce any throw to `false` — the probes must never
 * surface an exception to the caller or to startup. */
function safeBool(fn: () => boolean): boolean {
  try {
    return fn();
  } catch {
    return false;
  }
}

/** A throwaway on-DOM <canvas>, or null when there's no document (e.g. a
 * worker or a non-DOM test env). Detached canvases still create GL contexts. */
function throwawayCanvas(): HTMLCanvasElement | null {
  if (typeof document === "undefined") return null;
  return document.createElement("canvas");
}

/** WebGPU's navigator.gpu is not in the default TS DOM lib (no @webgpu/types
 * dependency here), so reach it through a narrow typed view rather than `any`. */
function navigatorGpu(): unknown {
  if (typeof navigator === "undefined") return undefined;
  return (navigator as Navigator & { gpu?: unknown }).gpu;
}

/** Detect every capability synchronously. Pure and side-effect-free apart
 * from creating (and discarding) throwaway canvases. */
export function probeWebviewCaps(): WebviewCaps {
  const offscreenCanvas2d = safeBool(() => {
    if (typeof OffscreenCanvas === "undefined") return false;
    // 1x1 is enough to prove the context backend exists.
    return new OffscreenCanvas(1, 1).getContext("2d") !== null;
  });

  const offscreenCanvasWebgl = safeBool(() => {
    if (typeof OffscreenCanvas === "undefined") return false;
    return new OffscreenCanvas(1, 1).getContext("webgl") !== null;
  });

  const workers = safeBool(() => typeof Worker !== "undefined");

  const createImageBitmap = safeBool(() => typeof globalThis.createImageBitmap === "function");

  const webgl = safeBool(() => {
    const c = throwawayCanvas();
    if (c === null) return false;
    // Some webviews expose only the prefixed experimental name.
    return c.getContext("webgl") !== null || c.getContext("experimental-webgl") !== null;
  });

  const webgl2 = safeBool(() => {
    const c = throwawayCanvas();
    if (c === null) return false;
    return c.getContext("webgl2") !== null;
  });

  const webgpuApi = safeBool(() => navigatorGpu() != null);

  const userAgent = safeBool(() => typeof navigator !== "undefined") ? navigator.userAgent : "";

  return {
    offscreenCanvas2d,
    offscreenCanvasWebgl,
    workers,
    createImageBitmap,
    webgl,
    webgl2,
    webgpuApi,
    userAgent,
  };
}

/** The definitive WebGPU check: request an adapter. Presence of navigator.gpu
 * (webgpuApi) does NOT guarantee a working adapter (it can be flagged off,
 * or the GPU can be denied), so this async probe is the real GO/NO-GO for any
 * future WebGPU render path. Returns false on any absence or failure. */
export async function probeWebGpuAdapter(): Promise<boolean> {
  try {
    const gpu = navigatorGpu() as { requestAdapter?: () => Promise<unknown> } | undefined;
    if (gpu == null || typeof gpu.requestAdapter !== "function") return false;
    const adapter = await gpu.requestAdapter();
    return adapter != null;
  } catch {
    return false;
  }
}

/** A one-line, log-friendly summary of a probe result. Used at startup so a
 * live `tauri dev` console (and the founder's dogfood session) records what
 * this exact host WKWebView supports without opening a panel. */
export function summarizeWebviewCaps(caps: WebviewCaps): string {
  const flag = (b: boolean): string => (b ? "yes" : "no");
  return [
    `offscreen2d=${flag(caps.offscreenCanvas2d)}`,
    `offscreenWebgl=${flag(caps.offscreenCanvasWebgl)}`,
    `workers=${flag(caps.workers)}`,
    `createImageBitmap=${flag(caps.createImageBitmap)}`,
    `webgl=${flag(caps.webgl)}`,
    `webgl2=${flag(caps.webgl2)}`,
    `webgpuApi=${flag(caps.webgpuApi)}`,
    `ua="${caps.userAgent}"`,
  ].join(" ");
}

/** Probe once and emit the summary on console.info so a real app run pins the
 * verdicts to the host WebKit version. Idempotent-safe to call at startup;
 * returns the result so callers can also wire it elsewhere. Defensive: a
 * console failure (none expected) is swallowed so startup can't break. */
export function logWebviewCaps(): WebviewCaps {
  const caps = probeWebviewCaps();
  try {
    // eslint-disable-next-line no-console -- intentional startup diagnostic
    console.info(`[webview-caps] ${summarizeWebviewCaps(caps)}`);
  } catch {
    // never let a diagnostic break startup
  }
  return caps;
}
