<script lang="ts">
  /**
   * The capture indicator (UI §7), a renderer over logic/segments.ts:
   * ingest hairline · scope (the pulse target) · n-of-m in Look · mode
   * segments (auto-advance; reserved mic seat). Hover = scope popover
   * (micro-thumbnails via the Popover primitive).
   *
   * The indicator stays a STATUS strip — it is not the content panel — but
   * its SCOPE segment is clickable (dogfood round 1, indicator↔inspector
   * bridge): click opens the inspector on the Journal tab for the active
   * image, dispatching the registry's open-inspector row via resolveAction
   * (the same Action J dispatches; tooltip resolves from the same def).
   * The rest of the capsule keeps click = summon the note input.
   *
   * EXEMPT from Tab lights-out (coordinator ruling, DECISIONS U5: the
   * indicator is capture-state truth — modes must stay visible; future mic
   * evidence lives here).
   */
  import Mic from "@lucide/svelte/icons/mic";
  import MicOff from "@lucide/svelte/icons/mic-off";
  import Pencil from "@lucide/svelte/icons/pencil";
  import { ui } from "../../state/app.svelte";
  import { segments } from "../../logic/segments";
  import { scopeTargets } from "../../logic/scope";
  import { thumbUrl } from "../../ipc/urls";
  import { resolveAction } from "../../actions/registry";
  import { tooltip } from "../../primitives/tooltip";
  import Popover from "../../primitives/Popover.svelte";

  let capsuleEl: HTMLDivElement | undefined = $state();

  const segs = $derived(
    segments({
      ingest: ui.shell.ingest,
      scope: ui.shell.scope,
      lookPosition:
        ui.surface === "look" && ui.look.order.length > 0
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
  // The mic segment is its OWN button (P6.4: click = the M toggle, via
  // the registry row — the audit's pointer path); the rest stay in the
  // note zone.
  const micSeg = $derived(segs.find((s) => s.id === "mode:mic"));
  const noteSegs = $derived(
    segs.filter((s) => s.id !== "ingest" && s.id !== "scope" && s.id !== "mode:mic"),
  );

  /** Scope segment click → the inspector's Journal tab for the active
   * image (the registry row J uses; zero new verbs). */
  function openJournal() {
    const action = resolveAction("open-inspector", ui.actionContext(), "journal");
    if (action !== null) void ui.perform(action);
  }

  /** Mic segment click → the registry's mic row, resolved with arg
   * "toggle": a click IS a tap, so the pointer form keeps the plain
   * toggle while the M KEY runs the two-gesture machine (tap toggles,
   * hold is push-to-talk — logic/michold.ts). Same def, zero new verbs. */
  function toggleMic() {
    const action = resolveAction("mic-press", ui.actionContext(), "toggle");
    if (action !== null) void ui.perform(action);
  }

  // Hover popover: micro-thumbnails of the scoped images, up to the cap
  // then "+N" (UI §7.2). Sourced from the same lists the scope derives
  // from; the echoed scope stays authoritative for the count. The slice
  // and the +N arithmetic must subtract the SAME cap or the count lies —
  // one constant keeps them honest.
  const POPOVER_THUMB_MAX = 8;
  const popThumbs = $derived(
    scopeTargets({
      surface: ui.surface,
      searchOpen: ui.searchOpen,
      gridSelection: ui.grid.selectionTargets,
      searchSelection: ui.searchSel.order,
      lookTargets: ui.look.currentTargets,
    }).slice(0, POPOVER_THUMB_MAX),
  );
  const popMore = $derived(Math.max(0, ui.shell.scope.count - POPOVER_THUMB_MAX));

  // Pulse hold time. The binding anchor is UI §7.4: "a single ~300 ms
  // brightness pulse of the scope dot" — 320 implements that spec value.
  // Secondary floor: it must also outlast the 120 ms color/text-shadow
  // transition on .segment.scope (style block below) so the brighten
  // visibly completes before the segment relaxes.
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
  class="indicator"
  role="status"
  aria-label="Capture indicator"
  onpointerenter={() => (ui.shell.popoverOpen = true)}
  onpointerleave={() => (ui.shell.popoverOpen = false)}
>
  {#if ui.shell.popoverOpen && ui.shell.scope.kind !== "session" && popThumbs.length > 0 && capsuleEl !== undefined}
    <Popover anchor={capsuleEl} placement="top-end" ondismiss={() => (ui.shell.popoverOpen = false)}>
      <div class="pop-thumbs">
        {#each popThumbs as h (h)}
          <img src={thumbUrl(h)} alt="" draggable="false" />
        {/each}
        {#if popMore > 0}<span class="more">+{popMore}</span>{/if}
      </div>
    </Popover>
  {/if}

  <div bind:this={capsuleEl} class="capsule">
    {#if hairline !== undefined}
      <span class="hairline" title={hairline.title}>
        <span
          class="hairline-fill"
          style:width="{Math.round((hairline.fraction ?? 0) * 100)}%"
        ></span>
      </span>
    {/if}
    {#if scopeSeg !== undefined}
      <!-- the indicator↔inspector bridge: status text, journal on click -->
      <button
        class="zone scope-btn"
        aria-label="Open the journal for this image"
        onclick={openJournal}
        {@attach tooltip({ actionId: "open-inspector", verb: "Journal", arg: "journal" })}
      >
        <span class="segment scope" class:pulsing>{scopeSeg.text}</span>
      </button>
    {/if}
    {#if micSeg}
      <!-- mic glyph (CAPTURE §6.4/§11 via modes.ts; Lucide, never emoji):
           dim = off/degraded, live = the quiet breathing affordance while
           speech is detected; the title is the §7.3 one-line hover copy.
           Click = the M toggle (P6.4) — the audit's pointer path. -->
      <button class="zone" onclick={toggleMic} aria-label="Toggle microphone" title={micSeg.title}>
        <span
          class="segment glyph"
          class:dim={micSeg.tone === "dim"}
          class:live={micSeg.tone === "live"}
        >
          {#if micSeg.icon === "mic-off"}
            <MicOff size={12} aria-hidden="true" />
          {:else}
            <Mic size={12} aria-hidden="true" />
          {/if}
        </span>
      </button>
    {/if}
    <button class="zone note-btn" onclick={() => ui.summonNote()} aria-label="Write a note">
      {#each noteSegs as seg (seg.id)}
        <span
          class="segment"
          class:glyph={seg.icon !== undefined}
          class:dim={seg.tone === "dim"}
          class:live={seg.tone === "live"}
          title={seg.title}
        >
          {#if seg.icon === "pencil"}
            <Pencil size={12} aria-hidden="true" />
          {:else}
            {seg.text}
          {/if}
        </span>
      {/each}
    </button>
  </div>
</div>

<style>
  .indicator {
    position: fixed;
    right: 14px;
    bottom: 12px;
    z-index: 50;
  }
  .capsule {
    position: relative;
    display: flex;
    align-items: center;
    height: 24px;
    padding: 0;
    border-radius: 12px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    overflow: hidden;
  }
  /* The two click zones tile the capsule; visually they are the capsule. */
  .zone {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 100%;
    border: none;
    background: transparent;
    padding: 0;
  }
  .scope-btn {
    padding-left: 10px;
  }
  .note-btn {
    flex: 1;
    min-width: 12px; /* note stays pointer-reachable with no other segments */
    padding: 0 10px 0 8px;
  }
  .segment {
    color: var(--text-dim);
    font-size: 12px;
    white-space: nowrap;
  }
  /* Lucide glyph segments (mic, pencil): center the icon on the strip. */
  .segment.glyph {
    display: inline-flex;
    align-items: center;
  }
  /* While speech is detected the mic brightens AND breathes — the
   * armed-idle vs armed-speaking difference must be readable at a
   * glance (founder dogfood: "can't tell if it is doing anything"). */
  .segment.glyph.live {
    color: var(--text);
  }
  .segment.scope {
    transition:
      color 120ms ease-out,
      text-shadow 120ms ease-out;
  }
  /* Hover affordance for the journal bridge (quiet: text brightens). */
  .scope-btn:hover .segment.scope {
    color: var(--text);
  }
  .segment.scope.pulsing {
    color: var(--text);
    text-shadow: 0 0 6px var(--text-dim);
  }
  .segment.dim {
    color: var(--text-faint);
  }
  /* The §7.3 "faint slow breathing" while VAD detects speech — opacity
   * only, token colors untouched, nothing moves. */
  .segment.live {
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
  .pop-thumbs {
    display: flex;
    gap: 4px;
    align-items: center;
    padding: 6px;
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
</style>
