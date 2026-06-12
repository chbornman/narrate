/**
 * Shell slice (state/shell.svelte.ts — the one slice FOUNDATIONS ships
 * complete): rail open/focus handoff, the context-menu host state,
 * surround persistence, the note transient + scope echo, and pulse
 * coalescing. Lights-out became a cross-slice SNAPSHOT-RESTORE at the
 * composition root (founder, June 12 2026) — its semantics are tested in
 * ui-store.test.ts; the slice only carries chromeHidden + panelSnapshot.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { ShellSlice } from "../src/lib/state/shell.svelte";
import type { ScopeView } from "../src/lib/types/dto";

const scope = (kind: ScopeView["kind"], count: number, hashes: string[] = []): ScopeView => ({
  kind,
  count,
  previewHashes: hashes,
});

let shell: ShellSlice;
beforeEach(() => {
  localStorage.clear();
  shell = new ShellSlice();
});

describe("rail (push panel, D5)", () => {
  it("\\ toggle hands the rail keyboard focus on open, returns it on close", () => {
    shell.toggleRail();
    expect(shell.railOpen).toBe(true);
    expect(shell.railFocused).toBe(true);
    shell.toggleRail();
    expect(shell.railOpen).toBe(false);
    expect(shell.railFocused).toBe(false);
  });

  it("openness persists (push-persistent rail)", () => {
    shell.toggleRail();
    const fresh = new ShellSlice();
    fresh.loadPrefs();
    expect(fresh.railOpen).toBe(true);
  });
});

describe("first-run welcome card (BACKLOG: how your data is stored)", () => {
  it("opens on a fresh profile; dismissal with the default-ON toggle persists", () => {
    shell.loadPrefs();
    expect(shell.welcomeOpen).toBe(true);
    expect(shell.welcomeDontShowAgain).toBe(true); // common path: seen once
    shell.dismissWelcome();
    expect(shell.welcomeOpen).toBe(false);
    const fresh = new ShellSlice();
    fresh.loadPrefs();
    expect(fresh.welcomeOpen).toBe(false); // next launch: stays away
  });

  it("toggle OFF keeps the card returning every launch (user's call)", () => {
    shell.loadPrefs();
    shell.welcomeDontShowAgain = false;
    shell.dismissWelcome();
    expect(shell.welcomeOpen).toBe(false); // dismissed for THIS session
    const fresh = new ShellSlice();
    fresh.loadPrefs();
    expect(fresh.welcomeOpen).toBe(true);
  });
});

describe("context-menu host state", () => {
  it("one menu at a time; seat + anchor + arg travel together", () => {
    shell.openContextMenu("thumb", { x: 10, y: 20 });
    expect(shell.contextMenu?.seat).toBe("thumb");
    shell.openContextMenu("rail-folder", null, { rootId: "r", folder: "f" });
    expect(shell.contextMenu?.seat).toBe("rail-folder");
    expect(shell.contextMenu?.anchor).toBeNull(); // keyboard-summoned
    shell.closeContextMenu();
    expect(shell.contextMenu).toBeNull();
  });
});

describe("surround (D6)", () => {
  it("sets and persists the level", () => {
    shell.setSurround("middle");
    expect(shell.surround).toBe("middle");
    const fresh = new ShellSlice();
    fresh.loadPrefs();
    expect(fresh.surround).toBe("middle");
  });
});

describe("note transient + scope echo", () => {
  it("summon snapshots the current echo; a scope CHANGE cancels the note", () => {
    shell.onScopeEcho(scope("multi", 2, ["a", "b"]));
    shell.summonNote();
    expect(shell.note.open).toBe(true);
    shell.onScopeEcho(scope("multi", 2, ["a", "b"])); // identical: stays
    expect(shell.note.open).toBe(true);
    shell.onScopeEcho(scope("single", 1, ["a"])); // changed: cancels
    expect(shell.note.open).toBe(false);
  });
});

describe("the station (pin + pops)", () => {
  it("the info seat's pin holds the expansion; closeStation clears hover AND pin", () => {
    shell.toggleStationPinned();
    shell.popoverOpen = true;
    expect(shell.stationPinned).toBe(true);
    shell.closeStation(); // Esc / outside-click: one dismissal, both flags
    expect(shell.stationPinned).toBe(false);
    expect(shell.popoverOpen).toBe(false);
  });

  it("mic transitions pop from the station; a scope-only echo pops nothing", () => {
    const state = (mic: "disarmed" | "armedIdle") => ({
      currentScope: scope("session", 0),
      mic,
      streamingUtterance: null,
      degraded: { asrUnavailable: false },
    });
    shell.onIndicatorState(state("armedIdle"));
    expect(shell.pops.map((p) => p.text)).toEqual(["Mic armed"]);
    shell.onIndicatorState(state("armedIdle")); // no transition: quiet
    expect(shell.pops.length).toBe(1);
    shell.onIndicatorState(state("disarmed"));
    expect(shell.pops.map((p) => p.text)).toEqual(["Mic armed", "Mic off"]);
    shell.dismissPop(shell.pops[0].id);
    expect(shell.pops.map((p) => p.text)).toEqual(["Mic off"]);
  });
});

describe("pulse coalescing (UI §7.4)", () => {
  it("rapid events coalesce above ~5/s", () => {
    shell.onPulse(1000);
    shell.onPulse(1050); // < 200 ms: coalesced
    shell.onPulse(1300);
    expect(shell.pulseCount).toBe(2);
  });
});
