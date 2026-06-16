<script lang="ts">
  /**
   * The Library-status indicator (BACKLOG "digest visibility", founder June
   * 2026) — the header-center readout of whether the catalog is still being
   * built. It REPLACES the old titlebar "digesting" text and absorbs the
   * offline-drive warning that used to sit beside it; the bottom-right
   * Station is now capture-only.
   *
   *   COLLAPSED (centered, small) — three registers:
   *     · settled  -> a calm dot + "Library settled".
   *     · working  -> a quiet activity glyph + the current stage + "done /
   *       total" + a thin progress sliver + the overall "~6m".
   *     · blocked  -> amber + the top waiting-on reason (a drive offline,
   *       a model downloading, an embedder lane loading, or a failed embedder
   *       lane: image/text search unavailable).
   *
   *   EXPANDED (hover, also keyboard-focusable) — a panel that drops from the
   *   header with the full stage list (label · "240 / 5,000" · a bar · "~6m ·
   *   12/s"), a "Waiting on" section, and an errors row when errors > 0.
   *
   * The whole thing is a pure RENDER of logic/librarystatus.ts — no state, no
   * verbs (it is read-only status, unlike the Station's clickable seats). It
   * mirrors the Station's hover-open / pointer-tracked auto-close timing so
   * the two surfaces feel consistent.
   */
  import Loader from "@lucide/svelte/icons/loader";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import Popover from "../../primitives/Popover.svelte";
  import { ui } from "../../state/app.svelte";
  import {
    libraryStatusModel,
    formatEta,
    formatRate,
    formatCount,
  } from "../../logic/librarystatus";

  const model = $derived(
    libraryStatusModel({
      ingest: ui.shell.ingest,
      runtime: ui.shell.runtime,
    }),
  );

  // The collapsed pill foregrounds either a blocking reason (amber) or the
  // current working stage. Blocked wins: a paused drive is more urgent than
  // the stage it paused.
  const blocked = $derived(model.waitingOn.length > 0);
  const overallEta = $derived(formatEta(model.etaSecs));

  // Hover lifetime — mirror the Station's open-on-enter / delayed-close idiom
  // (Station.svelte HOVER_CLOSE_MS) so the panel bridges the gap between the
  // pill and the dropped panel without flickering. Keyboard focus also opens
  // it (focusin/focusout) so the indicator is reachable without a pointer.
  const HOVER_CLOSE_MS = 140;
  let open = $state(false);
  let anchorEl: HTMLDivElement | undefined = $state();
  let closeTimer: ReturnType<typeof setTimeout> | undefined;
  function openNow() {
    clearTimeout(closeTimer);
    open = true;
  }
  function scheduleClose() {
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => (open = false), HOVER_CLOSE_MS);
  }

  /** Keyboard parity for the hover reveal: Enter/Space toggles the panel,
   * Escape closes it. The pill is a legitimate button (it reveals detail), so
   * it carries role=button + aria-expanded for assistive tech. */
  function onPillKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      open = !open;
    } else if (e.key === "Escape") {
      open = false;
    }
  }
</script>

<div
  class="libstatus"
  role="status"
  aria-label="Library status"
  onpointerenter={openNow}
  onpointerleave={scheduleClose}
  onfocusin={openNow}
  onfocusout={scheduleClose}
