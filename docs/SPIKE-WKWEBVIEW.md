# SPIKE WKWEBVIEW — webview capability check (June 16, 2026)

GATES two BACKLOG perf items: "Visualizer off main thread, then WebGL" and
"Off-main-thread thumbnail decode". Tauri on macOS renders in the SYSTEM
WKWebView (no bundled Chromium), so what these features can do tracks the
host's Safari/WebKit version. This spike ships a runtime probe
(`apps/desktop/src/lib/logic/webviewcaps.ts`, unit-tested in
`tests/webviewcaps.test.ts`) and records the version reasoning and the
GO/NO-GO calls. DEFINITIVE per-host confirmation comes from reading the probe
line out of a live app run (see "Reading the probe output").

## The capabilities, and what they unlock

| Capability | What it is | Gated work that needs it |
|---|---|---|
| Web Workers | Background JS thread (`new Worker`) | Visualizer sim off the main thread (the floor); worker-side thumbnail decode |
| OffscreenCanvas (2d / webgl) | A canvas usable from a worker, no DOM node | Rendering the visualizer FROM the worker thread |
| createImageBitmap | Decode an image to a transferable bitmap, off-thread | Off-main-thread thumbnail decode |
| WebGL / WebGL2 | GPU 2d/3d raster context | Full visualizer render (Sigma.js / GPU layout) |
| WebGPU (`navigator.gpu`) | Modern GPU compute + render API | Future GPU layout / render; NOT required for the interim worker move |

## Expected support on current macOS WKWebView

WKWebView == the host's installed Safari/WebKit. The version map that matters:

- **Web Workers**: ancient baseline. Present on every macOS WKWebView we
  target. No version concern.
- **createImageBitmap**: long-supported in WebKit (Safari 15+, and the
  ImageBitmap/transferToImageBitmap path is cross-browser baseline since
  March 2023). Safe on any macOS we ship to.
- **WebGL / WebGL2**: broadly supported; WebGL2 has been on by default in
  Safari since 15 (2021). Safe.
- **OffscreenCanvas**: this is the one with a real floor. Full OffscreenCanvas
  (including the 2d context, not just the old experimental webgl-only form)
  shipped in **Safari 16.4 (March 2023)**. Using an OffscreenCanvas WebGL2
  context from a worker is solid from **Safari 17 (macOS Sonoma, 2023)**
  onward. So: macOS Sonoma (14) / Sequoia (15) / macOS 26 all clear this bar
  comfortably; anything older than Safari 16.4 does not.
- **WebGPU**: the newest and the partial one. `navigator.gpu` was opt-in
  (experimental feature flag) through **Safari 18 / macOS 15 Sequoia**, and
  became **enabled by default in Safari 26 (macOS 26, September 2025)**. So
  on a Sequoia host the API may be ABSENT unless the user flipped the flag;
  on macOS 26 it is present by default. Even when present, an adapter request
  is async and can still be denied, which is why the probe exposes a separate
  `probeWebGpuAdapter()` async check.

Net: every capability the gated work actually NEEDS (Workers, OffscreenCanvas,
createImageBitmap, WebGL2) is expected GREEN on any macOS we realistically
target (Sonoma 14+). WebGPU is the only one that is version-contingent, and
none of the three gated items below require it.

## The probe

`apps/desktop/src/lib/logic/webviewcaps.ts` — pure, dependency-free, defensive
(every detector is wrapped so a missing API or a throwing `getContext` returns
`false`, never an exception). Exports:

- `probeWebviewCaps(): WebviewCaps` — synchronous booleans plus the webview
  user-agent string (carries the WebKit version, captured for the record).
- `probeWebGpuAdapter(): Promise<boolean>` — the definitive async WebGPU check
  (presence of `navigator.gpu` is necessary but not sufficient; this requests
  an adapter).
- `summarizeWebviewCaps(caps)` — a one-line `key=yes/no` summary with the UA.
- `logWebviewCaps()` — probe once and emit the summary on `console.info`.

Wiring: `apps/desktop/src/main.ts` calls `logWebviewCaps()` once at startup
(both webviews boot through `main.ts`). There is no frontend Tauri log plugin
in this app, so the chosen surface is `console.info`, visible in the webview
devtools console on a live `tauri dev` run. The probe is also exported for
use by a future debug-panel tab or a feature-gate decision without adding UI
now.

## Reading the probe output

On a live `tauri dev` run, open the webview devtools console and look for the
single line:

```
[webview-caps] offscreen2d=yes offscreenWebgl=yes workers=yes createImageBitmap=yes webgl=yes webgl2=yes webgpuApi=<yes|no> ua="...Version/XX.X Safari/..."
```

- The `ua="..."` `Version/XX.X` token is the host Safari/WebKit version — read
  it against the version map above.
- `webgpuApi` is presence only; for a real WebGPU go-decision later, call
  `probeWebGpuAdapter()` and check the adapter resolves.

## GO / NO-GO for the gated work

All three calls assume the expected modern-macOS verdicts above; LIVE
confirmation is reading the probe line from an actual app run on the founder's
target macOS (the founder is mid-dogfood, so this spike does NOT launch a
second `tauri dev` to capture it — the probe is in place to record it on the
next natural run).

1. **Visualizer sim into a Web Worker (interim move): GO.** Needs only
   `workers=yes`, which is universally available. This is the safe first step
   and does not touch OffscreenCanvas or WebGL.
2. **WebGL render for the visualizer (full step): GO.** `webgl2=yes` is
   expected everywhere we ship. If the render is driven FROM the worker it also
   needs `offscreenWebgl=yes` (Safari 17+ solid); if the worker only runs the
   sim and posts positions back to a main-thread WebGL canvas, OffscreenCanvas
   is not even required. Prefer the latter split if a host ever reports
   `offscreenWebgl=no`.
3. **Off-main-thread thumbnail decode via createImageBitmap: GO (but
   optional).** Needs `workers=yes` + `createImageBitmap=yes`, both expected
   green. Per BACKLOG this is a control upgrade, not a fix (the grid is already
   virtualized and `Thumb.svelte` already uses `decoding="async"`), so build
   it only if scroll-decode jank is actually measured.

**WebGPU: HOLD / not on the critical path.** Useful later for GPU layout or
render, but version-contingent (default-off until Safari 26 / macOS 26) and
not required by any of the three items above. Gate any WebGPU path on
`probeWebGpuAdapter()` returning true at runtime, with a WebGL2 fallback.

## Tests

`apps/desktop/tests/webviewcaps.test.ts` (vitest) stubs `OffscreenCanvas`,
`Worker`, `createImageBitmap`, canvas `getContext`, and `navigator.gpu`
present and absent, asserts each structured verdict, and proves the probe
never throws (a throwing `getContext` is coerced to `false`).
