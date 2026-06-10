<script lang="ts">
  /**
   * Typed-note input (UI §6): a transient floating input anchored above the
   * indicator, ~420 px, scope echoed as dimmed placeholder. Enter submits
   * and it vanishes immediately; Escape cancels; drafts are discarded
   * without prompt. Never persistent chrome.
   */
  import { ui } from "../state/app.svelte";
  import { notePlaceholder } from "../logic/scope";

  let textareaEl: HTMLTextAreaElement | undefined = $state();
  let draft = $state("");

  $effect(() => {
    if (ui.note.open) {
      draft = "";
      textareaEl?.focus();
    }
  });

  const placeholder = $derived(
    ui.note.snapshot !== null
      ? notePlaceholder(ui.note.snapshot.kind, ui.note.snapshot.count)
      : "",
  );

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      e.stopPropagation();
      const text = draft;
      draft = "";
      void ui.submitNote(text); // vanishes immediately; pulse on commit
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      draft = "";
      ui.cancelNote();
    }
    // Shift+Enter: newline (textarea default).
  }
</script>

{#if ui.note.open}
  <div class="note">
    <textarea
      bind:this={textareaEl}
      bind:value={draft}
      {placeholder}
      rows="2"
      onkeydown={onKeydown}
      data-note-input
    ></textarea>
  </div>
{/if}

<style>
  .note {
    position: fixed;
    right: 14px;
    bottom: 44px; /* anchored above the indicator */
    width: 420px;
    z-index: 60;
  }
  textarea {
    width: 100%;
    resize: none;
    background: var(--bg-overlay);
    border-color: var(--chrome-strong);
    line-height: 1.4;
  }
  textarea::placeholder {
    color: var(--text-faint);
  }
</style>
