<script lang="ts">
  /**
   * First-run welcome card (BACKLOG "First-run welcome card: how your data
   * is stored" — founder, dogfood round 3): plain words on what lives
   * where, shown once at launch. The frame/focus pattern is the redaction
   * modal's (the app's other self-owned modal — no Modal primitive exists,
   * by design): focus trap between its two focusables, Esc is the one key
   * allowed to propagate (it is escape layer 1, logic/escape.ts), every
   * other keydown stops here.
   *
   * The claims are deliberately blunt because the failure mode is real:
   * sidecars bind by FILENAME adjacency (SIDECARS §2.1) — rename an image
   * outside the app while it isn't watching and relinking depends on the
   * LIBRARY §7 heuristics. The card says so before the user learns it the
   * hard way. The index, by contrast, is rebuildable (SIDECARS principle 1).
   * The closing paragraph states the founder's hierarchy of importance
   * (June 2026): collections are the point, folders are mechanical.
   *
   * "Don't show again" persists via prefs.ts (default ON — the common path
   * reads this exactly once); the flag lives on the shell slice so the Esc
   * route honors the toggle identically (app.svelte.ts).
   */
  import { ui } from "../../state/app.svelte";

  const open = $derived(ui.shell.welcomeOpen);

  let toggleEl: HTMLInputElement | undefined = $state();
  let okEl: HTMLButtonElement | undefined = $state();

  // Default focus lands on the affirmative button: Enter dismisses.
  $effect(() => {
    if (open) okEl?.focus();
  });

  function onFrameKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") return; // escape layer 1 (global handler)
    if (e.key === "Tab") {
      // Two focusables; Tab and Shift+Tab both swap (and never reach the
      // global lights-out row).
      e.preventDefault();
      (document.activeElement === okEl ? toggleEl : okEl)?.focus();
    }
    e.stopPropagation();
  }
</script>

{#if open}
  <div class="frame" role="presentation">
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-label="How your data is stored"
      tabindex="-1"
      onkeydown={onFrameKeydown}
    >
      <h2>How your data is stored</h2>

      <p>
        Everything you write — notes, ratings, marks — is saved in small
        sidecar files named like the image plus
        <code>.photoproof.json</code>, sitting <strong>beside</strong> your
        photos. Those sidecar files <strong>are</strong> your data: back
        them up together with your images.
      </p>

      <p>
        Sidecars follow your photos by <strong>filename</strong>. If you
        rename or move a photo outside Photoproof while it isn’t running,
        the app has to guess to reconnect the photo with its journal — it
        usually gets it right, but renaming inside the watched folders
        while Photoproof is open is the safe way.
      </p>

      <p>
        Photoproof’s own database is only an index. It can always be
        rebuilt from your images and their sidecars — losing it never loses
        your work.
      </p>

      <!-- collections over folders (founder, June 2026): the card states
           the hierarchy of importance once, quietly -->
      <p>
        Folders here are mechanical — just where files happen to sit.
        <strong>Collections</strong> are what matter: gather images by
        intent, with the notes and context you collect. They live in the
        rail, beside Folders.
      </p>

      <div class="buttons">
        <label class="again">
          <input
            bind:this={toggleEl}
            type="checkbox"
            bind:checked={ui.shell.welcomeDontShowAgain}
          />
          Don’t show this again
        </label>
        <button bind:this={okEl} class="ok" onclick={() => ui.shell.dismissWelcome()}>
          Got it
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .frame {
    position: fixed;
    inset: 0;
    z-index: 80;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .modal {
    width: min(460px, 90vw);
    background: var(--bg-overlay);
    border: 1px solid var(--chrome-strong);
    border-radius: 8px;
    padding: 18px 20px;
  }
  h2 {
    margin: 0 0 10px;
    font-size: 14px;
    color: var(--text);
  }
  p {
    margin: 0 0 10px;
    color: var(--text-dim);
    font-size: 12px;
    line-height: 1.5;
  }
  p strong {
    color: var(--text);
    font-weight: 600;
  }
  code {
    font-family: var(--mono);
    font-size: 11px;
    color: var(--text);
  }
  .buttons {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    margin-top: 14px;
  }
  .again {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-dim);
    font-size: 12px;
  }
  .ok {
    background: var(--bg-raised);
    border: 1px solid var(--chrome-strong);
    border-radius: 4px;
    color: var(--text);
    padding: 5px 14px;
  }
</style>
