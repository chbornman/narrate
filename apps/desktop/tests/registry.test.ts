/**
 * Registry invariants (UI-ARCHITECTURE §10.2) — parity between the four
 * renderings (keys, menus, cheatsheet, tooltips) is structural; these
 * tests guard the residue:
 *   · (id, scope) unique
 *   · chord-collision property sweep over GENERATED contexts (not
 *     hand-picked fixtures): no two defs available+enabled for the same
 *     chord at the same scope-priority level
 *   · every def has ≥1 key, or a seat, or is on the pointer-only allowlist
 *   · cheatsheet rows ≡ registry minus reserved
 *   · every seated verb appears in a seat-order table (menus can render it)
 */
import { describe, expect, it } from "vitest";
import { REGISTRY } from "../src/lib/actions/registry";
import { candidates } from "../src/lib/actions/match";
import { cheatsheet } from "../src/lib/actions/cheatsheet";
import { menuModel } from "../src/lib/actions/menus";
import type { ActionContext, MenuSeat } from "../src/lib/actions/types";
import { withDefaults } from "../src/lib/logic/keymap";
import type { KeyInput } from "../src/lib/logic/keymap";

// ---- generated contexts (seeded PRNG: deterministic sweep) ------------------

function mulberry32(seed: number) {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function genContext(rnd: () => number): ActionContext {
  const bool = () => rnd() < 0.5;
  const pick = <T>(xs: readonly T[]): T => xs[Math.floor(rnd() * xs.length)];
  return withDefaults({
    surface: pick(["grid", "look"] as const),
    searchOpen: bool(),
    inputFocused: bool(),
    searchInputFocused: bool(),
    hasSelection: bool(),
    selectionCount: Math.floor(rnd() * 4),
    activeHash: bool() ? "ab".repeat(32) : null,
    activeIsPair: bool(),
    activePairCollapsed: bool(),
    railOpen: bool(),
    railFocused: bool(),
    inspectorOpen: pick([false, "metadata", "journal"] as const),
    cheatsheetOpen: bool(),
    contextMenuOpen: false, // menu-open suppression is its own rule
    chromeHidden: bool(),
    autoAdvance: bool(),
    lookAtFit: bool(),
    debugEnabled: bool(),
    asrReady: bool(),
    filmstrip: bool(),
    // pencil context (live since P5.1): sweep both halves of every gate
    pencilMode: bool(),
    overlayVisible: bool(),
    pencilUndoable: bool(),
  });
}

/** Every distinct chord shape in the registry, as concrete key inputs. */
function allInputs(): KeyInput[] {
  const seen = new Map<string, KeyInput>();
  for (const def of REGISTRY) {
    for (const c of def.keys) {
      for (const shift of c.shift === undefined ? [false, true] : [c.shift]) {
        const e: KeyInput = {
          key: c.key,
          ctrlOrMeta: c.ctrlOrMeta ?? false,
          shift,
        };
        seen.set(`${e.key}|${e.ctrlOrMeta}|${e.shift}`, e);
      }
    }
  }
  return [...seen.values()];
}

describe("registry shape", () => {
  it("(id, scope) pairs are unique", () => {
    const keys = REGISTRY.map((d) => `${d.id}@${d.scope}`);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("every def has ≥1 key, a seat, or sits on the pointer-only allowlist", () => {
    // Pointer-only rows with no seat would be unreachable; none exist.
    const POINTER_ONLY_ALLOWLIST: string[] = [];
    for (const def of REGISTRY) {
      const reachable =
        def.keys.length > 0 ||
        (def.seats !== undefined && def.seats.length > 0) ||
        POINTER_ONLY_ALLOWLIST.includes(def.id);
      expect(reachable, `unreachable def: ${def.id}`).toBe(true);
    }
  });

  it("reserved rows carry keys (their packets light them up) but no seats", () => {
    for (const def of REGISTRY.filter((d) => d.reserved === true)) {
      expect(def.seats ?? [], `reserved def ${def.id} must stay unseated`).toEqual([]);
    }
  });
});

describe("chord-collision property sweep", () => {
  it("no two defs are available+enabled for the same chord at the same priority (400 generated contexts)", () => {
    const rnd = mulberry32(42);
    const inputs = allInputs();
    for (let n = 0; n < 400; n++) {
      const ctx = genContext(rnd);
      for (const e of inputs) {
        const c = candidates(e, ctx, REGISTRY);
        if (c.length < 2) continue;
        const top = c.filter((x) => x.priority === c[0].priority);
        expect(
          top.length,
          `collision on ${JSON.stringify(e)} between ${top
            .map((x) => `${x.def.id}@${x.def.scope}`)
            .join(" vs ")} in ctx ${JSON.stringify(ctx)}`,
        ).toBeLessThanOrEqual(1);
      }
    }
  });
});

describe("cheatsheet completeness", () => {
  it("cheatsheet rows ≡ registry minus reserved (dimmed, never hidden)", () => {
    const ctx = withDefaults({
      surface: "grid",
      searchOpen: false,
      inputFocused: false,
      searchInputFocused: false,
      hasSelection: false,
      railOpen: false,
      debugEnabled: false,
      asrReady: false,
    });
    const rows = cheatsheet(ctx).flatMap((g) => g.rows.map((r) => `${r.id}@${r.scope}`));
    const expected = REGISTRY.filter((d) => d.reserved !== true).map(
      (d) => `${d.id}@${d.scope}`,
    );
    expect(new Set(rows)).toEqual(new Set(expected));
    expect(rows.length).toBe(expected.length);
  });
});

describe("pointer-reachability audit (dogfood round 1: visible UI for actions)", () => {
  /**
   * Every non-reserved verb must be reachable by POINTER somewhere — a
   * context-menu seat, a chrome button, or a pointer-native gesture. This
   * table names the affordance for every verb that is deliberately NOT
   * menu-seated; a new def missing from both fails the audit.
   */
  const CHROME_OR_POINTER_NATIVE: Record<string, string> = {
    escape: "every Esc layer has a pointer dismissal (outside-click, scrim, ×)",
    "toggle-rail": "titlebar rail button (Titlebar.svelte via resolveAction)",
    "open-search": "titlebar search button",
    "summon-note": "indicator capsule click (the note zone)",
    quit: "titlebar × window control",
    "toggle-debug-panel": "dev builds only (F12) — not a product surface",
    "focus-move": "pointer-native: clicking a thumb moves focus",
    "focus-edge": "pointer-native: scrollbar + clicks",
    "focus-page": "pointer-native: scrollbar + clicks",
    "toggle-select-focused": "pointer-native: Ctrl+click on a thumb",
    "open-sort-menu": "grid header sort ▾ button",
    "thumb-size": "grid header slider + Ctrl+wheel",
    "zoom-step": "pointer-native: wheel zoom-to-cursor",
    "look-close": "backdrop menu carries Go to Grid (the pointer path out of Look)",
    "look-nav": "filmstrip thumb clicks",
    "rail-nav": "pointer-native: rail rows are clickable",
    "rail-enter": "pointer-native: a rail row click opens the folder",
    "search-nav": "pointer-native: result rows are clickable",
    "search-open-result": "search result row click",
    "remove-last-chip": "chip × buttons in the search overlay",
    "pencil-eraser":
      "pointer-native: the stylus eraser end (hold-E is the keyboard form; a menu item cannot hold)",
    "toggle-mic": "indicator mic segment click (Indicator.svelte via resolveAction)",
    // pencil-undo is SEATED (look-backdrop "Undo stroke") — its keyboard-
    // only exemption was removed by the post-P5.1 polish round.
  };

  it("every non-reserved verb holds a seat or names its chrome/pointer affordance", () => {
    for (const def of REGISTRY) {
      if (def.reserved === true) continue;
      const seated = def.seats !== undefined && def.seats.length > 0;
      expect(
        seated || def.id in CHROME_OR_POINTER_NATIVE,
        `${def.id}@${def.scope} has no pointer path — seat it or name its affordance`,
      ).toBe(true);
    }
  });
});

describe("seat coverage (menus can render every seated verb)", () => {
  it("every seated, non-reserved def appears in its seat's order table", () => {
    const ctx = withDefaults({
      surface: "grid",
      searchOpen: false,
      inputFocused: false,
      searchInputFocused: false,
      hasSelection: true,
      selectionCount: 2,
      activeHash: "ab".repeat(32),
      railOpen: true,
      debugEnabled: false,
      asrReady: false,
      // Add-to-collection exists only when a collection exists, and
      // Remove-from-collection only when the active image is a member
      // (their availability gates); coverage needs both on the table.
      collections: [{ id: "01C", name: "Quiet Hours" }],
      activeMemberships: ["01C"],
    });
    const seats: MenuSeat[] = ["thumb", "gutter", "rail-folder", "look-backdrop"];
    for (const seat of seats) {
      const seated = REGISTRY.filter(
        (d) => d.reserved !== true && d.seats?.includes(seat),
      );
      const model = menuModel(seat, ctx, { rootId: "r", folder: "" });
      const verbs = new Set<string>();
      const walk = (rows: typeof model.rows) => {
        for (const r of rows) {
          if (r.verb !== undefined) verbs.add(r.verb);
          if (r.children !== undefined) walk(r.children);
        }
      };
      walk(model.rows);
      for (const def of seated) {
        expect(verbs.has(def.verb), `${def.id} missing from ${seat} menu`).toBe(true);
      }
    }
  });

  it("the journal-row seat renders its verb (its arg is the entry's target list)", () => {
    const ctx = withDefaults({
      surface: "grid",
      searchOpen: false,
      inputFocused: false,
      searchInputFocused: false,
      hasSelection: false,
      railOpen: false,
      debugEnabled: false,
      asrReady: false,
      inspectorOpen: "journal",
    });
    const model = menuModel("journal-row", ctx, ["ab".repeat(32)]);
    const seated = REGISTRY.filter(
      (d) => d.reserved !== true && d.seats?.includes("journal-row"),
    );
    expect(seated.length).toBeGreaterThan(0);
    for (const def of seated) {
      expect(
        model.rows.some((r) => r.verb === def.verb),
        `${def.id} missing from journal-row menu`,
      ).toBe(true);
    }
  });
});