>
  {#if open && anchorEl !== undefined}
    <Popover anchor={anchorEl} placement="bottom-center" ondismiss={() => (open = false)}>
      <!-- keep the hover alive while the pointer is over the dropped panel -->
      <div class="panel" onpointerenter={openNow} onpointerleave={scheduleClose} role="presentation">
        <div class="panel-head">
          <span class="panel-title">{model.headline}</span>
          {#if !model.settled && overallEta !== ""}
            <span class="panel-eta">{overallEta}</span>
          {/if}
        </div>

        {#if model.stages.length > 0}
          <div class="stages">
            {#each model.stages as s (s.id)}
              <div class="stage" class:done={s.state === "done"} class:pending={s.state === "pending"}>
                <div class="stage-row">
                  <span class="stage-label">{s.label}</span>
                  <span class="stage-count">{formatCount(s.done, s.total)}</span>
                </div>
                <span class="bar">
                  <span class="fill" style:width="{Math.round(s.fraction * 100)}%"></span>
                </span>
                {#if s.state === "working"}
                  {@const eta = formatEta(s.etaSecs)}
                  {@const rate = formatRate(s.ratePerSec)}
                  {#if eta !== "" || rate !== ""}
                    <span class="stage-meta">{[eta, rate].filter((p) => p !== "").join(" · ")}</span>
                  {/if}
                {/if}
              </div>
            {/each}
          </div>
        {:else if model.settled}
          <div class="empty">Nothing to build - the library is settled.</div>
        {/if}

        {#if model.waitingOn.length > 0}
          <div class="waiting">
            <span class="section-label">Waiting on</span>
            {#each model.waitingOn as w (w.id)}
              <!-- failed embedder lanes are DEGRADED: the error register (a
                   stronger weight) and the full error on a title attr. -->
              <div class="waiting-row" class:degraded={w.degraded === true} title={w.detail ?? w.text}>
                {w.text}
              </div>
            {/each}
          </div>
        {/if}

        {#if model.errors > 0}
          <div class="errors">
            {model.errors.toLocaleString()}
            {model.errors === 1 ? "error" : "errors"} during indexing
          </div>
        {/if}
      </div>
    </Popover>
  {/if}

  <!-- The collapsed pill. tabindex makes it keyboard-focusable so the panel
       opens without a pointer (the focusin handler on the wrapper fires). -->
  <div
    bind:this={anchorEl}
    class="pill"
    class:blocked
    class:working={!model.settled && !blocked}
    role="button"
    tabindex="0"
    aria-expanded={open}
    aria-label="Library status - {model.headline}"
    onkeydown={onPillKeydown}
    title={blocked ? (model.waitingOn[0].detail ?? model.waitingOn[0].text) : model.headline}
  >
    {#if blocked}
      <!-- A failed embedder lane is degraded: same amber error register as the
           other blockers; the full error rides the pill's title attr above. -->
      <span class="glyph amber"><TriangleAlert size={11} aria-hidden="true" /></span>
      <span class="label amber">{model.waitingOn[0].text}</span>
    {:else if model.settled}
      <span class="dot" aria-hidden="true"></span>
      <span class="label">Library settled</span>
    {:else}
      <span class="glyph spin"><Loader size={11} aria-hidden="true" /></span>
      <span class="label">{model.current?.label ?? model.headline}</span>
      {#if model.current !== undefined && model.current !== null && model.current.total > 0}
        <span class="count">{formatCount(model.current.done, model.current.total)}</span>
      {/if}
      <span class="sliver">
        <span class="sliver-fill" style:width="{Math.round((model.current?.fraction ?? 0) * 100)}%"></span>
      </span>
      {#if overallEta !== ""}<span class="eta">{overallEta}</span>{/if}
    {/if}
  </div>
</div>

<style>
  .libstatus {
    display: flex;
    align-items: center;
    justify-content: center;
  }
  /* The collapsed pill: small, centered, quiet. Same token palette as the
   * Station capsule so the two read as one design system. */
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 20px;
    padding: 0 8px;
    border-radius: 10px;
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    font-size: 11px;
    color: var(--text-dim);
    white-space: nowrap;
    max-width: 320px;
    cursor: default;
    transition: border-color 160ms ease-out;
  }
  .pill:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 1px;
  }
  .pill.working {
    border-color: var(--station-working);
  }
  .pill.blocked {
    border-color: var(--station-error);
  }
  /* the settled dot: a calm, dim marker — nothing is happening */
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text-faint);
    flex: 0 0 auto;
  }
  .glyph {
    display: inline-flex;
    align-items: center;
    color: var(--station-working);
  }
  .glyph.amber,
  .label.amber {
    color: var(--station-error);
  }
  /* the working glyph turns slowly — the ONLY motion, opacity-light and
   * photography-app restrained (matches the Station's quiet register). */
  .glyph.spin :global(svg) {
    animation: libstatus-spin 1.6s linear infinite;
  }
  @keyframes libstatus-spin {
    to {
      transform: rotate(360deg);
    }
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .count {
    color: var(--text-faint);
    font-variant-numeric: tabular-nums;
  }
  .eta {
    color: var(--text-faint);
  }
  /* the thin progress sliver on the collapsed pill */
  .sliver {
    width: 36px;
    height: 2px;
    background: var(--chrome);
    border-radius: 1px;
    overflow: hidden;
    flex: 0 0 auto;
  }
  .sliver-fill {
    display: block;
    height: 2px;
    background: var(--station-working);
    transition: width 300ms linear;
  }

  /* ---- the dropped panel (hover / focus expansion) ---------------------- */
  .panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 260px;
    max-width: 360px;
    padding: 10px 12px;
  }
  .panel-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }
  .panel-title {
    font-size: 12px;
    color: var(--text);
  }
  .panel-eta {
    font-size: 11px;
    color: var(--text-dim);
  }
  .stages {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .stage {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  /* done stages recede; pending (queued/paused) sit between done and working */
  .stage.done {
    opacity: 0.55;
  }
  .stage.pending {
    opacity: 0.8;
  }
  .stage-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    font-size: 12px;
  }
  .stage-label {
    color: var(--text);
  }
  .stage-count {
    color: var(--text-faint);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .bar {
    height: 3px;
    background: var(--chrome);
    border-radius: 2px;
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 3px;
    background: var(--station-working);
    transition: width 300ms linear;
  }
  .stage-meta {
    font-size: 11px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }
  .waiting {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-top: 6px;
    border-top: 1px solid var(--chrome);
  }
  .section-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-faint);
  }
  .waiting-row {
    font-size: 12px;
    color: var(--station-error);
  }
  /* a degraded row (a failed embedder lane) carries more weight than a
     transient wait — same error token, a touch bolder so it reads as a fault */
  .waiting-row.degraded {
    font-weight: 600;
  }
  .errors {
    font-size: 11px;
    color: var(--station-error);
    padding-top: 6px;
    border-top: 1px solid var(--chrome);
  }
  .empty {
    font-size: 12px;
    color: var(--text-faint);
  }
</style>
