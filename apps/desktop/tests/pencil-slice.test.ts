/**
 * Pencil state + flows (P5.1): the look slice's pencil fields (mode
 * stickiness across ←/→, reset on Grid return, overlay-off exits pencil —
 * UI §4.4) and the composition root's stroke flows against mocked IPC
 * (ui-store.test.ts pattern): commit → add_stroke bound to the VIEWED
 * image + undo stack push; Ctrl+Z → retraction of the most recent stacked
 * stroke through the EXISTING retract_event path (CAPTURE §13.4's
 * controller slice: two undos = two tombstone commands, never a delete);
 * eraser retraction drops the stack entry wherever it sits; mid-draw
 * Ctrl+Z cancels locally and calls NOTHING.
 *
 * The stack is SESSION-scoped (§8.5/C4) and session closure is lazy: the
 * backend echoes its post-touch session id from add_stroke and
 * report_activity, and a changed id empties the stack — Ctrl+Z must never
 * retract a prior-session stroke. The journal panel's Retract row rides
 * ui.perform so a committed retraction also leaves the stack.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { StrokePayloadWire } from "../src/lib/types/dto";

const ipcLog = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> | undefined }[],
  nextStrokeId: 0,
  /** The backend's CURRENT session (post-touch echo): flipping this
   * simulates a rotation across the 30-minute idle boundary — closure is
   * lazy, so the next add_stroke/report_activity lands in the new id. */
  sessionId: "S1",
  retractResult: true,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    ipcLog.calls.push({ cmd, args });
    switch (cmd) {
      case "add_stroke":
        return { id: `01STROKE${ipcLog.nextStrokeId++}`, sessionId: ipcLog.sessionId };
      case "report_activity":
        return ipcLog.sessionId;
      case "retract_event":
        return ipcLog.retractResult;
      case "set_scope":
        return { kind: "single", count: 1, previewHashes: [] };
      case "image_journal":
      case "list_folder":
        return [];
      default:
        return null;
    }
  }),
  convertFileSrc: (p: string, proto = "asset") => `${proto}://localhost/${p}`,
}));

import { Ui } from "../src/lib/state/app.svelte";
import { LookSlice } from "../src/lib/state/look.svelte";
import { UNDO_DEPTH, pushUndo } from "../src/lib/logic/pencil";

const HASH_A = "aa".repeat(32);
const HASH_B = "bb".repeat(32);

const payload: StrokePayloadWire = {
  baseW: 40,
  orientation: 1,
  points: [
    [4312, 2210, 1000, 0],
    [4391, 2188, 770, 17],
  ],
  tool: "pencil",
};

const calls = (cmd: string) => ipcLog.calls.filter((c) => c.cmd === cmd);

let ui: Ui;
beforeEach(() => {
  ipcLog.calls.length = 0;
  ipcLog.nextStrokeId = 0;
  ipcLog.sessionId = "S1";
  ipcLog.retractResult = true;
  localStorage.clear();
  ui = new Ui();
  ui.look.open(
    [
      { display: HASH_A, alt: null },
      { display: HASH_B, alt: null },
    ],
    0,
  );
  ui.surface = "look";
});

describe("pencil mode lifecycle (UI §4.4)", () => {
  it("B toggles; the mode persists across prev/next within Look", () => {
    ui.look.togglePencil();
    expect(ui.look.pencilMode).toBe(true);
    ui.look.next(1);
    expect(ui.look.pencilMode).toBe(true);
  });

  it("resets to OFF on return to Grid; overlay visibility and stack survive", () => {
    ui.look.togglePencil();
    ui.look.onStrokeCommitted("01S", HASH_A, "S1");
    ui.look.overlayVisible = true;
    ui.look.close();
    expect(ui.look.pencilMode).toBe(false);
    expect(ui.look.eraserHeld).toBe(false);
    expect(ui.look.overlayVisible).toBe(true); // per app-run, not per visit
    expect(ui.look.undoStack.length).toBe(1); // per process (§8.5)
  });

  it("O off exits pencil mode; B while hidden shows the paper AND arms the pencil", () => {
    ui.look.togglePencil();
    ui.look.toggleOverlay();
    expect(ui.look.overlayVisible).toBe(false);
    expect(ui.look.pencilMode).toBe(false);
    ui.look.togglePencil(); // one keystroke: show + arm (a bound key is never dead)
    expect(ui.look.overlayVisible).toBe(true);
    expect(ui.look.pencilMode).toBe(true);
    // With the paper visible again, B is back to the plain sticky toggle.
    ui.look.togglePencil();
    expect(ui.look.overlayVisible).toBe(true);
    expect(ui.look.pencilMode).toBe(false);
  });

  it("the action context reports the live pencil fields", () => {
    ui.look.togglePencil();
    let ctx = ui.actionContext();
    expect(ctx.pencilMode).toBe(true);
    expect(ctx.overlayVisible).toBe(true);
    expect(ctx.pencilUndoable).toBe(false);
    ui.look.onStrokeCommitted("01S", HASH_A, "S1");
    ctx = ui.actionContext();
    expect(ctx.pencilUndoable).toBe(true);
  });
});

