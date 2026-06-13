/**
 * Global registry rows — the contract keys (featureset §0), capture keys,
 * surround (D6), and system rows. Owned by FOUNDATIONS; frozen during
 * parallel work.
 */
import type { ActionDef } from "../types";
import {
  SURROUND_LABELS,
  SURROUND_LEVELS,
  type SurroundLevel,
} from "../../theme/surround";

const always = () => true;

export const GLOBAL_DEFS: ActionDef[] = [
  {
    id: "escape",
    verb: "Back",
    label: "Back one layer (exits text first; never quits)",
    keys: [{ key: "Escape" }],
    scope: "global",
    group: "contract",
    available: always,
    worksInInput: true,
  },
  {
    id: "go-grid",
    verb: "Go to Grid",
    label: "Go to Grid from anywhere",
    keys: [{ key: "g" }],
    scope: "global",
    group: "contract",
    // The pointer path OUT of Look (dogfood round 1 reachability audit).
    seats: ["look-backdrop"],
    available: always,
  },
  {
    id: "toggle-graph",
    verb: "Topic graph",
    label: "Open or close the topic graph lens (semantic map)",
    keys: [{ key: "l" }],
    scope: "global",
    group: "panels",
    seats: ["gutter"],
    available: always,
  },
  {
    id: "toggle-lights-out",
    verb: "Lights-out",
    label: "Hide all chrome (Tab again restores)",
    keys: [{ key: "Tab" }],
    scope: "global",
    group: "contract",
    seats: ["gutter", "look-backdrop"],
    available: always,
    checked: (ctx) => ctx.chromeHidden,
  },
  {
    id: "toggle-rail",
    verb: "Toggle source rail",
    keys: [{ key: "\\" }],
    scope: "global",
    group: "panels",
    // Pointer path: the titlebar rail button (Titlebar.svelte dispatches
    // through resolveAction — same row, zero new verbs).
    available: always,
  },
  {
    id: "toggle-filmstrip",
    verb: "Filmstrip",
    label: "Filmstrip (current scope, bottom edge)",
    keys: [{ key: "f" }],
    // GLOBAL since the layout-architecture round (founder, June 12 2026):
    // F is total — the filmstrip is a center-column bottom panel in BOTH
    // surfaces (in Grid it walks the grid's own units, focused cell
    // highlighted). Was scope "look" — the founder's "F works sometimes".
    scope: "global",
    group: "panels",
    // Pointer reachability: gutter (Grid) + look-backdrop (Look).
    seats: ["gutter", "look-backdrop"],
    available: always,
    enabled: (ctx) => !ctx.railFocused,
    checked: (ctx) => ctx.filmstrip,
  },
  {
    id: "toggle-cheatsheet",
    verb: "Keyboard map",
    keys: [{ key: "?" }, { key: "F1" }],
    scope: "global",
    group: "contract",
    seats: ["gutter", "look-backdrop"],
    available: always,
  },
  {
    id: "open-search",
    verb: "Search",
    keys: [{ key: "/" }, { key: "f", ctrlOrMeta: true }],
    scope: "global",
    group: "search",
    available: always,
    worksInInput: true, // Ctrl+F only — "/" types (match.ts chord rule)
  },
  {
    id: "summon-note",
    verb: "Write a note",
    keys: [{ key: "n" }],
    scope: "global",
    group: "capture",
    available: always,
  },
  {
    id: "rate",
    verb: "Rate",
    label: "Rate 0-5 (0 clears)",
    keys: [0, 1, 2, 3, 4, 5].map((v) => ({ key: String(v), arg: v })),
    scope: "global",
    group: "capture",
    seats: ["thumb"],
    available: always,
    // Grid needs a selection; Look always rates the viewed image
    // (CAPTURE §10: session scope → rating keys do nothing).
    enabled: (ctx) => ctx.surface === "look" || ctx.hasSelection,
    toAction: (_ctx, arg) => ({ kind: "rate", value: arg as number }),
    options: () =>
      [0, 1, 2, 3, 4, 5].map((v) => ({
        arg: v,
        label: v === 0 ? "Clear (0)" : "★".repeat(v),
      })),
  },
  {
    id: "toggle-auto-advance",
    verb: "Auto-advance",
    label: "Auto-advance after rate/note (visible mode)",
    keys: [{ key: "a" }],
    scope: "global",
    group: "capture",
    seats: ["gutter"],
    available: always,
    checked: (ctx) => ctx.autoAdvance,
  },
  {
    id: "set-surround",
    verb: "Surround",
    label: "Image surround luminance",
    keys: [], // pointer-only: backdrop/gutter right-click (D6)
    scope: "global",
    group: "system",
    seats: ["gutter", "look-backdrop"],
    available: always,
    toAction: (_ctx, arg) => ({ kind: "set-surround", level: arg as SurroundLevel }),
    options: (ctx) =>
      SURROUND_LEVELS.map((level) => ({
        arg: level,
        label: SURROUND_LABELS[level],
        checked: ctx.surround === level,
      })),
  },
  {
    id: "toggle-fullscreen",
    verb: "Fullscreen",
    keys: [{ key: "F11" }],
    scope: "global",
    group: "system",
    seats: ["gutter", "look-backdrop"],
    available: always,
    worksInInput: true,
  },
  // UI scale (desktop-conventions pass, June 2026): Cmd-modified zoom
  // chords scale the WEBVIEW (the whole chrome); Look's IMAGE zoom stays
  // on plain +/−/= (defs/look.ts zoom-step) — no collision by modifier.
  // Cmd+0 IS reused by Look's zoom-fit: scope precedence (look > global,
  // match.ts) keeps Cmd+0 the image-fit key inside Look — the native View
  // menu's Reset Zoom row still resets the UI scale from anywhere.
  {
    id: "ui-zoom",
    verb: "Zoom UI in / out",
    label: "Scale the whole UI (Look's image zoom is plain +/−)",
    keys: [
      { key: "=", ctrlOrMeta: true, arg: 1 },
      { key: "+", ctrlOrMeta: true, arg: 1 }, // Cmd+Shift+= arrives as "+"
      { key: "-", ctrlOrMeta: true, arg: -1 },
    ],
    scope: "global",
    group: "system",
    available: always,
    worksInInput: true,
    toAction: (_ctx, arg) => ({ kind: "ui-zoom", delta: arg as 1 | -1 }),
  },
  {
    id: "ui-zoom-reset",
    verb: "Reset UI zoom",
    keys: [{ key: "0", ctrlOrMeta: true }],
    scope: "global",
    group: "system",
    available: always,
    worksInInput: true,
  },
  {
    id: "open-settings",
    verb: "Settings…",
    keys: [{ key: ",", ctrlOrMeta: true }],
    scope: "global",
    group: "system",
    seats: ["gutter", "look-backdrop"],
    available: always,
    worksInInput: true,
  },
  {
    id: "quit",
    verb: "Quit",
    keys: [{ key: "q", ctrlOrMeta: true }],
    scope: "global",
    group: "system",
    available: always,
    worksInInput: true,
  },
  {
    id: "toggle-debug-panel",
    verb: "Debug panel",
    keys: [{ key: "F12" }],
    scope: "global",
    group: "system",
    available: (ctx) => ctx.debugEnabled,
    worksInInput: true,
  },
  {
    id: "toggle-station-detail",
    verb: "What's happening",
    label: "Pin the station's status detail open",
    keys: [], // pointer-only: the station's info seat (Station.svelte)
    scope: "global",
    group: "system",
    // The seat itself appears only when activities exist (station model);
    // the VERB is always available — pinning an empty detail is a no-op
    // body, not an error, and availability gating belongs to defs, never
    // to a component's whim about what state the shell happens to hold.
    available: always,
  },
  {
    // ONE mic verb, two input forms (June 12 2026 ruling: SPACE is 100%
    // the microphone key — "like a Zoom call"; M went back to the
    // reserved pool). The Space KEYDOWN dispatches `mic-press`, which
    // begins the tap-vs-hold machine (logic/michold.ts: tap toggles, hold
    // is push-to-talk; release/blur are raw window facts in App.svelte —
    // the hold-E precedent). The station mic seat click resolves with
    // arg "toggle" for the instantaneous `toggle-mic` — a click IS a tap,
    // so the pointer form keeps plain toggle semantics with zero new
    // verbs. Space's former verbs were displaced for this (founder-
    // accepted): grid open-look is Enter-only, Look closes on Esc only,
    // and the Look Space-pan pipeline is deleted. Typing safety needs no
    // special case: match.ts's §11 rule suppresses any chord that CAN
    // TYPE while an input is focused, and " " types a space.
    id: "mic-press",
    verb: "Microphone",
    label: "Microphone - tap toggles, hold to talk",
    keys: [{ key: " " }],
    scope: "global",
    group: "capture",
    // Live since P6.4 (the cpal device + supervised sherpa client are
    // wired). Gated on the §8.3 readiness flag: the row — and the Space
    // key — exists only while the supervised ASR child reports Ready.
    available: (ctx) => ctx.asrReady,
    toAction: (_ctx, arg) =>
      arg === "toggle" ? { kind: "toggle-mic" } : { kind: "mic-press" },
  },
];
