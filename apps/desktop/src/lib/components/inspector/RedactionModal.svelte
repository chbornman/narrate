<script lang="ts">
  /**
   * The redaction confirm — THE APP'S ONE MODAL (R5) — STAGE C OWNS THIS
   * FILE. It owns its own frame (no Modal primitive exists, by design):
   * focus trap between its two buttons, default-focused Cancel, and the
   * §8.4 REQUIRED copy — the verbatim content quoted, what redaction DOES,
   * what it does NOT do (external copies are out of reach; offline-volume
   * sidecars are scrubbed only on next mount — named explicitly, I4) —
   * with hold-to-confirm via logic/confirmhold.ts.
   *
   * Escape layer 1 cancels it before anything else closes: Esc is the one
   * key allowed to propagate to the global handler; every other key stops
   * here (focus is trapped, so all keydowns pass through this frame).
   */
  import { ui } from "../../state/app.svelte";
  import {
    fraction,
    HOLD_IDLE,
    press,
    release,
    tick,
    type HoldState,
  } from "../../logic/confirmhold";
  import { formatTime } from "../../logic/journal";

  const open = $derived(ui.inspector.redactTargetId !== null);
  const entry = $derived(
    ui.inspector.entries.find((e) => e.id === ui.inspector.redactTargetId) ?? null,
  );
  /** §8.4 item 1: the verbatim content about to be redacted, quoted. */
  const quoted = $derived.by(() => {
    if (entry === null) return "this entry";
    if (entry.kind === "stroke") return `the stroke drawn at ${formatTime(entry.ts)}`;
    return `“${entry.text ?? ""}”`;
  });
  /** §8.4 item 3: affected offline volumes, named when applicable. */
  const offlineVolumes = $derived(
    ui.roots.filter((r) => !r.online).map((r) => r.displayName),
  );

  let cancelEl: HTMLButtonElement | undefined = $state();
  let confirmEl: HTMLButtonElement | undefined = $state();

  // Default focus lands on Cancel (§8.4 item 4).
  $effect(() => {
    if (open) cancelEl?.focus();
  });

  // ---- hold-to-confirm drive (machine is pure; the timer lives here) -------

  // ~30 ticks across the 1.5 s HOLD_TO_CONFIRM_MS keeps the fill smooth;
  // the tick must stay far below the threshold or release() ends up doing
  // all the confirming defensively.
  const HOLD_TICK_MS = 50;

  let hold: HoldState = HOLD_IDLE;
  let progress = $state(0);
  let timer: ReturnType<typeof setInterval> | null = null;

  function stopTimer() {
    if (timer !== null) clearInterval(timer);
    timer = null;
  }

  function settle(next: HoldState) {
    hold = next;
    progress = fraction(hold, Date.now());
    if (hold.confirmed) {
      stopTimer();
      hold = HOLD_IDLE;
      progress = 0;
      void ui.inspector.confirmRedact();
    } else if (hold.heldSince === null) {
      stopTimer();
    }
  }

  function holdStart() {
    settle(press(hold, Date.now()));
    if (timer === null && hold.heldSince !== null)
      timer = setInterval(() => settle(tick(hold, Date.now())), HOLD_TICK_MS);
  }

  function holdEnd() {
    settle(release(hold, Date.now()));
  }

  $effect(() => {
    if (!open) {
      stopTimer();
      hold = HOLD_IDLE;
      progress = 0;
    }
    return stopTimer;
  });

  // ---- the frame: key ownership + the two-button focus trap ----------------

  function onFrameKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") return; // escape layer 1 (global handler)
    if (e.key === "Tab") {
      // Two focusables; Tab and Shift+Tab both swap (and never reach the
      // global lights-out row).
      e.preventDefault();
      (document.activeElement === cancelEl ? confirmEl : cancelEl)?.focus();
    }
    e.stopPropagation();
  }

  function onConfirmKeydown(e: KeyboardEvent) {
    if (e.key === " " || e.key === "Enter") {
      e.preventDefault(); // no synthetic click; the hold is the only path
      if (!e.repeat) holdStart();
    }
  }

  function onConfirmKeyup(e: KeyboardEvent) {
    if (e.key === " " || e.key === "Enter") {
      e.preventDefault();
      holdEnd();
    }
  }
</script>

{#if open}
  <div class="frame" role="presentation">
    <div
      class="modal"
      role="alertdialog"
      aria-modal="true"
      aria-label="Redact permanently"
      tabindex="-1"
      onkeydown={onFrameKeydown}
    >
      <h2>Redact permanently?</h2>

      <blockquote>{quoted}</blockquote>

      <!-- §8.4 item 2: what redaction DOES (claims are not copy-editable). -->
      <p>
        Redaction permanently scrubs this content from the database, from
        every search index and vector, and from every sidecar file it appears
        in. The event’s id and timestamp remain as a “redacted” marker for
        log continuity. A redaction can never be undone, and old sidecars can
        never merge the content back - redaction wins.
      </p>

      <!-- §8.4 item 3: what redaction does NOT do (I4 limits, named). -->
      <p>
        It cannot scrub copies exported or backed up outside Photoproof.
        Sidecars on currently-offline volumes are scrubbed only when that
        volume next connects - until then the content still exists on that
        disk{offlineVolumes.length > 0
          ? ` (offline now: ${offlineVolumes.join(", ")})`
          : ""}.
      </p>

      <div class="buttons">
        <button bind:this={cancelEl} class="cancel" onclick={() => ui.inspector.cancelRedact()}>
          Cancel
        </button>
        <button
          bind:this={confirmEl}
          class="confirm"
          onpointerdown={holdStart}
          onpointerup={holdEnd}
          onpointerleave={holdEnd}
          onkeydown={onConfirmKeydown}
          onkeyup={onConfirmKeyup}
        >
          <span class="fill" style:width="{progress * 100}%"></span>
          <span class="label">Hold to redact permanently</span>
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
  blockquote {
    margin: 0 0 10px;
    padding: 6px 10px;
    border-left: 2px solid var(--chrome-strong);
    color: var(--text);
    overflow-wrap: anywhere;
    max-height: 30vh;
    overflow-y: auto;
  }
  p {
    margin: 0 0 10px;
    color: var(--text-dim);
    font-size: 12px;
    line-height: 1.5;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 14px;
  }
  .cancel {
    background: var(--bg-raised);
    border: 1px solid var(--chrome-strong);
    border-radius: 4px;
    color: var(--text);
    padding: 5px 14px;
  }
  .confirm {
    position: relative;
    overflow: hidden;
    background: transparent;
    border: 1px solid var(--chrome-strong);
    border-radius: 4px;
    color: var(--text-dim);
    padding: 5px 14px;
  }
  .confirm .fill {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--journal-dot);
    opacity: 0.45;
  }
  .confirm .label {
    position: relative;
  }
  .confirm:hover {
    color: var(--text);
  }
</style>
