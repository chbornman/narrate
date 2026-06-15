<script lang="ts">
  /**
   * The What's-Happening Station (UI §7 evolved; founder-confirmed June
   * 2026) — the capture indicator grown into the app-wide status organ.
   * Same bottom-right corner, two states:
   *
   *   COLLAPSED — a quiet icon row (mic · magnifier · info dot · note
   *   pencil, plus the visible-mode segments), breathing gently while
   *   anything is happening (logic/station.ts `pulsing`). The ingest
   *   hairline lives on the capsule edge.
   *
   *   HOVER — the capsule expands with real read-only context: the scope
   *   row (STILL the indicator↔inspector bridge — click opens the Journal
   *   tab via the registry's open-inspector row), the §5.4 streaming
   *   tether, micro-thumbnails, n-of-m in Look, and the activity rows
   *   (indexing/digest counts, download progress + retry hints,
   *   listening). Shrinks back on pointer leave; the info seat PINS it
   *   open (registry row toggle-station-detail) until Esc/outside-click.
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
  // Station 2.0 transient icons (DESIGN-STATION.md): work · embed · download
  // · missing-model warning. Fade in + pulse only while live, then retire.
  import Loader from "@lucide/svelte/icons/loader";
  import Sparkles from "@lucide/svelte/icons/sparkles";
  import Download from "@lucide/svelte/icons/download";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
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
  const hairline = $derived(segs.find((s) => s.id === "ingest"));
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
      ingest: ui.shell.ingest,
      runtime: ui.shell.runtime,
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
        {#if model.missingModels.length > 0}
          <!-- Missing-model prompts, FIRST-CLASS (DESIGN-STATION.md): a model
               a feature needs is absent, so the feature is silently dark. The
               Station is where that surfaces AND resolves — the fix is inline
               (Accept license first when gated, then Download), wired to the
               same runtime_* path Settings → Models drives. No faked progress;
               the row flips to the download transient on the next status. -->
          {#each model.missingModels as mm (mm.id)}
            <div class="missing">
              <span class="missing-icon"><TriangleAlert size={13} aria-hidden="true" /></span>
              <div class="missing-text">
                <span class="missing-feature">{mm.feature}</span>
                {#if mm.needsLicense}
                  <button class="missing-action" onclick={() => void ui.acceptModelLicense(mm.id)}>
                    Accept license
                  </button>
                {:else}
                  <button class="missing-action" onclick={() => void ui.downloadMissingModel(mm.id)}>
                    Download{mm.sizeLabel !== undefined ? ` (${mm.sizeLabel})` : ""}
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        {/if}
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
          <div
            class="activity"
            class:warn={a.kind === "offline" || a.kind === "download-failed"}
          >
            <span class="text">{a.text}</span>
            {#if a.hint !== undefined}<span class="hint">{a.hint}</span>{/if}
            {#if a.fraction !== undefined}
              <span class="bar">
                <span class="fill" style:width="{Math.round(a.fraction * 100)}%"></span>
              </span>
            {/if}
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
    {#if hairline !== undefined}
      <span class="hairline" title={hairline.title}>
        <span
          class="hairline-fill"
          style:width="{Math.round((hairline.fraction ?? 0) * 100)}%"
        ></span>
      </span>
    {/if}
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
    {#each model.transients as t (t.id)}
      <!-- transient icons (DESIGN-STATION.md): fade in + gently pulse only
           while their thing is live, then retire. Founder, June 13 2026:
           every transient is ALSO an individual click target — its registry
           verb (toggle-station-detail) pins the detail panel open, where its
           progress row or the missing-model download prompt resolves. A count
           badge + a progress arc ride the ingest/digest "work" icon. -->
      <button
        class="transient"
        class:warn={t.warn}
        aria-label={t.title}
        onclick={() => seatClick(t.actionId)}
        {@attach tooltip({ actionId: t.actionId, text: t.title })}
      >
        {#if t.fraction !== undefined}
          <!-- progress arc: a conic ring whose swept angle is the fraction,
               sitting behind the glyph (no SVG, token-colored). -->
          <span
            class="arc"
            style:--arc="{Math.round((t.fraction ?? 0) * 360)}deg"
          ></span>
        {/if}
        <span class="t-glyph">
          {#if t.icon === "loader"}
            <Loader size={13} aria-hidden="true" />
          {:else if t.icon === "sparkles"}
            <Sparkles size={13} aria-hidden="true" />
          {:else if t.icon === "download"}
            <Download size={13} aria-hidden="true" />
          {:else}
            <TriangleAlert size={13} aria-hidden="true" />
          {/if}
        </span>
        {#if t.count !== undefined}
          <span class="badge">{t.count > 999 ? "999+" : t.count.toLocaleString()}</span>
        {/if}
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
    /* Station 2.0: a larger collapsed footprint so the status border +
     * transient icons read at a glance (DESIGN-STATION.md). */
    height: 30px;
    padding: 0 6px;
    border-radius: 15px;
    background: var(--bg-overlay);
    /* A 2px ring so the status color (data-border) reads at a glance. */
    border: 2px solid var(--chrome);
    overflow: hidden;
    transition: border-color 160ms ease-out;
  }
  /* The collapsed pill's border = the most salient active state, by priority
   * resolved in logic/station.ts (borderState). Token-driven, theme-aware. */
  .capsule[data-border="mic"] {
    border-color: var(--station-mic);
  }
  .capsule[data-border="error"] {
    border-color: var(--station-error);
  }
  .capsule[data-border="working"] {
    border-color: var(--station-working);
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
  .hairline {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 2px;
    background: var(--chrome);
  }
  .hairline-fill {
    display: block;
    height: 2px;
    background: var(--text-dim);
    transition: width 300ms linear;
  }

  /* ---- transient icons (Station 2.0) ------------------------------------ */
  /* Fade in on mount and gently pulse while live; Svelte retires the node
   * when the source state clears (the {#each} key drops it). Quiet,
   * photography-app restrained — opacity only, nothing slides. */
  .transient {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    margin: 0 1px;
    /* now a button (each transient is its own click target): strip the chrome
     * but keep the icon look. A click pins the detail panel open. */
    padding: 0;
    border: none;
    background: transparent;
    cursor: pointer;
    color: var(--text-dim);
    animation:
      transient-in 240ms ease-out,
      transient-pulse 2.6s ease-in-out 240ms infinite;
  }
  /* hover brightens the glyph (the seats do the same) so the affordance reads */
  .transient:hover {
    color: var(--text);
  }
  .transient.warn:hover {
    color: var(--station-error);
  }
  /* The missing-model warning reads in the error/amber hue, not the cool
   * working gray — it is a call to act, not just "work in progress". */
  .transient.warn {
    color: var(--station-error);
  }
  @keyframes transient-in {
    from {
      opacity: 0;
      transform: scale(0.7);
    }
    to {
      opacity: 1;
      transform: scale(1);
    }
  }
  @keyframes transient-pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.5;
    }
  }
  .t-glyph {
    display: inline-flex;
    align-items: center;
    position: relative;
    z-index: 1;
  }
  /* The progress arc: a conic ring swept to the fraction, behind the glyph.
   * --arc (set inline) is the swept angle; the cool working hue draws it. */
  .arc {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background: conic-gradient(
      var(--station-working) var(--arc, 0deg),
      var(--chrome) 0
    );
    /* punch out the centre so it reads as a thin ring, not a pie */
    -webkit-mask: radial-gradient(circle, transparent 7px, #000 7px);
    mask: radial-gradient(circle, transparent 7px, #000 7px);
    opacity: 0.9;
  }
  /* The count badge: a small numeric chip clinging to the icon's corner. */
  .badge {
    position: absolute;
    bottom: -2px;
    right: -3px;
    min-width: 12px;
    height: 12px;
    padding: 0 2px;
    box-sizing: border-box;
    font-size: 9px;
    line-height: 12px;
    text-align: center;
    color: var(--text);
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    border-radius: 6px;
    z-index: 2;
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
  /* Missing-model prompt (first-class): the amber warning + the inline fix. */
  .missing {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 6px 8px;
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    border-radius: 6px;
  }
  .missing-icon {
    display: inline-flex;
    color: var(--station-error);
    margin-top: 1px;
  }
  .missing-text {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .missing-feature {
    font-size: 12px;
    color: var(--text);
  }
  .missing-action {
    align-self: flex-start;
    font-size: 11px;
    color: var(--text);
    background: var(--bg-overlay);
    border: 1px solid var(--chrome-strong);
    border-radius: 5px;
    padding: 2px 8px;
    cursor: pointer;
  }
  .missing-action:hover {
    border-color: var(--station-error);
    color: var(--station-error);
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
  /* Offline / failed rows read as a problem, not routine background work:
     the station error color, so a paused-because-offline cause stands out. */
  .activity.warn .text {
    color: var(--station-error);
  }
  .activity .hint {
    color: var(--text-faint);
    font-size: 11px;
  }
  .activity .bar {
    flex: 1;
    min-width: 40px;
    height: 2px;
    align-self: center;
    background: var(--chrome);
    border-radius: 1px;
    overflow: hidden;
  }
  .activity .fill {
    display: block;
    height: 2px;
    background: var(--text-dim);
    transition: width 300ms linear;
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