describe("stroke commit (CAPTURE §8.4 → add_stroke)", () => {
  it("binds to the VIEWED image and pushes the undo stack", async () => {
    await ui.commitStroke(payload);
    const call = calls("add_stroke")[0];
    expect(call.args?.hash).toBe(HASH_A);
    expect(call.args?.payload).toEqual(payload);
    expect(ui.look.undoStack).toEqual([{ id: "01STROKE0", hash: HASH_A }]);
  });

  it("targets the displayed image after ←/→ (always single-target)", async () => {
    ui.look.next(1);
    await ui.commitStroke(payload);
    expect(calls("add_stroke")[0].args?.hash).toBe(HASH_B);
  });

  it("the stack holds at most 10 strokes (§8.5 depth)", async () => {
    for (let i = 0; i < 12; i++) await ui.commitStroke(payload);
    expect(ui.look.undoStack.length).toBe(UNDO_DEPTH);
    expect(ui.look.undoStack[0].id).toBe("01STROKE2"); // oldest two fell off
  });
});

describe("undo = retraction (CAPTURE §8.5 / §13.4 controller slice)", () => {
  it("two undos mint two retractions, newest first, through retract_event", async () => {
    for (let i = 0; i < 3; i++) await ui.commitStroke(payload);
    await ui.pencilUndo();
    await ui.pencilUndo();
    const retracts = calls("retract_event").map((c) => c.args?.eventId);
    expect(retracts).toEqual(["01STROKE2", "01STROKE1"]);
    expect(ui.look.undoStack.map((e) => e.id)).toEqual(["01STROKE0"]);
  });

  it("an empty stack no-ops (nothing minted, nothing thrown)", async () => {
    await ui.pencilUndo();
    expect(calls("retract_event")).toEqual([]);
  });

  it("during pen-down, Ctrl+Z cancels LOCALLY: no IPC, just the cancel signal", async () => {
    ui.look.penDown = true;
    const seq = ui.look.penCancelSeq;
    await ui.pencilUndo();
    expect(ui.look.penCancelSeq).toBe(seq + 1);
    expect(calls("retract_event")).toEqual([]);
  });

  it("undo reaches strokes drawn on a PREVIOUS image (the stack is session-scoped)", async () => {
    await ui.commitStroke(payload); // on A
    ui.look.next(1); // now viewing B
    await ui.pencilUndo();
    expect(calls("retract_event")[0].args?.eventId).toBe("01STROKE0");
  });
});

describe("session boundary (§8.5: the stack is cleared at session close)", () => {
  it("Ctrl+Z across the 30-minute rotation clears the stack and mints NOTHING", async () => {
    await ui.commitStroke(payload);
    await ui.commitStroke(payload);
    // Idle 31+ minutes: closure is lazy (CAPTURE §2.2), so the rotation
    // lands at the next activity — this very Ctrl+Z press. Its
    // report_activity echo arbitrates before any tombstone is minted.
    ipcLog.sessionId = "S2";
    await ui.pencilUndo();
    expect(calls("retract_event")).toEqual([]); // prior-session strokes are
    expect(ui.look.undoStack).toEqual([]); // journal-panel/eraser work
    expect(calls("report_activity").length).toBeGreaterThan(0);
    // A follow-up press is the plain empty-stack no-op.
    await ui.pencilUndo();
    expect(calls("retract_event")).toEqual([]);
  });

  it("same-session Ctrl+Z still retracts (the activity echo agrees)", async () => {
    await ui.commitStroke(payload);
    await ui.pencilUndo();
    expect(calls("retract_event").map((c) => c.args?.eventId)).toEqual(["01STROKE0"]);
  });

  it("a stroke landing in a NEW session starts a fresh stack (C4: this-session only)", async () => {
    await ui.commitStroke(payload); // S1
    ipcLog.sessionId = "S2";
    await ui.commitStroke(payload); // S2: the S1 entry must fall away
    expect(ui.look.undoStack.map((e) => e.id)).toEqual(["01STROKE1"]);
    expect(ui.look.undoSessionId).toBe("S2");
  });

  it("a rotated session id from ANY activity echo empties the stack (slice unit)", () => {
    const look = new LookSlice();
    look.onStrokeCommitted("01S", HASH_A, "S1");
    look.syncUndoSession("S1"); // same session: the stack stands
    expect(look.undoStack.length).toBe(1);
    look.syncUndoSession("S2"); // rotation observed
    expect(look.undoStack).toEqual([]);
    expect(look.undoSessionId).toBeNull();
  });
});

