/**
 * Look rows — STAGE B OWNS THIS FILE. Prev/next, the zoom band (Z
 * anchored at pointer and the dblclick toggle are LookStage pointer facts
 * over the same Actions), and R member flip (featureset §5). The
 * look-close row is GONE (June 12 2026: Space became 100% the microphone
 * key — defs/global.ts mic-press); Esc remains the close path through
 * the escape ladder (logic/escape.ts "leave-look"), exactly as before.
 * The zoomed Space-pan pipeline went with it — drag-pan is the only pan.
 * The filmstrip row moved to defs/global.ts the same day (founder: F is
 * total across Grid and Look). The look-backdrop seat is complete by
 * construction:
 * zoom-toggle/zoom-fit/zoom-100 seated here + the global set-surround row
 * fill menus.ts's frozen look-backdrop order table.
 *
 * The grease-pencil band (P5.1, M2a — CAPTURE §8, UI §4.4/§11): the P4.2
 * reserved rows reconciled onto the SPEC's keys — B = sticky pencil
 * toggle, hold-E = eraser, O = overlay toggle, Ctrl+Z = undo; the
 * redundant pencil-visibility (V) row collapsed into the single O toggle.
 */
import type { ActionDef } from "../types";

const always = () => true;
const lookKeysFree = (ctx: { railFocused: boolean }) => !ctx.railFocused;

export const LOOK_DEFS: ActionDef[] = [
  {
    id: "look-nav",
    verb: "Previous / next",
    keys: [
      { key: "ArrowLeft", arg: -1 },
      { key: "ArrowRight", arg: 1 },
    ],
    scope: "look",
    group: "look",
    available: always,
    enabled: lookKeysFree,
    toAction: (_ctx, arg) => ({ kind: "look-nav", delta: arg as 1 | -1 }),
  },
  {
    id: "zoom-toggle",
    verb: "Zoom fit ⇄ 100%",
    keys: [{ key: "z" }],
    scope: "look",
    group: "look",
    seats: ["look-backdrop"],
    available: always,
    enabled: lookKeysFree,
  },
  {
    id: "zoom-step",
    verb: "Zoom in / out",
    keys: [
      { key: "+", arg: 1 },
      { key: "=", shift: true, arg: 1 },
      { key: "-", arg: -1 },
    ],
    scope: "look",
    group: "look",
    available: always,
    enabled: lookKeysFree,
    toAction: (_ctx, arg) => ({ kind: "zoom-step", delta: arg as 1 | -1 }),
  },
  {
    id: "zoom-fit",
    verb: "Zoom to fit",
    keys: [{ key: "0", ctrlOrMeta: true }],
    scope: "look",
    group: "look",
    seats: ["look-backdrop"],
    available: always,
  },
  {
    id: "zoom-100",
    verb: "Zoom to 100%",
    keys: [{ key: "1", ctrlOrMeta: true }],
    scope: "look",
    group: "look",
    seats: ["look-backdrop"],
    available: always,
  },
  // toggle-filmstrip MOVED to defs/global.ts (founder, June 12 2026): F
  // is total — the strip is a center-column bottom panel in BOTH surfaces.
  {
    id: "toggle-histogram",
    verb: "Histogram",
    label: "Histogram overlay (exposure / clipping check)",
    keys: [{ key: "h" }],
    scope: "look",
    group: "look",
    seats: ["look-backdrop"], // pointer reachability, like the other toggles
    available: always,
    enabled: lookKeysFree,
    checked: (ctx) => ctx.histogram,
  },
  {
    id: "flip-stack-member",
    verb: "Flip RAW/JPEG",
    label: "Show the other stack member",
    keys: [{ key: "r" }],
    scope: "look",
    group: "look",
    seats: ["look-backdrop"], // pointer reachability (dogfood round 1)
    available: always,
    // Deliberately NOT gated on ctx.activeIsPair: that field tracks the
    // GRID's active unit and goes stale as ←/→ move through a Look
    // session; the look slice no-ops safely on a lone image (FRV
    // convention: the key is inert, never an error). Grid's R row is
    // Stage A's (same Action kind, scope "grid").
    enabled: lookKeysFree,
  },
  // ---- the grease pencil (P5.1 — CAPTURE §8, UI §4.4) --------------------
  {
    id: "pencil-pen",
    verb: "Pencil",
    label: "Grease pencil (sticky; resets on return to Grid)",
    keys: [{ key: "b" }],
    scope: "look",
    group: "capture",
    seats: ["look-backdrop"], // pointer reachability (R6 excludes only drawing)
    available: always,
    // A bound key must never be dead (BACKLOG, founder): with the overlay
    // hidden, B shows the paper AND arms the pencil in one keystroke — the
    // show-and-arm branch lives in the look slice (togglePencil), so the
    // row stays enabled regardless of overlay visibility.
    enabled: lookKeysFree,
    checked: (ctx) => ctx.pencilMode,
  },
  {
    id: "pencil-eraser",
    verb: "Eraser",
    label: "Eraser - hold E, tap a stroke to retract it",
    keys: [{ key: "e" }],
    scope: "look",
    group: "capture",
    available: always,
    // HOLD semantics: keydown engages through this row; the release is a
    // raw keyup fact in LookStage (the registry is keydown-only — the
    // same split the Space mic uses in App.svelte). Pointer path: the
    // stylus eraser end (named in the reachability audit).
    enabled: (ctx) => !ctx.railFocused && ctx.pencilMode,
  },
  {
    id: "pencil-undo",
    verb: "Undo stroke",
    label: "Undo last stroke (retraction; mid-draw it cancels, unlogged)",
    keys: [{ key: "z", ctrlOrMeta: true }],
    scope: "look",
    group: "capture",
    seats: ["look-backdrop"], // pointer reachability (supersedes U14's named exemption)
    available: always,
    // Empty stack and no pen down → the pencil layer must NOT swallow the
    // chord (CAPTURE §8.5: Ctrl+Z is a no-op in the pencil layer); the
    // same gate grays the menu row.
    enabled: (ctx) => !ctx.railFocused && ctx.pencilUndoable,
  },
  {
    id: "cycle-overlay",
    verb: "Overlay",
    label: "Tracing-paper overlay (hiding it exits pencil mode)",
    keys: [{ key: "o" }],
    scope: "look",
    group: "look",
    seats: ["look-backdrop"],
    available: always,
    enabled: lookKeysFree,
    checked: (ctx) => ctx.overlayVisible,
  },
  // Configurable external editor (BACKLOG "Configurable external editor, D4
  // revisit"): the Look-side def. SEPARATE from grid's because id is unique
  // per (id, scope) — same Action kind, scope "look" (the flip-stack-member
  // precedent). NO key; pointer-seated on the look backdrop. Empty pref =
  // OS default handler.
  {
    id: "open-in-external-editor",
    verb: "Open in external editor",
    keys: [],
    scope: "look",
    group: "system",
    seats: ["look-backdrop"],
    available: always,
    enabled: (ctx) => ctx.activeHash !== null,
  },
];
