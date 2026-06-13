/**
 * UI preference persistence (implementation latitude per UI.md):
 * localStorage — small, synchronous, webview-local. ALL P4.2 keys are
 * declared up front (contract freeze): sort per folder · thumbnail size ·
 * filmstrip · surround (D6) · auto-advance (D7, default OFF) · cell-info
 * level · panel sizes + rail open · global stack collapse · last folder.
 * The rail PIN pref is gone with the pin affordance (the rail is
 * push-persistent — D5/DECISIONS 3).
 */
import { clampSize } from "../primitives/panel";
import { DEFAULT_SORT, DEFAULT_THUMB_STEP, type SortMode } from "../logic/sort";
import {
  DEFAULT_SURROUND,
  parseSurround,
  type SurroundLevel,
} from "../theme/surround";

const safeGet = (k: string): string | null => {
  try {
    return localStorage.getItem(k);
  } catch {
    return null;
  }
};

const safeSet = (k: string, v: string) => {
  try {
    localStorage.setItem(k, v);
  } catch {
    /* private-mode etc.: prefs simply don't persist */
  }
};

const loadBool = (k: string, fallback: boolean): boolean => {
  const v = safeGet(k);
  return v === null ? fallback : v === "1";
};

const saveBool = (k: string, v: boolean) => safeSet(k, v ? "1" : "0");

// ---- sort (per folder) ------------------------------------------------------

export function loadSort(rootId: string, folder: string): SortMode {
  const v = safeGet(`pp.sort.${rootId}/${folder}`);
  if (v === "capture-desc" || v === "capture-asc" || v === "filename" || v === "added")
    return v;
  return DEFAULT_SORT;
}

export function saveSort(rootId: string, folder: string, mode: SortMode) {
  safeSet(`pp.sort.${rootId}/${folder}`, mode);
}

// ---- grid -------------------------------------------------------------------

export function loadThumbStep(): number {
  const v = Number(safeGet("pp.thumbStep"));
  return Number.isInteger(v) && v >= 0 && v <= 3 ? v : DEFAULT_THUMB_STEP;
}

export function saveThumbStep(step: number) {
  safeSet("pp.thumbStep", String(step));
}

export type CellInfoLevel = "none" | "minimal" | "annotated";

export function loadCellInfo(): CellInfoLevel {
  const v = safeGet("pp.cellInfo");
  return v === "minimal" || v === "annotated" ? v : "none";
}

export function saveCellInfo(level: CellInfoLevel) {
  safeSet("pp.cellInfo", level);
}

/** Global stack collapse (featureset §5 — live, reversible). */
export function loadStackGlobal(): boolean {
  return loadBool("pp.stackGlobal", true);
}

export function saveStackGlobal(collapsed: boolean) {
  saveBool("pp.stackGlobal", collapsed);
}

// ---- panels -----------------------------------------------------------------

/** One frame contract for every edge panel (founder, June 12 2026): the
 * canvas is always the center; rail (left), inspector (right), and
 * filmstrip (bottom of the center column) are peers with ONE remembered
 * size each — GLOBAL, never per-surface (the filmstrip is the same panel
 * in Grid and Look). min/max clamp drags AND junk stored values;
 * defaultSize is the double-click-the-handle reset. */
export interface PanelSpec {
  defaultSize: number;
  minSize: number;
  maxSize: number;
  /** Pre-refactor width key (P4.2), read once as a fallback so existing
   * installs keep their widths across the layout-architecture round. */
  legacyKey?: string;
}

export const PANEL_SPECS = {
  rail: { defaultSize: 240, minSize: 160, maxSize: 420, legacyKey: "pp.railWidth" },
  inspector: {
    defaultSize: 320,
    minSize: 260,
    maxSize: 520,
    legacyKey: "pp.inspectorWidth",
  },
  filmstrip: { defaultSize: 80, minSize: 56, maxSize: 200 },
} as const satisfies Record<string, PanelSpec>;

export type PanelId = keyof typeof PANEL_SPECS;

export function loadPanelSize(id: PanelId): number {
  const spec: PanelSpec = PANEL_SPECS[id];
  const raw =
    safeGet(`pp.panel.${id}.size`) ??
    (spec.legacyKey === undefined ? null : safeGet(spec.legacyKey));
  const v = Number(raw);
  return raw !== null && Number.isFinite(v)
    ? clampSize(v, spec.minSize, spec.maxSize)
    : spec.defaultSize;
}

