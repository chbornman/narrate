<script lang="ts">
  /**
   * The Capture Station (UI §7 evolved; founder June 2026, digest-visibility
   * split) — CAPTURE-ONLY now. The digest/ingest/embed/download/offline/
   * settled signaling moved to the header Library-status indicator
   * (LibraryStatus.svelte); this corner is the capture organ alone. Same
   * bottom-right corner, two states:
   *
   *   COLLAPSED — a quiet icon row (mic · magnifier · info dot · note
   *   pencil, plus the visible-mode segments), breathing gently while CAPTURE
   *   is happening (logic/station.ts `pulsing` — streaming speech / speech
   *   detected; background digest work no longer breathes it).
   *
   *   HOVER — the capsule expands with read-only CAPTURE context: the scope
   *   row (STILL the indicator↔inspector bridge — click opens the Journal
   *   tab via the registry's open-inspector row), the §5.4 streaming tether,
   *   micro-thumbnails, n-of-m in Look, and the capture activity rows
   *   (listening / arming / capturing). Shrinks back on pointer leave; the
   *   info seat PINS it open (toggle-station-detail) until Esc/outside-click.
   *
   * Founder ruling on status-vs-launcher: ICONS are the click targets —
   * each seat dispatches its registry row via resolveAction (one action
   * table, zero new verbs) — and the expanded body is READ-ONLY except
   * the scope bridge, which the founder kept exactly. Events POP from the
   * station as short rising chips (shell.pops): note shipped, mic
   * armed/disarmed, an utterance captured. No toasts, no sound.
   *
   * EXEMPT from Tab lights-out (coordinator ruling, DECISIONS U5, carried
   * forward: the station is capture-state truth — modes must stay visible).
   */
  import Info from "@lucide/svelte/icons/info";
  import Mic from "@lucide/svelte/icons/mic";
  import MicOff from "@lucide/svelte/icons/mic-off";
  import Pencil from "@lucide/svelte/icons/pencil";
  import Search from "@lucide/svelte/icons/search";
  import { ui } from "../../state/app.svelte";
  import { segments } from "../../logic/segments";
  import { stationModel } from "../../logic/station";
  import { scopeTargets } from "../../logic/scope";
  import { thumbUrl } from "../../ipc/urls";
  import { resolveAction } from "../../actions/registry";
  import { tooltip } from "../../primitives/tooltip";
  import Popover from "../../primitives/Popover.svelte";

  let capsuleEl: HTMLDivElement | undefined = $state();

  // segments.ts stays the truth for scope/tether/position/modes — the
  // station model adds activities and seats over it, never replaces it.
  const segs = $derived(
    segments({
      ingest: ui.shell.ingest,
      scope: ui.shell.scope,
      lookPosition:
        ui.viewMode === "look" && ui.look.order.length > 0
          ? { index: ui.look.index, total: ui.look.order.length }
          : null,
      // §5.4: a still-streaming utterance bound to an earlier scope
      // tethers the scope segment until finalization.
      streaming: ui.shell.streamingSegment(),
      ctx: ui.actionContext(),
    }),
  );
  const scopeSeg = $derived(segs.find((s) => s.id === "scope"));
  const positionSeg = $derived(segs.find((s) => s.id === "position"));
  // Visible modes stay on the COLLAPSED chrome (featureset §0 — a mode
  // hidden behind a hover would be an invisible mode). The mic mode
  // segment is superseded by the state-aware mic SEAT.
  const modeSegs = $derived(
    segs.filter((s) => s.id.startsWith("mode:") && s.id !== "mode:mic"),
  );

  const model = $derived(
    stationModel({
      micState: ui.shell.mic,
      asrReady: ui.shell.asrReady,
      streaming:
        ui.shell.streamingUtterance === null
          ? null
          : {
              kind: ui.shell.streamingUtterance.boundScope.kind,
              count: ui.shell.streamingUtterance.boundScope.count,
            },
    }),
  );

  // Hover opens, pointer-leave shrinks; the info seat's pin holds it.
  const expanded = $derived(ui.shell.popoverOpen || ui.shell.stationPinned);

  // Hover intent (founder, June 13 2026): the detail panel now floats ABOVE
  // the pill with an 8px gap, so the pointer crosses empty space travelling
  // from an icon up into the panel. A short close delay bridges that gap —
  // re-entering (the panel or the pill) inside the window cancels the close,
  // so the panel never flickers shut mid-traverse. Pinned stays open
  // regardless; this only governs the hover lifetime.
  const HOVER_CLOSE_MS = 140;
  let closeTimer: ReturnType<typeof setTimeout> | undefined;
  function openHover() {
    clearTimeout(closeTimer);
    ui.shell.popoverOpen = true;
  }
  function scheduleClose() {
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => (ui.shell.popoverOpen = false), HOVER_CLOSE_MS);
  }

  /** Every seat dispatches its registry row — availability and enablement
   * gate on the def (resolveAction), never here. The optional arg is the
   * seat's spelling of a parametrized row (mic-press + "toggle": a click
   * IS a tap — the two-gesture machine belongs to the key alone). */
  function seatClick(actionId: Parameters<typeof resolveAction>[0], arg?: string) {
    const action = resolveAction(actionId, ui.actionContext(), arg);
    if (action !== null) void ui.perform(action);
  }

  /** Scope row click → the inspector's Journal tab for the active image
   * (the registry row J dispatches; the bridge kept exactly). */
  function openJournal() {
    const action = resolveAction("open-inspector", ui.actionContext(), "journal");
    if (action !== null) void ui.perform(action);
  }

  // Micro-thumbnails of the scoped images, up to the cap then "+N"
  // (UI §7.2) — now a row INSIDE the expansion. The slice and the +N
  // arithmetic must subtract the SAME cap or the count lies.
  const POPOVER_THUMB_MAX = 8;
  const popThumbs = $derived(
    scopeTargets({
      viewMode: ui.viewMode,
      // Search is no longer a separate selection surface (M3): results are
      // grid cells, so the scope is the grid selection in every non-Look
      // case. searchOpen/searchSelection held false/empty for scope.ts.
      searchOpen: false,
      gridSelection: ui.grid.selectionTargets,
      searchSelection: [],
      lookTargets: ui.look.currentTargets,
      // The visualizer's selected node owns scope when active (R6).
      viewSelection: ui.viewSelection,
    }).slice(0, POPOVER_THUMB_MAX),
  );
  const popMore = $derived(Math.max(0, ui.shell.scope.count - POPOVER_THUMB_MAX));

  // Commit-pulse hold time (UI §7.4: "a single ~300 ms brightness pulse").
  // Distinct from `model.pulsing` — that is the slow something-is-happening
  // breath; THIS is the sharp an-event-just-committed blink.
  const PULSE_MS = 320;
  let pulsing = $state(false);
  let pulseTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (ui.shell.pulseCount === 0) return;
    pulsing = false;
    clearTimeout(pulseTimer);
    requestAnimationFrame(() => (pulsing = true));
    pulseTimer = setTimeout(() => (pulsing = false), PULSE_MS);
  });
