<script lang="ts">
  /**
   * Centered dimmed line + optional next-action slot (featureset §6:
   * "empty states say the next action"). Consumers: FirstRun, an empty
   * Grid folder, JournalTab "Nothing yet.".
   */
  import type { Snippet } from "svelte";

  let { line, action }: { line: string; action?: Snippet } = $props();
</script>

<div class="empty">
  <p>{line}</p>
  {#if action !== undefined}
    {@render action()}
  {/if}
</div>

<style>
  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 14px;
  }
  p {
    color: var(--text-faint);
    margin: 0;
  }
</style>
