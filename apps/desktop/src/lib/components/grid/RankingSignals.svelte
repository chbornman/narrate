<script lang="ts">
  /**
   * The ⚙ "Ranking signals" popover (search-as-scope Phase 3): makes the B75
   * fusion weights USER-VISIBLE as on/off checkboxes. Founder decision D2 —
   * on/off first; continuous weight sliders and the similarity-tilt control are
   * eval-gated / Phase 4, so this popover ships NEITHER.
   *
   * Each checkbox includes/excludes one fusion signal: checked sends its B75
   * default weight, unchecked sends 0 (the signal drops out of the fusion). The
   * toggles are SEMANTIC-LANE ONLY by construction — S1/S3/S4 don't exist on
   * the lexical keystroke path, so changing them only re-runs a committed
   * semantic scope and can never tax the <100 ms as-you-type budget. While this
   * popover is open the semantic search also asks for per-signal debug, so each
   * grid cell can show its contribution (the "show, don't just tune" rule).
   *
   * Esc is routed by the HOST (the Popover primitive never self-handles it).
   */
  import Popover from "../../primitives/Popover.svelte";
  import { ui } from "../../state/app.svelte";
  import { SIGNALS } from "../../logic/ranking";

  let { anchor }: { anchor: HTMLElement | null } = $props();

  // The hint copy adapts to the live scope: the toggles only bite a committed
  // semantic search, so say so plainly when one is NOT active (no em-dashes —
  // the check:emdash gate).
  const semanticActive = $derived(
    ui.gridScope.kind === "query" && ui.searchLane === "semantic",
  );
</script>

<Popover {anchor} ondismiss={() => void ui.setRankingPopover(false)}>
  <div class="ranking" role="group" aria-label="Ranking signals">
    <div class="title">Ranking signals</div>
    {#each SIGNALS as sig (sig.key)}
      <label class="signal">
        <input
          type="checkbox"
          checked={ui.signalToggles[sig.key]}
          onchange={(e) => void ui.setSignal(sig.key, e.currentTarget.checked)}
        />
        <span class="code">{sig.code}</span>
        <span class="label">{sig.label}</span>
      </label>
    {/each}
    <div class="foot">
      <span class="note">
        {semanticActive
          ? "Showing per signal contributions"
          : "Applies to semantic search (press Enter)"}
      </span>
      <button class="reset" onclick={() => void ui.resetSignals()}>Reset to defaults</button>
    </div>
  </div>
</Popover>

<style>
  .ranking {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px;
    min-width: 220px;
    font-size: 13px;
  }
  .title {
    color: var(--text-dim);
    font-size: 12px;
    padding: 0 4px 4px;
  }
  .signal {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 4px;
    border-radius: 4px;
    cursor: pointer;
  }
  .signal:hover {
    background: var(--bg-raised);
  }
  .signal input {
    accent-color: var(--text-faint);
    margin: 0;
  }
  .code {
    color: var(--text-faint);
    width: 18px;
  }
  .label {
    color: var(--text);
  }
  .foot {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 6px;
    padding: 6px 4px 0;
    border-top: 1px solid var(--chrome);
  }
  .note {
    color: var(--text-faint);
    font-size: 11px;
  }
  .reset {
    align-self: flex-start;
    border: 1px solid var(--chrome);
    background: transparent;
    color: var(--text-dim);
    border-radius: 4px;
    padding: 2px 8px;
    font-size: 12px;
    cursor: pointer;
  }
  .reset:hover {
    color: var(--text);
    border-color: var(--text-faint);
  }
</style>
