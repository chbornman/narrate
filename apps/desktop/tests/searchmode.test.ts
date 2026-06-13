/**
 * Search lane state machine (logic/searchmode.ts — M3 search-as-scope, Phase
 * 2): the PURE transition that makes the live-lexical / commit-semantic split
 * explicit, plus the detail-row status copy. Tested in isolation (no runes, no
 * IPC) so every edge of the machine is pinned independently of the store.
 */
import { describe, expect, it } from "vitest";
import { laneStatus, nextLane, type SearchLane } from "../src/lib/logic/searchmode";

const LANES: SearchLane[] = ["none", "lexical", "semantic"];

describe("nextLane (the live-lexical / commit-semantic machine)", () => {
  it("every keystroke runs lexical — from ANY prior lane (the <100 ms floor)", () => {
    // (a) as-you-type is never semantic, including right after a commit:
    // editing drops you back to the live keyword lane until the next Enter.
    for (const lane of LANES) expect(nextLane(lane, "type")).toBe("lexical");
  });

  it("Enter is the ONLY path to semantic — from any prior lane", () => {
    // (b) commit selects the hybrid lane regardless of where you were.
    for (const lane of LANES) expect(nextLane(lane, "commit")).toBe("semantic");
  });

  it("clearing the query returns to 'none' (the bar is no longer a scope)", () => {
    // (d) dropping the query scope drops the lane — no stale lane is named.
    for (const lane of LANES) expect(nextLane(lane, "clear")).toBe("none");
  });

  it("commit-then-edit round-trips semantic -> lexical (re-typing drops back)", () => {
    // (c) the design's "editing after a commit returns to lexical".
    const committed = nextLane("lexical", "commit");
    expect(committed).toBe("semantic");
    expect(nextLane(committed, "type")).toBe("lexical");
  });
});

describe("laneStatus (the detail-row indicator copy)", () => {
  it("while typing: lexical with the Enter-upgrade hint", () => {
    expect(laneStatus("lexical")).toBe("lexical · Enter for semantic");
  });

  it("after Enter: the single quiet 'semantic'", () => {
    expect(laneStatus("semantic")).toBe("semantic");
  });

  it("no scope: empty copy (nothing to name)", () => {
    expect(laneStatus("none")).toBe("");
  });

  it("uses no em-dashes in any state (the check:emdash gate)", () => {
    for (const lane of LANES) expect(laneStatus(lane)).not.toContain("—");
  });
});
