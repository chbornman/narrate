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

/** Clipboard write with the webview fallback: navigator.clipboard needs a
 * secure context some webviews (webkit2gtk dev origins) don't grant, so
 * the write falls back to the classic textarea + execCommand path
 * (platform smoke check named in DOGFOOD §visual, Appendix B). */
export async function copyToClipboard(key: string, text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.className = "pp-offscreen";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      ta.remove();
      if (!ok) return; // no confirmation for a write that did not land
    } catch {
      return;
    }
  }
  copyFlash.begin(key);
}
