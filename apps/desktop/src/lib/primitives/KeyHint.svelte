<script lang="ts">
  /**
   * Key-chord chip — THE ONLY component allowed to render key names
   * (menus, tooltips, cheatsheet). Platform glyphs: Cmd on macOS, Ctrl
   * elsewhere.
   */
  import type { KeyChord } from "../actions/types";
  import { isMac } from "../logic/platform";

  let { chord }: { chord: KeyChord } = $props();

  // The shared probe (logic/platform.ts) — same answer the window-chrome
  // split uses, so glyphs and chrome can never disagree about the OS.
  const mac = isMac();

  const KEY_NAMES: Record<string, string> = {
    " ": "Space",
    Escape: "Esc",
    ArrowUp: "↑",
    ArrowDown: "↓",
    ArrowLeft: "←",
    ArrowRight: "→",
    Backspace: "⌫",
  };

  const text = $derived.by(() => {
    const parts: string[] = [];
    if (chord.ctrlOrMeta === true) parts.push(mac ? "⌘" : "Ctrl");
    if (chord.shift === true) parts.push("Shift");
    const k = KEY_NAMES[chord.key] ?? (chord.key.length === 1 ? chord.key.toUpperCase() : chord.key);
    parts.push(k);
    return parts.join("+");
  });
</script>

<kbd>{text}</kbd>

<style>
  kbd {
    font-family: var(--font);
    font-size: 11px;
    color: var(--text-faint);
    border: 1px solid var(--chrome);
    border-radius: 3px;
    padding: 0 4px;
    white-space: nowrap;
  }
</style>