describe("journal-panel Retract (the row verb rides ui.perform — §8.5 cleanup)", () => {
  it("a committed retraction leaves the undo stack; Ctrl+Z then hits the next LIVE stroke", async () => {
    await ui.commitStroke(payload);
    await ui.commitStroke(payload);
    await ui.perform({ kind: "journal-retract", eventId: "01STROKE1" });
    expect(ui.look.undoStack.map((e) => e.id)).toEqual(["01STROKE0"]);
    // The press must target the remaining live stroke — never mint a
    // duplicate tombstone for the already-retracted one.
    await ui.pencilUndo();
    expect(calls("retract_event").map((c) => c.args?.eventId)).toEqual([
      "01STROKE1",
      "01STROKE0",
    ]);
  });

  it("an UNcommitted retraction leaves the stack alone", async () => {
    await ui.commitStroke(payload);
    ipcLog.retractResult = false;
    await ui.perform({ kind: "journal-retract", eventId: "01STROKE0" });
    expect(ui.look.undoStack.map((e) => e.id)).toEqual(["01STROKE0"]);
  });
});

describe("eraser retraction (CAPTURE §8.6: whole stroke, existing path)", () => {
  it("retracts the hit stroke and drops it from the stack wherever it sits", async () => {
    for (let i = 0; i < 3; i++) await ui.commitStroke(payload);
    await ui.eraseStroke("01STROKE1"); // the MIDDLE entry
    expect(calls("retract_event")[0].args?.eventId).toBe("01STROKE1");
    expect(ui.look.undoStack.map((e) => e.id)).toEqual(["01STROKE0", "01STROKE2"]);
  });

  it("bumps the overlay version so the fold refetches", async () => {
    await ui.commitStroke(payload);
    const v = ui.look.strokesVersion;
    await ui.eraseStroke("01STROKE0");
    expect(ui.look.strokesVersion).toBeGreaterThan(v);
  });
});

describe("journal-row flash (UI §8.2)", () => {
  it("the journal-flash-stroke action flashes on the Look overlay only in Look", async () => {
    await ui.perform({ kind: "journal-flash-stroke", eventId: "01S" });
    expect(ui.look.strokeFlash).toEqual({ id: "01S", seq: 1 });
    ui.surface = "grid";
    await ui.perform({ kind: "journal-flash-stroke", eventId: "01T" });
    expect(ui.look.strokeFlash).toEqual({ id: "01S", seq: 1 }); // unchanged
  });
});

describe("perform routing for the band", () => {
  it("pencil-pen toggles, cycle-overlay exits pencil, pencil-eraser engages the hold", async () => {
    await ui.perform({ kind: "pencil-pen" });
    expect(ui.look.pencilMode).toBe(true);
    await ui.perform({ kind: "pencil-eraser" });
    expect(ui.look.eraserHeld).toBe(true);
    await ui.perform({ kind: "cycle-overlay" });
    expect(ui.look.overlayVisible).toBe(false);
    expect(ui.look.pencilMode).toBe(false);
    expect(ui.look.eraserHeld).toBe(false);
    // B with the paper hidden: one keystroke shows the overlay and
    // re-arms the pencil (BACKLOG, founder: a bound key is never dead).
    await ui.perform({ kind: "pencil-pen" });
    expect(ui.look.overlayVisible).toBe(true);
    expect(ui.look.pencilMode).toBe(true);
  });
});

describe("undo stack helpers (pure)", () => {
  it("pushUndo trims to depth from the FRONT", () => {
    let stack: { id: string; hash: string }[] = [];
    for (let i = 0; i < 12; i++) stack = pushUndo(stack, { id: `s${i}`, hash: HASH_A });
    expect(stack.length).toBe(UNDO_DEPTH);
    expect(stack[0].id).toBe("s2");
    expect(stack[9].id).toBe("s11");
  });
});

describe("slice unit behaviors (no IPC)", () => {
  it("flashStroke increments the seq for repeated clicks on one row", () => {
    const look = new LookSlice();
    look.flashStroke("01S");
    look.flashStroke("01S");
    expect(look.strokeFlash).toEqual({ id: "01S", seq: 2 });
  });
});
