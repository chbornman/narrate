/**
 * UI preference persistence (implementation latitude per UI.md): sort per
 * folder, thumbnail size globally, rail pin, filmstrip. localStorage —
 * small, synchronous, webview-local.
 */
import { DEFAULT_SORT, DEFAULT_THUMB_STEP, type SortMode } from "../logic/sort";

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

export function loadSort(rootId: string, folder: string): SortMode {
  const v = safeGet(`pp.sort.${rootId}/${folder}`);
  if (v === "capture-desc" || v === "capture-asc" || v === "filename" || v === "added")
    return v;
  return DEFAULT_SORT;
}

export function saveSort(rootId: string, folder: string, mode: SortMode) {
  safeSet(`pp.sort.${rootId}/${folder}`, mode);
}

export function loadThumbStep(): number {
  const v = Number(safeGet("pp.thumbStep"));
  return Number.isInteger(v) && v >= 0 && v <= 3 ? v : DEFAULT_THUMB_STEP;
}

export function saveThumbStep(step: number) {
  safeSet("pp.thumbStep", String(step));
}

export function loadRailPinned(): boolean {
  return safeGet("pp.railPinned") === "1";
}

export function saveRailPinned(pinned: boolean) {
  safeSet("pp.railPinned", pinned ? "1" : "0");
}

export function loadFilmstrip(): boolean {
  return safeGet("pp.filmstrip") === "1";
}

export function saveFilmstrip(on: boolean) {
  safeSet("pp.filmstrip", on ? "1" : "0");
}

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
