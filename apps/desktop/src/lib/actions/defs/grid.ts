/**
 * Grid rows — STAGE A OWNS THIS FILE. The legacy rows (selection,
 * open-look, sort, sizes) plus the two amended contract rows landed with
 * FOUNDATIONS:
 *   · Space opens Look (featureset §0 symmetric open/close; supersedes
 *     UI.md §3.4 Space-toggle) — selection-toggle moved to Ctrl+Space.
 *   · D4 OS rows are seated on the thumb menu (bodies in os.rs).
 * Stage A adds: T cell-info, Home/End/PgUp/PgDn edge/page moves, the
 * stack verbs (per-pair toggle on the thumb seat, collapse/expand-all on
 * the gutter's Stacks ▸, R display flip). Marquee is pointer-only by
 * nature (logic/marquee.ts; Ctrl-additive is a modifier, not a chord).
 */
import type { ActionDef } from "../types";
import { SORT_MODES, THUMB_STEPS, type SortMode } from "../../logic/sort";

type Dir = "up" | "down" | "left" | "right";

const always = () => true;
const gridKeysFree = (ctx: { railFocused: boolean }) => !ctx.railFocused;

export const GRID_DEFS: ActionDef[] = [
  {
    id: "open-look",
    verb: "Open",
    label: "Open in Look",
    keys: [{ key: "Enter" }, { key: " " }],
    scope: "grid",
    group: "grid",
    seats: ["thumb"],
    available: always,
    enabled: gridKeysFree, // Enter opens the focused FOLDER while the rail has focus
  },
  {
    id: "focus-move",
    verb: "Move focus",
    label: "Move active image (Shift extends)",
    keys: [
      { key: "ArrowUp", shift: false, arg: { dir: "up", extend: false } },
      { key: "ArrowDown", shift: false, arg: { dir: "down", extend: false } },
      { key: "ArrowLeft", shift: false, arg: { dir: "left", extend: false } },
      { key: "ArrowRight", shift: false, arg: { dir: "right", extend: false } },
      { key: "ArrowUp", shift: true, arg: { dir: "up", extend: true } },
      { key: "ArrowDown", shift: true, arg: { dir: "down", extend: true } },
      { key: "ArrowLeft", shift: true, arg: { dir: "left", extend: true } },
      { key: "ArrowRight", shift: true, arg: { dir: "right", extend: true } },
    ],
    scope: "grid",
    group: "grid",
    available: always,
    enabled: gridKeysFree,
    toAction: (_ctx, arg) => {
      const a = arg as { dir: Dir; extend: boolean };
      return { kind: "focus-move", dir: a.dir, extend: a.extend };
    },
  },
  {
    id: "toggle-select-focused",
    verb: "Toggle selection",
    label: "Toggle selection on the active image",
    keys: [{ key: " ", ctrlOrMeta: true }],
    scope: "grid",
    group: "grid",
    available: always,
    enabled: gridKeysFree,
  },
  {
    id: "select-all",
    verb: "Select all",
    keys: [{ key: "a", ctrlOrMeta: true, shift: false }],
    scope: "grid",
    group: "grid",
    seats: ["gutter"],
    available: always,
  },
  {
    id: "select-none",
    verb: "Select none",
    keys: [{ key: "a", ctrlOrMeta: true, shift: true }],
    scope: "grid",
    group: "grid",
    seats: ["gutter"],
    available: always,
    enabled: (ctx) => ctx.hasSelection,
  },
  {
    id: "focus-edge",
    verb: "Jump to start / end",
    label: "Jump to first / last image (Shift extends)",
    keys: [
      { key: "Home", shift: false, arg: { edge: "home", extend: false } },
      { key: "End", shift: false, arg: { edge: "end", extend: false } },
      { key: "Home", shift: true, arg: { edge: "home", extend: true } },
      { key: "End", shift: true, arg: { edge: "end", extend: true } },
    ],
    scope: "grid",
    group: "grid",
    available: always,
    enabled: gridKeysFree,
    toAction: (_ctx, arg) => {
      const a = arg as { edge: "home" | "end"; extend: boolean };
      return { kind: "focus-edge", edge: a.edge, extend: a.extend };
    },
  },
  {
    id: "focus-page",
    verb: "Page up / down",
    label: "Move one viewport of rows (Shift extends)",
    keys: [
      { key: "PageUp", shift: false, arg: { dir: "up", extend: false } },
      { key: "PageDown", shift: false, arg: { dir: "down", extend: false } },
      { key: "PageUp", shift: true, arg: { dir: "up", extend: true } },
      { key: "PageDown", shift: true, arg: { dir: "down", extend: true } },
    ],
    scope: "grid",
    group: "grid",
    available: always,
    enabled: gridKeysFree,
    toAction: (_ctx, arg) => {
      const a = arg as { dir: "up" | "down"; extend: boolean };
      return { kind: "focus-page", dir: a.dir, extend: a.extend };
    },
  },
  {
    id: "cycle-cell-info",
    verb: "Cell info",
    label: "Cycle cell info (none → minimal → annotated)",
    keys: [{ key: "t" }],
    scope: "grid",
    group: "grid",
    available: always,
    enabled: gridKeysFree,
  },
  // ---- stacks (featureset §5, D1: live, reversible, display-level) --------
  {
    id: "stack-toggle-active",
    verb: "Collapse / expand stack",
    keys: [], // pointer-only: the chevron control + the thumb seat
    scope: "grid",
    group: "grid",
    seats: ["thumb"],
    available: always,
    enabled: (ctx) => ctx.activeIsPair,
  },
  {
    id: "stack-collapse-all",
    verb: "Collapse all",
    label: "Collapse all stacks",
    keys: [], // pointer-only: the gutter Stacks ▸ submenu
    scope: "grid",
    group: "grid",
    seats: ["gutter"],
    available: always,
  },
  {
    id: "stack-expand-all",
    verb: "Expand all",
    label: "Expand all stacks",
    keys: [], // pointer-only: the gutter Stacks ▸ submenu
    scope: "grid",
    group: "grid",
    seats: ["gutter"],
    available: always,
  },
  {
    id: "flip-stack-member",
    verb: "Flip RAW / JPEG",
    label: "Show the other stack member (collapsed pair)",
    keys: [{ key: "r" }],
    scope: "grid",
    group: "grid",
    available: always,
    // Only a COLLAPSED pair has a hidden member to flip to (the Look-side
    // R row is Stage B's, in defs/look.ts).
    enabled: (ctx) => !ctx.railFocused && ctx.activeIsPair && ctx.activePairCollapsed,
  },
  {
    id: "open-sort-menu",
    verb: "Sort",
    label: "Open the sort menu",
    keys: [{ key: "s" }],
    scope: "grid",
    group: "grid",
    available: always,
    enabled: gridKeysFree,
  },
  {
    id: "set-sort",
    verb: "Sort",
    keys: [], // pointer-only: sort ▾ and the gutter seat
    scope: "grid",
    group: "grid",
    seats: ["gutter"],
    available: always,
    toAction: (_ctx, arg) => ({ kind: "set-sort", mode: arg as SortMode }),
    options: (ctx) =>
      SORT_MODES.map(({ mode, label }) => ({
        arg: mode,
        label,
        checked: ctx.sort === mode,
      })),
  },
  {
    id: "thumb-size",
    verb: "Thumbnail size",
    label: "Thumbnail size down / up",
    keys: [
      { key: "-", arg: -1 },
      { key: "=", arg: 1 },
    ],
    scope: "grid",
    group: "grid",
    available: always,
    enabled: gridKeysFree,
    toAction: (_ctx, arg) => ({ kind: "thumb-size", delta: arg as 1 | -1 }),
  },
  {
    id: "set-thumb-step",
    verb: "Size",
    keys: [], // pointer-only: slider + the gutter seat
    scope: "grid",
    group: "grid",
    seats: ["gutter"],
    available: always,
    toAction: (_ctx, arg) => ({ kind: "set-thumb-step", step: arg as number }),
    options: (ctx) =>
      THUMB_STEPS.map((px, step) => ({
        arg: step,
        label: `${px} px`,
        checked: ctx.thumbStep === step,
      })),
  },
  // ---- D4 OS integration (pointer-seated; bodies land with Stage A os.rs)
  {
    id: "reveal-in-file-manager",
    verb: "Show in file manager",
    keys: [],
    scope: "grid",
    group: "system",
    seats: ["thumb"],
    available: always,
    enabled: (ctx) => ctx.activeHash !== null,
  },
  {
    id: "copy-file-path",
    verb: "Copy file path",
    keys: [],
    scope: "grid",
    group: "system",
    seats: ["thumb"],
    available: always,
    enabled: (ctx) => ctx.activeHash !== null,
  },
  {
    id: "open-with-default",
    verb: "Open with default app",
    keys: [],
    scope: "grid",
    group: "system",
    seats: ["thumb"],
    available: always,
    enabled: (ctx) => ctx.activeHash !== null,
  },
];
