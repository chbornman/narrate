/**
 * Source-rail rows: keyboard navigation (gated on railFocused — the rail
 * is push-persistent, so openness no longer implies focus; amended
 * expectation 3 in keymap.test.ts) and the rail-folder context-menu seat
 * (Open · Show in file manager · Rescan — featureset §6).
 */
import type { ActionDef } from "../types";

type Dir = "up" | "down" | "left" | "right";

const always = () => true;

/** Seat argument for "rail-folder": the row the menu was opened on. */
export interface RailFolderArg {
  rootId: string;
  folder: string;
}

const railArg = (arg: unknown): RailFolderArg | null =>
  arg !== null && typeof arg === "object" && "rootId" in (arg as object)
    ? (arg as RailFolderArg)
    : null;

export const RAIL_DEFS: ActionDef[] = [
  {
    id: "rail-nav",
    verb: "Navigate folders",
    keys: [
      { key: "ArrowUp", arg: "up" },
      { key: "ArrowDown", arg: "down" },
      { key: "ArrowLeft", arg: "left" },
      { key: "ArrowRight", arg: "right" },
    ],
    scope: "global",
    group: "panels",
    available: (ctx) => ctx.railOpen,
    enabled: (ctx) => ctx.railFocused,
    toAction: (_ctx, arg) => ({ kind: "rail-nav", dir: arg as Dir }),
  },
  {
    id: "rail-enter",
    verb: "Open folder",
    keys: [{ key: "Enter" }],
    scope: "global",
    group: "panels",
    available: (ctx) => ctx.railOpen,
    enabled: (ctx) => ctx.railFocused,
  },
  // ---- rail-folder seat (pointer-only rows; arg = the clicked row) -------
  {
    id: "rail-folder-open",
    verb: "Open",
    keys: [],
    scope: "global",
    group: "panels",
    seats: ["rail-folder"],
    available: always,
    toAction: (_ctx, arg) => {
      const a = railArg(arg);
      return a === null ? null : { kind: "rail-folder-open", ...a };
    },
  },
  {
    id: "rail-folder-reveal",
    verb: "Show in file manager",
    keys: [],
    scope: "global",
    group: "panels",
    seats: ["rail-folder"],
    available: always,
    toAction: (_ctx, arg) => {
      const a = railArg(arg);
      return a === null ? null : { kind: "rail-folder-reveal", ...a };
    },
  },
  {
    id: "rescan-root",
    verb: "Rescan",
    keys: [],
    scope: "global",
    group: "panels",
    seats: ["rail-folder"],
    available: always,
    toAction: (_ctx, arg) => {
      const a = railArg(arg);
      return a === null ? null : { kind: "rescan-root", rootId: a.rootId };
    },
  },
  // The rail's own verb (footer button + this seat): the OS folder picker
  // → add_root, no Settings round-trip (founder, dogfood rounds 1+2).
  {
    id: "add-root",
    verb: "Add folder…",
    label: "Add a watched folder",
    keys: [],
    scope: "global",
    group: "panels",
    seats: ["rail-folder"],
    available: always,
  },
];
