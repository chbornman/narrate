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
import { defaultToggles, type SignalToggles } from "../logic/ranking";
import { DEFAULT_SORT, DEFAULT_THUMB_STEP, type SortMode } from "../logic/sort";
import {
  DEFAULT_SURROUND,
  DEFAULT_SURROUND_MODE,
  parseSurround,
  parseSurroundMode,
  type SurroundLevel,
  type SurroundMode,
} from "../theme/surround";
import { DEFAULT_THEME, parseTheme, type ThemeMode } from "../theme/theme";

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

/** Rail tab — Folders | Collections | Topics as peers (founder, June 2026;
 * Topics added June 13 2026 per DESIGN-TOPICS-COLLECTIONS.md). Persisted like
 * railOpen: the rail comes back the way it was left. */
export type RailTab = "folders" | "collections" | "topics";

export function loadRailTab(): RailTab {
  const v = safeGet("pp.railTab");
  return v === "collections" || v === "topics" ? v : "folders";
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

// ---- fuzzy quiet-toggle (search-as-scope Phase 4) ---------------------------

/** The `~` fuzzy quiet-toggle, default OFF (never default-on — the whole point
 * of the feature). Persisted across the session like every other UI pref so an
 * armed user keeps it armed; an absent/malformed value falls back to off. */
export function loadFuzzy(): boolean {
  return loadBool("pp.fuzzy", false);
}

export function saveFuzzy(on: boolean) {
  saveBool("pp.fuzzy", on);
}

export function loadSurround(): SurroundLevel {
  return parseSurround(safeGet("pp.surround")) ?? DEFAULT_SURROUND;
}

export function saveSurround(level: SurroundLevel) {
  safeSet("pp.surround", level);
}

/** Surround MODE (follow-theme | manual), default follow-theme: a fresh
 * install derives the image backdrop from the active light/dark theme until
 * the user pins a level. A malformed/absent value falls back to follow-theme
 * (see theme/surround.ts). The pinned MANUAL level reuses the existing
 * `pp.surround` key above, so an old install's level is honored the moment it
 * switches to manual. */
export function loadSurroundMode(): SurroundMode {
  return parseSurroundMode(safeGet("pp.surroundMode")) ?? DEFAULT_SURROUND_MODE;
}

export function saveSurroundMode(mode: SurroundMode) {
  safeSet("pp.surroundMode", mode);
}

// ---- interface theme (BACKLOG "Full interface themes") ----------------------

/** App-chrome theme: system | light | dark, default system (follows the OS).
 * Orthogonal to surround (the image backdrop); see theme/theme.ts. A malformed
 * or absent value falls back to `system`, so an old/junk blob never strands
 * the app in a theme the user can't see to fix. */
export function loadTheme(): ThemeMode {
  return parseTheme(safeGet("pp.theme")) ?? DEFAULT_THEME;
}

export function saveTheme(mode: ThemeMode) {
  safeSet("pp.theme", mode);
}

// ---- attention/engagement heatmap (DESIGN-ATTENTION-HEATMAP.md) -------------

/** Grid heat-tint toggle, default OFF (a reviewing aid, like the histogram).
 * Persisted across the session like every other UI toggle so a reviewer who
 * wants it keeps it. */
export function loadHeatOn(): boolean {
  return loadBool("pp.heatOn", false);
}

export function saveHeatOn(on: boolean) {
  saveBool("pp.heatOn", on);
}

/** "All-time" recency switch (founder decision), default OFF = recency-weighted
 * ("what am I working on now"); ON = flat all-time ("what mattered most ever").
 * Persisted like the heat toggle. */
export function loadHeatAllTime(): boolean {
  return loadBool("pp.heatAllTime", false);
}

export function saveHeatAllTime(on: boolean) {
  saveBool("pp.heatAllTime", on);
}

// ---- near-duplicate lens (DESIGN-DEDUP-AND-SIMILARITY.md "Tier 1") ----------

/** The Duplicates lens toggle, default OFF (opt-in, destructive-adjacent — the
 * design doc's "Opt-in" rule). Persisted like the other review-aid toggles so a
 * reviewer mid-cull keeps it across a relaunch. */
export function loadDupesOn(): boolean {
  return loadBool("pp.dupesOn", false);
}

export function saveDupesOn(on: boolean) {
  saveBool("pp.dupesOn", on);
}

/** The looseness-slider value (Hamming threshold over the 64-bit perceptual
 * hash), persisted so a sweep the founder settled on survives the session.
 * `fallback` is the caller's calibrated default (tuning.DEDUP_THRESHOLD_DEFAULT)
 * so the one-true default lives in the tuning registry, not here. A stored
 * non-number / out-of-nowhere value falls back rather than feeding the backend
 * garbage. */
export function loadDupeThreshold(fallback: number): number {
  const v = Number(safeGet("pp.dupeThreshold"));
  return Number.isFinite(v) && v >= 0 ? v : fallback;
}

export function saveDupeThreshold(value: number) {
  safeSet("pp.dupeThreshold", String(value));
}

// ---- semantic graph: attention overlay (heatmap x graph synthesis) ----------

/** The three-state Attention overlay on the topic-graph (Off / Engaged /
 * Overlooked), default OFF = the plain graph. Persisted across the session like
 * the other graph + heatmap toggles so a reviewer who leaves the overlay on the
 * "Overlooked" view returns to it. A malformed/absent value falls back to off. */
export type AttentionMode = "off" | "engaged" | "overlooked";

export function loadAttentionMode(): AttentionMode {
  const v = safeGet("pp.graphAttention");
  return v === "engaged" || v === "overlooked" ? v : "off";
}

export function saveAttentionMode(mode: AttentionMode) {
  safeSet("pp.graphAttention", mode);
}

// ---- semantic graph: per-topic influence field ------------------------------

/** The per-topic INFLUENCE FIELD layer on the topic-graph (the soft colored
 * glow showing each topic's power level where its strong images landed), default
 * OFF. It is a SEPARATE layer from the Attention overlay (that one is attention;
 * this is topic-affinity power) and the two coexist. Default off so the plain
 * graph stays the first read and the founder opts INTO the painted view;
 * persisted across the session like the other graph toggles. */
export function loadGraphField(): boolean {
  return loadBool("pp.graphField", false);
}

export function saveGraphField(on: boolean) {
  saveBool("pp.graphField", on);
}

// ---- ranking signals (search-as-scope Phase 3) ------------------------------

/** The ⚙ "Ranking signals" on/off state, persisted across the session like
 * every other UI pref. Default = all signals checked (the B75 defaults); a
 * malformed or absent value falls back to that. The keys mirror the
 * FusionWeights fields so the mapping in logic/ranking.ts can read them. */
export function loadSignalToggles(): SignalToggles {
  const fallback = defaultToggles();
  const v = safeGet("pp.signalToggles");
  if (v === null) return fallback;
  try {
    const parsed = JSON.parse(v) as Partial<Record<keyof SignalToggles, unknown>>;
    // Read each key defensively: an old/partial blob keeps the on default for
    // any missing or non-boolean signal — a signal is never silently dropped.
    return {
      s1: typeof parsed.s1 === "boolean" ? parsed.s1 : fallback.s1,
      s2: typeof parsed.s2 === "boolean" ? parsed.s2 : fallback.s2,
      s3_each: typeof parsed.s3_each === "boolean" ? parsed.s3_each : fallback.s3_each,
      s4: typeof parsed.s4 === "boolean" ? parsed.s4 : fallback.s4,
    };
  } catch {
    return fallback;
  }
}

export function saveSignalToggles(t: SignalToggles) {
  safeSet("pp.signalToggles", JSON.stringify(t));
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
