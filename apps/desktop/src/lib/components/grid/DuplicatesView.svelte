<script lang="ts">
  /**
   * The Duplicates lens surface (DESIGN-DEDUP-AND-SIMILARITY.md "Tier 1"): an
   * opt-in DETECT + DISPLAY view over the current grid scope. The backend's
   * near-dup GROUPS arrive as bare member hashes (ui.dupeGroups); logic/dedup.ts
   * turns them into ordered clusters (biggest first), each split into the
   * representative to KEEP and the redundant copies. We render one row per
   * cluster: the representative highlighted with a "keep" marker, then the
   * redundant copies, with a per-group "x{count}" badge.
   *
   * DISPLAY ONLY. There is NO delete or archive here (v1): keep/cull is sidecar
   * truth, deliberately deferred to founder design. The one action is the
   * non-destructive "Select redundant copies", which multi-selects the
   * non-representatives via the grid's existing selection so the founder can SEE
   * (and act on, with existing verbs) what would go. No em-dashes in copy
   * (gate: check:emdash).
   */
  import Copy from "@lucide/svelte/icons/copy";
  import Star from "@lucide/svelte/icons/star";
  import { ui } from "../../state/app.svelte";
  import { thumbUrl } from "../../ipc/urls";
  import { toClusters, summaryCopy } from "../../logic/dedup";
  import {
    DEDUP_THRESHOLD_DEBOUNCE_MS,
    DEDUP_THRESHOLD_MIN,
    DEDUP_THRESHOLD_MAX,
  } from "../../tuning";
  import { debounce } from "../../logic/dedup";

  // The ordered clusters the view renders. The representative pick prefers the
  // highest-rated copy in scope (ui exposes the rating lookup through
  // selectRedundantDuplicates' twin); here we read ratings off the loaded grid
  // items, the same image set the lens scoped to.
  const ratingOf = (h: string): number | null =>
    ui.grid.rawItems.find((i) => i.hash === h)?.rating ?? null;

  // null groups = a scan in flight (show the quiet "scanning" line); [] = the
  // calm none-state; otherwise the ordered clusters.
  const clusters = $derived(
    ui.dupeGroups === null ? null : toClusters(ui.dupeGroups, ratingOf),
  );
  const summary = $derived(
    clusters === null ? "Scanning for near-duplicates" : summaryCopy(clusters),
  );
  const hasRedundant = $derived(
    clusters !== null && clusters.some((c) => c.redundant.length > 0),
  );

  // The looseness slider debounces the (O(n^2)) rescan while the founder drags:
  // only the settled value lands on ui.setDupeThreshold. A local mirror keeps the
  // thumb tracking the drag smoothly; the committed value is ui.dupeThreshold.
  let sliderValue = $state(ui.dupeThreshold);
  const sweep = debounce<number>(
    (v) => ui.setDupeThreshold(v),
    DEDUP_THRESHOLD_DEBOUNCE_MS,
  );
  function onSlider(e: Event) {
    const v = Number((e.currentTarget as HTMLInputElement).value);
    sliderValue = v;
    sweep.run(v);
  }
</script>