</script>

<div
  class="station"
  role="status"
  aria-label="What's-happening station"
  onpointerenter={openHover}
  onpointerleave={scheduleClose}
>
  <!-- the pop move: short rising chips, retired when their animation ends -->
  {#each ui.shell.pops as pop, i (pop.id)}
    <span
      class="pop-chip"
      style:bottom="{30 + i * 20}px"
      onanimationend={() => ui.shell.dismissPop(pop.id)}>{pop.text}</span
    >
  {/each}

  {#if expanded && capsuleEl !== undefined}
    <Popover anchor={capsuleEl} placement="top-end" ondismiss={() => ui.shell.closeStation()}>
      <div class="body">
        {#if scopeSeg !== undefined}
          <!-- the one clickable element in the body: the indicator↔inspector
               bridge, kept exactly (everything else is read-only context) -->
          <button
            class="scope-btn"
            aria-label="Open the journal for this image"
            onclick={openJournal}
            {@attach tooltip({ actionId: "open-inspector", verb: "Journal", arg: "journal" })}
          >
            <span class="segment scope" class:pulsing>{scopeSeg.text}</span>
            <span class="scope-hint">{scopeSeg.title}</span>
          </button>
        {/if}
        {#if ui.shell.scope.kind !== "session" && popThumbs.length > 0}
          <div class="pop-thumbs">
            {#each popThumbs as h (h)}
              <img src={thumbUrl(h)} alt="" draggable="false" />
            {/each}
            {#if popMore > 0}<span class="more">+{popMore}</span>{/if}
          </div>
        {/if}
        {#if positionSeg !== undefined}
          <div class="row dim" title={positionSeg.title}>{positionSeg.text}</div>
        {/if}
        {#each model.activities as a (a.id)}
          <!-- capture activity rows only (mic / streaming utterance); the
               digest rows moved to the header Library-status indicator. -->
          <div class="activity">
            <span class="text">{a.text}</span>
          </div>
        {:else}
          <div class="row dim">Nothing happening - the library is settled.</div>
        {/each}
      </div>
    </Popover>
  {/if}

  <div
    bind:this={capsuleEl}
    class="capsule"
    class:breathing={model.pulsing}
    class:pulsing
    data-border={model.border}
  >
    {#each model.seats as seat (seat.id)}
      <!-- each seat is a clickable verb: its registry row, nothing else -->
      <button
        class="seat"
        onclick={() => seatClick(seat.actionId, seat.arg)}
        aria-label={seat.title}
        {@attach tooltip({ actionId: seat.actionId, text: seat.title, arg: seat.arg })}
      >
        <span
          class="segment glyph"
          class:dim={seat.tone === "dim"}
          class:live={seat.tone === "live"}
        >
          {#if seat.icon === "mic"}
            <Mic size={12} aria-hidden="true" />
          {:else if seat.icon === "mic-off"}
            <MicOff size={12} aria-hidden="true" />
          {:else if seat.icon === "search"}
            <Search size={12} aria-hidden="true" />
          {:else if seat.icon === "info"}
            <Info size={12} aria-hidden="true" />
          {:else}
            <Pencil size={12} aria-hidden="true" />
          {/if}
        </span>
      </button>
    {/each}
    {#each modeSegs as seg (seg.id)}
      <!-- visible modes (auto-advance, pencil) — status text, not seats;
           rendered as TEXT so the note seat keeps the only pencil glyph -->
      <span class="segment mode" class:dim={seg.tone === "dim"} title={seg.title}>
        {seg.text}
      </span>
    {/each}
  </div>
</div>

<style>
  .station {
    position: fixed;
    right: 14px;
    bottom: 12px;
    z-index: 50;
  }
  .capsule {
    position: relative;
    display: flex;
    align-items: center;
    /* A comfortable collapsed footprint so the capture seats + the mic
     * recording border read at a glance. */
    height: 30px;
    padding: 0 6px;
    border-radius: 15px;
    background: var(--bg-overlay);
    /* A 2px ring so the mic recording state (data-border) reads at a glance. */
    border: 2px solid var(--chrome);
    overflow: hidden;
    transition: border-color 160ms ease-out;
  }
  /* The collapsed pill's border, now capture-only: mic recording red when
   * armed (the error/working edges left with the digest signaling — the
   * header Library-status indicator owns those now). */
  .capsule[data-border="mic"] {
    border-color: var(--station-mic);
  }
  /* The §7.4 commit pulse, relocated from the scope text to the capsule
   * edge (the scope dot lives in the hover now): one short brighten. It only
   * shows when no status border owns the edge — a status color must win. */
  .capsule.pulsing[data-border="none"] {
    border-color: var(--text-dim);
  }
  /* The something-is-happening breath: ONE pulse driver, gated on the
   * station model's `pulsing` — opacity only, slow, photography-app
   * restrained; nothing moves. */
  .capsule.breathing {
    animation: station-breathe 3.2s ease-in-out infinite;
  }
  @keyframes station-breathe {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.62;
    }
  }
  .seat {
    display: flex;
    align-items: center;
    height: 100%;
    border: none;
    background: transparent;
    padding: 0 6px;
  }
  .seat:hover .segment {
    color: var(--text);
  }
  .segment {
    color: var(--text-dim);
    font-size: 12px;
    white-space: nowrap;
  }
  .segment.glyph {
    display: inline-flex;
    align-items: center;
  }
  .segment.mode {
    padding: 0 6px;
  }
  .segment.dim {
    color: var(--text-faint);
  }
  /* The §7.3 "faint slow breathing" while VAD detects speech — opacity
   * only, token colors untouched, nothing moves. */
  .segment.live {
    color: var(--text);
    animation: mic-breathe 2.4s ease-in-out infinite;
  }
  @keyframes mic-breathe {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.45;
    }
  }

  /* ---- the expansion body (read-only context) --------------------------- */
  .body {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 240px;
    max-width: 360px;
    padding: 8px 10px;
  }
  .scope-btn {
    display: flex;
    align-items: baseline;
    gap: 8px;
    border: none;
    background: transparent;
    padding: 0;
    text-align: left;
  }
  .segment.scope {
    transition:
      color 120ms ease-out,
      text-shadow 120ms ease-out;
  }
  .scope-btn:hover .segment.scope {
    color: var(--text);
  }
  .segment.scope.pulsing {
    color: var(--text);
    text-shadow: 0 0 6px var(--text-dim);
  }
  .scope-hint {
    color: var(--text-faint);
    font-size: 11px;
  }
  .row {
    font-size: 12px;
  }
  .row.dim {
    color: var(--text-faint);
  }
  .activity {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 12px;
    color: var(--text-dim);
  }
  .pop-thumbs {
    display: flex;
    gap: 4px;
    align-items: center;
  }
  .pop-thumbs img {
    width: 36px;
    height: 36px;
    object-fit: cover;
    border-radius: 2px;
    background: var(--bg-raised);
  }
  .more {
    color: var(--text-dim);
    font-size: 11px;
    padding: 0 4px;
  }

  /* ---- the pop move ------------------------------------------------------ */
  .pop-chip {
    position: absolute;
    right: 8px;
    font-size: 11px;
    color: var(--text-dim);
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 9px;
    padding: 1px 8px;
    pointer-events: none;
    white-space: nowrap;
    animation: station-pop 1100ms ease-out forwards;
  }
  @keyframes station-pop {
    0% {
      opacity: 0;
      transform: translateY(6px);
    }
    18% {
      opacity: 1;
      transform: translateY(0);
    }
    70% {
      opacity: 1;
    }
    100% {
      opacity: 0;
      transform: translateY(-14px);
    }
  }
</style>
