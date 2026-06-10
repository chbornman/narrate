<script lang="ts">
  /**
   * Renders the sanctioned-toast queue (toast.svelte.ts), fixed above the
   * indicator, 5 s auto-dismiss. Row markup lives here — there is no
   * separate Toast.svelte (one consumer). Exempt from nothing: toasts are
   * transient feedback, not chrome, but in practice only inspector flows
   * (Stage C) ever enqueue one.
   */
  import { toasts, TOAST_DISMISS_MS } from "./toast.svelte";

  // One timer per visible toast; cleared if the toast is dismissed early.
  const timers = new Map<number, ReturnType<typeof setTimeout>>();
  $effect(() => {
    for (const t of toasts.items) {
      if (!timers.has(t.id)) {
        timers.set(
          t.id,
          setTimeout(() => {
            timers.delete(t.id);
            toasts.dismiss(t.id);
          }, TOAST_DISMISS_MS),
        );
      }
    }
    for (const [id, timer] of timers) {
      if (!toasts.items.some((t) => t.id === id)) {
        clearTimeout(timer);
        timers.delete(id);
      }
    }
  });
</script>

{#if toasts.items.length > 0}
  <div class="host" role="status" aria-label="Notifications">
    {#each toasts.items as t (t.id)}
      <div class="toast">
        <span class="text">{t.text}</span>
        {#if t.action !== undefined}
          <button class="action" onclick={() => toasts.act(t.id)}>
            {t.action.label}
          </button>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .host {
    position: fixed;
    right: 14px;
    bottom: 44px; /* above the indicator capsule */
    z-index: 65;
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: flex-end;
  }
  .toast {
    display: flex;
    align-items: center;
    gap: 10px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 6px;
    padding: 6px 10px;
    color: var(--text-dim);
  }
  .action {
    border: none;
    background: transparent;
    color: var(--text);
    padding: 0 2px;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
</style>
