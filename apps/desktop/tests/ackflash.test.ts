/**
 * Fire-and-forget verbs acknowledge completion (primitives/
 * ackflash.svelte.ts + AckButton.svelte — founder dogfood, June 2026:
 * "Restart runtime ... I don't see anything"): the button disables while
 * the verb is in flight, says its past-tense label for a beat once the
 * promise RESOLVES, then reverts. Truthful like the copy check: a rejected
 * verb shows nothing. Toasts stay spec-capped at three triggers (UI §7.5)
 * — none here.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import { ACK_FLASH_MS, AckFlash } from "../src/lib/primitives/ackflash.svelte";
import AckButton from "../src/lib/primitives/AckButton.svelte";

/** A verb whose completion the test controls. */
function deferred() {
  let resolve!: () => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  document.body.innerHTML = "";
});

describe("AckFlash — the truthful done flash", () => {
  it("is busy while the verb is in flight, done after it resolves, then auto-clears", async () => {
    const ack = new AckFlash();
    const d = deferred();
    const running = ack.run(() => d.promise);
    expect(ack.busy).toBe(true);
    expect(ack.done).toBe(false);
    d.resolve();
    await running;
    expect(ack.busy).toBe(false);
    expect(ack.done).toBe(true);
    vi.advanceTimersByTime(ACK_FLASH_MS - 1);
    expect(ack.done).toBe(true); // the full window, not a moment less
    vi.advanceTimersByTime(1);
    expect(ack.done).toBe(false);
  });

  it("a rejected verb flashes nothing (no false 'done') and releases busy", async () => {
    const ack = new AckFlash();
    await expect(
      ack.run(() => Promise.reject(new Error("runtime spawn failed"))),
    ).rejects.toThrow("runtime spawn failed");
    expect(ack.busy).toBe(false);
    expect(ack.done).toBe(false);
  });

  it("a second run while in flight is a no-op (Enter racing the disable)", async () => {
    const ack = new AckFlash();
    const d = deferred();
    let fires = 0;
    const verb = () => {
      fires += 1;
      return d.promise;
    };
    const first = ack.run(verb);
    await ack.run(verb); // resolves immediately: the guard dropped it
    expect(fires).toBe(1);
    d.resolve();
    await first;
  });

  it("a re-run during the flash drops the stale clear and gives the new ack its full window", async () => {
    const ack = new AckFlash();
    await ack.run(() => Promise.resolve());
    vi.advanceTimersByTime(ACK_FLASH_MS - 100);
    await ack.run(() => Promise.resolve());
    // The first run's timer was restarted, not left to fire 100ms in.
    vi.advanceTimersByTime(ACK_FLASH_MS - 1);
    expect(ack.done).toBe(true);
    vi.advanceTimersByTime(1);
    expect(ack.done).toBe(false);
  });
});

describe("AckButton — the verb's one seat", () => {
  /** The visible label: AckButton overlays rest/done labels in one grid
   * cell (width stability), so "visible" = the seat not marked `.off`. */
  function visibleLabel(button: HTMLElement): string | null {
    const seat = [...button.querySelectorAll(".seat")].find(
      (s) => !s.classList.contains("off"),
    );
    return seat?.textContent ?? null;
  }

  it("disables in flight, says the past tense once landed, then reverts", async () => {
    const d = deferred();
    const { getByRole } = render(AckButton, {
      label: "Restart runtime",
      doneLabel: "Restarted",
      verb: () => d.promise,
    });
    const button = getByRole("button");
    expect(visibleLabel(button)).toBe("Restart runtime");

    await fireEvent.click(button);
    expect(button).toHaveProperty("disabled", true);

    d.resolve();
    await vi.advanceTimersByTimeAsync(0); // let the run() await settle
    expect(button).toHaveProperty("disabled", false);
    expect(visibleLabel(button)).toBe("Restarted");

    await vi.advanceTimersByTimeAsync(ACK_FLASH_MS);
    expect(visibleLabel(button)).toBe("Restart runtime");
  });

  it("both labels stay in the DOM (the grid overlay that keeps the width steady)", () => {
    const { getByRole } = render(AckButton, {
      label: "Re-detect hardware",
      doneLabel: "Re-detected",
      verb: () => Promise.resolve(),
    });
    const seats = getByRole("button").querySelectorAll(".seat");
    expect([...seats].map((s) => s.textContent)).toEqual([
      "Re-detect hardware",
      "Re-detected",
    ]);
  });
});
