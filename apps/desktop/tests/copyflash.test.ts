/**
 * The ONE copy-confirmation register (primitives/copyflash.svelte.ts —
 * BACKLOG "Copy actions confirm themselves"): the write flashes the
 * caller's key only when it LANDS; the flash auto-clears; copy menu rows
 * (Menu.svelte) defer their close so the check has a seat to show on.
 * Toasts stay spec-capped at three triggers (UI §7.5) — none here.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import {
  COPY_FLASH_MS,
  copyFlash,
  copyToClipboard,
} from "../src/lib/primitives/copyflash.svelte";
import Menu from "../src/lib/primitives/Menu.svelte";
import type { MenuModel } from "../src/lib/actions/menus";

function stubClipboard(writeText: (t: string) => Promise<void>) {
  Object.defineProperty(window.navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  delete (window.navigator as { clipboard?: unknown }).clipboard;
  copyFlash.key = null;
  document.body.innerHTML = "";
});

describe("copyToClipboard — the truthful flash", () => {
  it("flashes the caller's key after a successful write, then auto-clears", async () => {
    const writes: string[] = [];
    stubClipboard(async (t) => {
      writes.push(t);
    });
    await copyToClipboard("copy-file-path", "/mnt/vol/IMG_4471.jpg");
    expect(writes).toEqual(["/mnt/vol/IMG_4471.jpg"]);
    expect(copyFlash.key).toBe("copy-file-path");
    vi.advanceTimersByTime(COPY_FLASH_MS);
    expect(copyFlash.key).toBeNull();
  });

  it("a failed write flashes nothing (no false 'copied')", async () => {
    // navigator.clipboard rejects AND jsdom has no execCommand: both
    // paths fail, so the register must stay silent.
    stubClipboard(async () => {
      throw new Error("denied");
    });
    await copyToClipboard("copy-file-path", "x");
    expect(copyFlash.key).toBeNull();
  });

  it("a second copy moves the one check (never two at once)", async () => {
    stubClipboard(async () => {});
    await copyToClipboard("metadata:Hash", "aa");
    vi.advanceTimersByTime(400);
    await copyToClipboard("metadata:Path", "/p");
    expect(copyFlash.key).toBe("metadata:Path");
    // The shared timer restarted: the second flash gets its full window.
    vi.advanceTimersByTime(COPY_FLASH_MS - 1);
    expect(copyFlash.key).toBe("metadata:Path");
    vi.advanceTimersByTime(1);
    expect(copyFlash.key).toBeNull();
  });
});

describe("Menu.svelte — copy rows confirm inline", () => {
  const model: MenuModel = {
    rows: [
      { kind: "item", verb: "Open", action: { kind: "open-look" } },
      {
        kind: "item",
        verb: "Copy file path",
        action: { kind: "copy-file-path" },
        flashKey: "copy-file-path",
      },
    ],
  };

  it("a copy row defers the close and shows the check once the write lands", async () => {
    const onclose = vi.fn();
    // The perform sink's role, minimally: the action lands the copy.
    stubClipboard(async () => {});
    const onaction = vi.fn(() => void copyToClipboard("copy-file-path", "/p"));
    const { getByRole } = render(Menu, { model, onaction, onclose });

    await fireEvent.click(getByRole("menuitem", { name: /Copy file path/ }));
    expect(onaction).toHaveBeenCalledWith({ kind: "copy-file-path" });
    // The menu held open for the confirmation seat...
    expect(onclose).not.toHaveBeenCalled();
    // ...the landed write flashes the row's check...
    await vi.advanceTimersByTimeAsync(0);
    expect(copyFlash.key).toBe("copy-file-path");
    // ...and the close follows while the flash is still showing.
    await vi.advanceTimersByTimeAsync(COPY_FLASH_MS);
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it("ordinary rows still close on activate", async () => {
    const onclose = vi.fn();
    const onaction = vi.fn();
    const { getByRole } = render(Menu, { model, onaction, onclose });
    await fireEvent.click(getByRole("menuitem", { name: /Open/ }));
    expect(onclose).toHaveBeenCalledTimes(1);
  });
});
