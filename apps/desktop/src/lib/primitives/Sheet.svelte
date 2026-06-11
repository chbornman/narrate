<script lang="ts">
  /**
   * Quiet dismissable centered sheet (primitive): scrim or the close
   * button dismisses; Esc routes through the escape layer (host state).
   * NO focus trap — the sheet is informational, not modal (R5: the
   * redaction confirm is the app's only modal and owns its own frame).
   * Instances are ENUMERATED: Cheatsheet, DropConfirm — new instances are
   * spec changes (guardrail).
   */
  import X from "@lucide/svelte/icons/x";
  import type { Snippet } from "svelte";

  let {
    open,
    onclose,
    label,
    children,
  }: {
    open: boolean;
    onclose: () => void;
    label: string;
    children: Snippet;
  } = $props();
</script>

{#if open}
  <div class="scrim" role="presentation" onpointerdown={onclose}>
    <div
      class="sheet"
      role="dialog"
      aria-label={label}
      tabindex="-1"
      onpointerdown={(e) => e.stopPropagation()}
    >
      <button class="close" aria-label="Close" onclick={onclose}><X size={14} /></button>
      {@render children()}
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 75;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .sheet {
    position: relative;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 8px;
    padding: 18px 22px;
    max-width: min(720px, 90vw);
    max-height: 84vh;
    overflow-y: auto;
  }
  .close {
    position: absolute;
    top: 6px;
    right: 6px;
    border: none;
    background: transparent;
    color: var(--text-faint);
    padding: 2px 8px;
    display: inline-flex; /* center the Lucide X svg */
    align-items: center;
  }
  .close:hover {
    color: var(--text);
  }
</style>
