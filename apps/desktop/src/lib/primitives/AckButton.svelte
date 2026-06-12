<script lang="ts">
  /**
   * A button for fire-and-forget verbs (primitive): runs the async verb,
   * disables while it's in flight, then shows the past-tense label for a
   * beat before reverting — the AckFlash register (ackflash.svelte.ts)
   * carries the timing truth; this component is just its one seat.
   * Consumers: Settings "Restart runtime" / "Re-detect hardware"; any
   * future verb whose completion would otherwise be invisible.
   */
  import { AckFlash } from "./ackflash.svelte";

  let {
    label,
    doneLabel,
    verb,
    quiet = false,
  }: {
    label: string;
    /** Past tense — the button briefly SAYS the verb landed. */
    doneLabel: string;
    verb: () => Promise<void>;
    /** The borderless dim variant (the Settings/ConsentCard idiom). */
    quiet?: boolean;
  } = $props();

  const ack = new AckFlash();
</script>

<button class:quiet disabled={ack.busy} onclick={() => void ack.run(verb)}>
  <!-- Both labels share one grid cell so the button keeps the wider
       label's width — the swap must not jiggle the row it sits in. -->
  <span class="labels">
    <span class="seat" class:off={ack.done}>{label}</span>
    <span class="seat" class:off={!ack.done}>{doneLabel}</span>
  </span>
</button>

<style>
  /* Scoped styles stop at the component boundary, so the quiet variant is
   * restated here (same three lines as SettingsApp/ConsentCard). */
  .quiet {
    background: transparent;
    border-color: transparent;
    color: var(--text-dim);
  }
  .labels {
    display: inline-grid;
    justify-items: center;
  }
  .seat {
    grid-area: 1 / 1;
    white-space: nowrap;
  }
  .seat.off {
    visibility: hidden;
  }
</style>
