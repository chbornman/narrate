/**
 * Look rows — STAGE B OWNS THIS FILE. Prev/next, the §0 symmetry row
 * (Space closes Look at fit; the zoomed-Space hold-to-pan / clean-tap-
 * close split is a pointer-pipeline fact — logic/looknav.ts), the zoom
 * band (Z anchored at pointer and the dblclick toggle are LookStage
 * pointer facts over the same Actions), R member flip (featureset §5),
 * and the filmstrip. The look-backdrop seat is complete by construction:
 * zoom-toggle/zoom-fit/zoom-100 seated here + the global set-surround row
 * fill menus.ts's frozen look-backdrop order table.
 *
 * Reserved M2a band (P/E/V, O): registry rows that dispatch to NOTHING —
 * hidden from menus and the cheatsheet until the pencil packet lights
 * them up (featureset §9; ActionDef.reserved).
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
    id: "look-close",
    verb: "Close",
    label: "Close Look (Space at fit / Esc)",
    keys: [{ key: " " }],
    scope: "look",
    group: "look",
    available: always,
    // While zoomed, Space is hold-to-pan (pointer pipeline, Stage B) —
    // the §3 showcase: no special-case branch in any component.
    enabled: (ctx) => ctx.lookAtFit && !ctx.railFocused,
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
  {
    id: "toggle-filmstrip",
    verb: "Filmstrip",
    keys: [{ key: "f" }],
    scope: "look",
    group: "look",
    seats: ["look-backdrop"], // pointer reachability (dogfood round 1)
    available: always,
    enabled: lookKeysFree,
    checked: (ctx) => ctx.filmstrip,
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
  // ---- reserved M2a band (P/E/V + overlay cycle): dispatch to nothing ----
  {
    id: "pencil-pen",
    verb: "Pencil",
    keys: [{ key: "p" }],
    scope: "look",
    group: "look",
    available: always,
    reserved: true,
  },
  {
    id: "pencil-eraser",
    verb: "Eraser",
    keys: [{ key: "e" }],
    scope: "look",
    group: "look",
    available: always,
    reserved: true,
  },
  {
    id: "pencil-visibility",
    verb: "Strokes visibility",
    keys: [{ key: "v" }],
    scope: "look",
    group: "look",
    available: always,
    reserved: true,
  },
  {
    id: "cycle-overlay",
    verb: "Overlay",
    keys: [{ key: "o" }],
    scope: "look",
    group: "look",
    available: always,
    reserved: true,
  },
];
