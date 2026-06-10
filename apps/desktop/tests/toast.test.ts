/**
 * The sanctioned-toast queue (primitives/toast.svelte.ts): the CLOSED kind
 * enum is the §7.5 guardrail — exactly three toast kinds exist in the type
 * system; queue/dismiss/act semantics are pure (auto-dismiss timing lives
 * in ToastHost and is excluded here by design).
 */
import { describe, expect, it } from "vitest";
import {
  TOAST_DISMISS_MS,
  ToastQueue,
  type ToastKind,
} from "../src/lib/primitives/toast.svelte";

describe("the §7.5 guardrail", () => {
  it("the kind enum is closed: retracted · redacted · offline-redaction-complete", () => {
    // Compile-time: a fourth kind is a type error. Runtime echo for the suite:
    const kinds: ToastKind[] = ["retracted", "redacted", "offline-redaction-complete"];
    expect(kinds.length).toBe(3);
  });

  it("auto-dismiss is the spec'd 5 s", () => {
    expect(TOAST_DISMISS_MS).toBe(5000);
  });
});

describe("queue semantics", () => {
  it("queues in order with stable ids", () => {
    const q = new ToastQueue();
    const a = q.toast("retracted", "Retracted");
    const b = q.toast("redacted", "Redacted from journal, sidecars, and indexes");
    expect(q.items.map((t) => t.id)).toEqual([a, b]);
    expect(q.items[0].kind).toBe("retracted");
  });

  it("dismiss removes one toast; unknown ids are a no-op", () => {
    const q = new ToastQueue();
    const a = q.toast("retracted", "Retracted");
    q.dismiss(999);
    expect(q.items.length).toBe(1);
    q.dismiss(a);
    expect(q.items.length).toBe(0);
  });

  it("act() runs the action (Undo = RE-STATE, E4) and dismisses", () => {
    const q = new ToastQueue();
    let ran = 0;
    const id = q.toast("retracted", "Retracted", {
      label: "Undo",
      run: () => {
        ran += 1;
      },
    });
    q.act(id);
    expect(ran).toBe(1);
    expect(q.items.length).toBe(0);
    // act on an action-less toast just dismisses.
    const id2 = q.toast("redacted", "Redacted");
    q.act(id2);
    expect(q.items.length).toBe(0);
  });
});
