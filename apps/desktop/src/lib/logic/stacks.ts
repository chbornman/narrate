/**
 * RAW+JPEG stacks (featureset §5, D1) — DISPLAY-LEVEL pairing only; the
 * data model keeps two images (K13). Pure functions over the sorted
 * GridItem list:
 *
 *   · auto-pair by basename + folder (one JPEG + one RAW, case-insensitive)
 *   · live, reversible collapse per pair AND globally (overrides XOR the
 *     global state — import-time-only pairing is the recorded anti-pattern)
 *   · collapsed preview = the PREFERRED member (Settings → "Stacked pairs
 *     show", JPEG by default — dogfood round 1); the other member hides as
 *     `alt`; an R flip swaps the displayed member (XOR the preference)
 *   · expandTargets(): a collapsed pair is ONE cell but TWO write-scope
 *     targets — display member first, then the hidden member (CAPTURE
 *     event_targets.position, DECISIONS 4; the indicator truthfully reads
 *     "● 2")
 *   · expandedLinks(): which EXPANDED members sit next to their partner
 *     (the grid draws them visually linked)
 */
import type { GridItem } from "../types/dto";
import type { DisplayUnit } from "../types/display";

/** The pairable extension classes. Anything else (PNG, HEIC, …) never
 * pairs — D1 is strictly RAW+JPEG. */
const JPEG_EXTS = new Set(["jpg", "jpeg", "jpe"]);
const RAW_EXTS = new Set([
  "3fr", "arw", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "iiq",
  "kdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "pef", "raf", "raw",
  "rw2", "rwl", "sr2", "srf", "srw", "x3f",
]);

function ext(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  return dot <= 0 ? "" : fileName.slice(dot + 1).toLowerCase();
}

function basename(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  return (dot <= 0 ? fileName : fileName.slice(0, dot)).toLowerCase();
}

function folderOf(relPath: string): string {
  const slash = relPath.lastIndexOf("/");
  return slash < 0 ? "" : relPath.slice(0, slash);
}

/** Pair identity: folder + lowercased basename. */
export function pairKey(item: GridItem): string {
  return `${folderOf(item.relPath)}/${basename(item.fileName)}`;
}

export function isCollapsed(
  key: string,
  globalCollapsed: boolean,
  overrides: ReadonlySet<string>,
): boolean {
  return overrides.has(key) ? !globalCollapsed : globalCollapsed;
}

/** Which member a collapsed pair displays by default (Settings →
 * "Stacked pairs show": JPEG (default) | RAW — dogfood round 1). */
export type StackDisplayMember = "jpeg" | "raw";

export interface StackOptions {
  globalCollapsed: boolean;
  /** Pair keys deviating from the global state (live per-pair toggle). */
  overrides: ReadonlySet<string>;
  /** Pair keys flipped to the NON-preferred member (R; collapsed only). */
  flips: ReadonlySet<string>;
  /** The preferred display member; "jpeg" when absent. */
  display?: StackDisplayMember;
}

export interface StackModel {
  units: DisplayUnit[];
  /** Pair key per UNIT; null = not a pair member (chevron + menu gating). */
  pairs: (string | null)[];
}

/**
 * Items (already sorted) → grid cells. A collapsed pair emits ONE unit at
 * the first-occurring member's position (the preferred member on top
 * unless flipped — R XORs the preference); an expanded pair emits both
 * members as individual units that still carry their pair key (so the
 * collapse control and menu row stay live).
 */
export function buildUnits(items: GridItem[], opts: StackOptions): StackModel {
  // First pass: the JPEG+RAW pairs (first of each class per key wins;
  // extras stay solo — duplicate basenames are not silently swallowed).
  const jpegAt = new Map<string, number>();
  const rawAt = new Map<string, number>();
  for (let i = 0; i < items.length; i++) {
    const e = ext(items[i].fileName);
    const side = JPEG_EXTS.has(e) ? jpegAt : RAW_EXTS.has(e) ? rawAt : null;
    if (side === null) continue;
    const key = pairKey(items[i]);
    if (!side.has(key)) side.set(key, i);
  }
  const memberKey = new Map<number, string>(); // item index → pair key
  for (const [key, j] of jpegAt) {
    const r = rawAt.get(key);
    if (r !== undefined) {
      memberKey.set(j, key);
      memberKey.set(r, key);
    }
  }

  // Second pass: emit units in item order.
  const units: DisplayUnit[] = [];
  const pairs: (string | null)[] = [];
  const emitted = new Set<string>(); // collapsed pair keys already emitted
  for (let i = 0; i < items.length; i++) {
    const key = memberKey.get(i);
    if (key === undefined) {
      units.push({ primary: items[i], alt: null });
      pairs.push(null);
      continue;
    }
    if (!isCollapsed(key, opts.globalCollapsed, opts.overrides)) {
      units.push({ primary: items[i], alt: null });
      pairs.push(key);
      continue;
    }
    if (emitted.has(key)) continue; // the second member of a collapsed pair
    emitted.add(key);
    const jpeg = items[jpegAt.get(key) as number];
    const raw = items[rawAt.get(key) as number];
    const rawOnTop = (opts.display === "raw") !== opts.flips.has(key);
    units.push(rawOnTop ? { primary: raw, alt: jpeg } : { primary: jpeg, alt: raw });
    pairs.push(key);
  }
  return { units, pairs };
}

/** Per-unit adjacency for EXPANDED pair members (dogfood round 1: expanded
 * members must read as visually linked in the grid). A link joins the two
 * members of one pair when they occupy adjacent cells on the same row of a
 * `cols`-wide grid; a row wrap (or a foreign cell between them, e.g. under
 * capture-time sort) breaks it — the chevrons still mark membership. */
export function expandedLinks(
  model: StackModel,
  cols: number,
): { left: boolean; right: boolean }[] {
  const { units, pairs } = model;
  const expanded = (i: number) => pairs[i] !== null && units[i].alt === null;
  return units.map((_, i) => {
    if (!expanded(i)) return { left: false, right: false };
    const sameRow = (a: number, b: number) =>
      cols > 0 && Math.floor(a / cols) === Math.floor(b / cols);
    const left =
      i > 0 && pairs[i - 1] === pairs[i] && expanded(i - 1) && sameRow(i - 1, i);
    const right =
      i + 1 < units.length &&
      pairs[i + 1] === pairs[i] &&
      expanded(i + 1) &&
      sameRow(i, i + 1);
    return { left, right };
  });
}

/**
 * Write-scope expansion (CAPTURE §3 upstream of logic/scope.ts): selected
 * hashes in SELECTION ORDER → ordered targets. A hash on a collapsed pair
 * (whether it names the displayed or the hidden member) expands to BOTH —
 * display member first, then the hidden one (DECISIONS 4). Solo/expanded
 * hashes pass through; duplicates collapse to first occurrence.
 */
export function expandTargets(order: readonly string[], units: DisplayUnit[]): string[] {
  const byMember = new Map<string, DisplayUnit>();
  for (const u of units) {
    byMember.set(u.primary.hash, u);
    if (u.alt !== null) byMember.set(u.alt.hash, u);
  }
  const out: string[] = [];
  const seen = new Set<string>();
  const push = (h: string) => {
    if (!seen.has(h)) {
      seen.add(h);
      out.push(h);
    }
  };
  for (const hash of order) {
    const unit = byMember.get(hash);
    if (unit === undefined) {
      push(hash); // not on the surface (stale selection) — pass through
    } else if (unit.alt === null) {
      push(hash);
    } else {
      push(unit.primary.hash);
      push(unit.alt.hash);
    }
  }
  return out;
}