export function savePanelSize(id: PanelId, size: number) {
  safeSet(`pp.panel.${id}.size`, String(size));
}

export function loadRailOpen(): boolean {
  return loadBool("pp.railOpen", true);
}

export function saveRailOpen(open: boolean) {
  saveBool("pp.railOpen", open);
}

/** Rail tab — Folders | Collections as peers (founder, June 2026).
 * Persisted like railOpen: the rail comes back the way it was left. */
export type RailTab = "folders" | "collections";

export function loadRailTab(): RailTab {
  return safeGet("pp.railTab") === "collections" ? "collections" : "folders";
}

export function saveRailTab(tab: RailTab) {
  safeSet("pp.railTab", tab);
}

// Inspector OPENNESS deliberately does not persist (DECISIONS 3): width
// does (the inspector PanelSpec), openness resets each launch.

// ---- look -------------------------------------------------------------------

export function loadFilmstrip(): boolean {
  return loadBool("pp.filmstrip", false);
}

export function saveFilmstrip(on: boolean) {
  saveBool("pp.filmstrip", on);
}

/** Look histogram overlay (H), default OFF. A reviewing aid (exposure /
 * clipping check), not an editing tool; remembered across the session like
 * the filmstrip so a reviewer who wants it keeps it. */
export function loadHistogram(): boolean {
  return loadBool("pp.histogram", false);
}

export function saveHistogram(on: boolean) {
  saveBool("pp.histogram", on);
}

// ---- UI scale (desktop-conventions pass, June 2026) --------------------------

/** Webview-zoom ladder for Cmd+= / Cmd+− / Cmd+0 (browser-conventional
 * steps; 1 is the design size). A LADDER rather than a free factor so
 * repeated presses land on round, reproducible sizes — and so a persisted
 * value can be validated by membership instead of range-clamping. */
export const UI_ZOOM_STEPS = [0.8, 0.9, 1, 1.1, 1.25, 1.5] as const;

export function loadUiZoom(): number {
  const v = Number(safeGet("pp.uiZoom"));
  // Membership check: anything off-ladder (corrupt storage, an old build's
  // experiment) silently resets to the design size rather than rendering
  // the whole app at a weird scale forever.
  return (UI_ZOOM_STEPS as readonly number[]).includes(v) ? v : 1;
}

export function saveUiZoom(factor: number) {
  safeSet("pp.uiZoom", String(factor));
}

// ---- capture / viewing ------------------------------------------------------

/** Auto-advance, default OFF (D7). */
export function loadAutoAdvance(): boolean {
  return loadBool("pp.autoAdvance", false);
}

export function saveAutoAdvance(on: boolean) {
  saveBool("pp.autoAdvance", on);
}

export function loadSurround(): SurroundLevel {
  return parseSurround(safeGet("pp.surround")) ?? DEFAULT_SURROUND;
}

export function saveSurround(level: SurroundLevel) {
  safeSet("pp.surround", level);
}

// ---- first-run welcome card (BACKLOG: how your data is stored) ---------------

/** The card shows on launch until a dismissal carries the "don't show
 * again" toggle (default ON — the common path sees it exactly once). The
 * pref is webview-local like every other UI pref: a fresh machine rightly
 * gets the storage story again. */
export function loadWelcomeSeen(): boolean {
  return loadBool("pp.welcomeSeen", false);
}

export function saveWelcomeSeen(seen: boolean) {
  saveBool("pp.welcomeSeen", seen);
}

// ---- session ----------------------------------------------------------------

export function loadLastFolder(): { rootId: string; folder: string } | null {
  const v = safeGet("pp.lastFolder");
  if (!v) return null;
  try {
    const parsed = JSON.parse(v);
    if (typeof parsed.rootId === "string" && typeof parsed.folder === "string")
      return parsed;
  } catch {
    /* fallthrough */
  }
  return null;
}

export function saveLastFolder(rootId: string, folder: string) {
  safeSet("pp.lastFolder", JSON.stringify({ rootId, folder }));
}
