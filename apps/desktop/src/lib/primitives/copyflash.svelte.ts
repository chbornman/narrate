/**
 * The ONE copy-confirmation register (BACKLOG "Copy actions confirm
 * themselves"): every copy affordance writes through copyToClipboard(key,
 * text) and renders a brief check while `copyFlash.key` matches its key.
 * WHY not a toast: toasts are spec-capped at exactly three triggers
 * (UI §7.5 / R5), so the confirmation lives AT the affordance — and one
 * shared register keeps every copy verb confirming the same quiet way
 * (founder: "pick ONE register and use it everywhere").
 *
 * The flash is truthful: the key is set only after the clipboard write
 * succeeded — a failed copy shows nothing rather than lying.
 */

export const COPY_FLASH_MS = 1200;

class CopyFlash {
  /** The affordance currently showing its check; null = none. */
  key = $state<string | null>(null);
  #timer: ReturnType<typeof setTimeout> | undefined;

  begin(key: string) {
    this.key = key;
    // One shared timer: a second copy inside the window simply moves the
    // check (two checks at once would claim two writes landed last).
    clearTimeout(this.#timer);
    this.#timer = setTimeout(() => (this.key = null), COPY_FLASH_MS);
  }
}

export const copyFlash = new CopyFlash();

/** Compose a flash key scoped to the copied SUBJECT (the image hash, the
 * row's value). WHY: the affordance alone is not enough — the Metadata
 * tab's "Hash" glyph and the thumb menu's "Copy file path" row survive a
 * selection change, and a residual check on image B would vouch for a
 * copy of B's value that never happened. One composer keeps the row
 * renderer and the perform sink agreeing on the same key by construction. */
export function copyKey(id: string, subject: string): string {
  return `${id}:${subject}`;
}

/** Clipboard write with the webview fallback: navigator.clipboard needs a
 * secure context some webviews (webkit2gtk dev origins) don't grant, so
 * the write falls back to the classic textarea + execCommand path
 * (platform smoke check named in DOGFOOD §visual, Appendix B). */
export async function copyToClipboard(key: string, text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.className = "pp-offscreen";
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand("copy");
    } catch {
      // execCommand is deprecated and absent in newer engines: the write
      // did not land, so the register stays silent (no false "copied").
      ok = false;
    } finally {
      // WHY finally: an execCommand throw must not leak the offscreen
      // textarea — one orphaned focusable node per copy attempt, forever.
      ta.remove();
    }
    if (!ok) return; // no confirmation for a write that did not land
  }
  copyFlash.begin(key);
}