<div class="dupes">
  <div class="dupes-bar">
    <span class="summary">{summary}</span>

    <!-- Looseness slider (DESIGN-DEDUP-AND-SIMILARITY.md): sweep the Hamming
         threshold tighter (same file re-saved) to looser (light edits). Debounced
         so a drag does not fire a scan per tick. No em-dashes in copy. -->
    <label class="loose">
      tighter
      <input
        type="range"
        min={DEDUP_THRESHOLD_MIN}
        max={DEDUP_THRESHOLD_MAX}
        step="1"
        value={sliderValue}
        oninput={onSlider}
        aria-label="Near-duplicate looseness"
      />
      looser
      <span class="threshold">{sliderValue}/64</span>
    </label>

    {#if hasRedundant}
      <!-- The ONE non-destructive affordance: multi-select the redundant copies
           via the grid's existing selection. NOT a delete (v1 surfaces, never
           culls); it just gathers them so the founder sees and can act. -->
      <button
        class="select-redundant"
        onclick={() => ui.selectRedundantDuplicates()}
      >
        Select redundant copies
      </button>
    {/if}
  </div>

  {#if clusters === null}
    <!-- In-flight: a quiet line, no flash of the none-state. -->
    <p class="scanning">Scanning for near-duplicates</p>
  {:else if clusters.length === 0}
    <!-- The calm none-state (DESIGN-DEDUP-AND-SIMILARITY.md): no near-dups in the
         current scope at this looseness. -->
    <div class="empty">
      <p class="empty-head">No near-duplicates found</p>
      <p class="empty-sub">
        Nothing in this view looks like a re-saved or lightly edited copy of
        another. Try a looser threshold to widen the match.
      </p>
    </div>
  {:else}
    <div class="clusters">
      {#each clusters as cluster (cluster.representative)}
        <section class="cluster">
          <header class="cluster-head">
            <span class="badge">x{cluster.count}</span>
          </header>
          <div class="row">
            {#each cluster.members as hash, i (hash)}
              {@const isRep = i === 0}
              <!-- The representative (i === 0) is the one to KEEP: a brighter
                   ring + a star marker. The redundant copies dim slightly and
                   carry a copy glyph so the cluster reads at a glance. -->
              <figure class="member" class:rep={isRep}>
                <img
                  src={thumbUrl(hash)}
                  alt={isRep ? "Representative to keep" : "Near-duplicate copy"}
                  loading="lazy"
                  draggable="false"
                />
                <figcaption class="member-tag">
                  {#if isRep}
                    <Star size={11} /> keep
                  {:else}
                    <Copy size={11} /> copy
                  {/if}
                </figcaption>
              </figure>
            {/each}
          </div>
        </section>
      {/each}
    </div>
  {/if}
</div>

<style>
  .dupes {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--surround);
  }
  /* The lens' own thin chrome row: summary + looseness slider + the one
     non-destructive action. Mirrors the grid header's quiet tone. */
  .dupes-bar {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--chrome);
    font-size: 12px;
    flex-wrap: wrap;
  }
  .summary {
    color: var(--text-dim);
    white-space: nowrap;
  }
  .loose {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-faint);
  }
  .loose input {
    width: 120px;
    accent-color: var(--text-faint);
    background: transparent;
  }
  .threshold {
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
    min-width: 36px;
  }
  .select-redundant {
    margin-left: auto;
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    border-radius: 6px;
    padding: 3px 10px;
    color: var(--text-dim);
    white-space: nowrap;
  }
  .select-redundant:hover {
    color: var(--text);
    border-color: var(--text-faint);
  }
  /* The scrollable cluster list fills the rest. */
  .clusters {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .cluster {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .cluster-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .badge {
    font-size: 11px;
    color: var(--text-faint);
    border: 1px solid var(--chrome);
    border-radius: 9px;
    padding: 0 7px;
    font-variant-numeric: tabular-nums;
  }
  /* Members of one group sit together in a wrapping row, the representative
     first; a thin divider after the rep separates keep from copies. */
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: flex-start;
  }
  .member {
    margin: 0;
    width: 132px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .member img {
    width: 132px;
    height: 132px;
    object-fit: cover;
    border-radius: 4px;
    background: var(--bg-raised);
    border: 2px solid transparent;
    display: block;
  }
  /* The representative reads as the one to KEEP: a distinct focus-cell ring (the
     same token the grid active cell uses). Copies stay quiet with a subtle dim. */
  .member.rep img {
    border-color: var(--focus-cell);
  }
  .member:not(.rep) img {
    opacity: 0.78;
  }
  .member-tag {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10px;
    color: var(--text-faint);
  }
  .member.rep .member-tag {
    color: var(--text-dim);
  }
  .scanning {
    padding: 24px;
    color: var(--text-faint);
    font-size: 13px;
  }
  /* The calm none-state, centered. */
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 24px;
    text-align: center;
  }
  .empty-head {
    color: var(--text-dim);
    font-size: 14px;
    margin: 0;
  }
  .empty-sub {
    color: var(--text-faint);
    font-size: 12px;
    max-width: 360px;
    margin: 0;
  }
</style>
