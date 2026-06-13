<script lang="ts">
  /**
   * The filmstrip (UI §4.1, `F`, default hidden) — a BOTTOM edge Panel of
   * the CENTER column, shared by Grid and Look (founder, June 12 2026: F
   * is total, and the strip spans the canvas width by construction — it
   * sits between rail and inspector and resizes as they toggle). One
   * order source per surface: in Look it walks the navigation order,
   * stack-aware (each cell shows the entry's DISPLAYED member, so an R
   * flip is visible — featureset §5); in Grid it walks the SAME units the
   * grid renders (display member already folded into unit.primary) with
   * the focused cell highlighted. Shows the same two badges as the Grid,
   * nothing more. Obeys Tab through the lights-out snapshot (the root
   * closes panels on hide). A push panel, never an overlay.
   *
   * The strip scrolls the WHOLE order, virtually (logic/filmstrip.ts):
   * spacers stand in for the unrendered runs so the scrollbar describes
   * the full set, and the window re-derives from scroll/size facts.
   *
   * Centering contract (founder, June 12 2026): the selected photo sits
   * horizontally centered, re-applied on geometry changes (panel drag,
   * sidebar toggles, window resize) — EXCEPT while the user has scrolled
   * away manually; a manual scroll holds until the next selection change
   * (arrows or mouse), which snaps back to center and clears it.
   */
  // Lucide Unplug = the offline-volume badge (same glyph as the Grid).
  import Unplug from "@lucide/svelte/icons/unplug";
  import { untrack } from "svelte";
  import { ui } from "../../state/app.svelte";
  import * as sel from "../../logic/selection";
  import { thumbUrl } from "../../ipc/urls";
  import { displayedHash } from "../../logic/looknav";
  import { centerOn, stripWindow } from "../../logic/filmstrip";
  import Panel from "../../primitives/Panel.svelte";

  const GAP = 4;
  // .filmstrip vertical padding (6 + 6) — cells are square at the
  // remaining inner height, so the pitch tracks the panel drag height.
  const VPAD = 12;

  /** The surface's shared order, as displayed hashes (the grid's units
   * and Look's entries already agree on order by construction). */
  const order = $derived(
    ui.viewMode === "look"
      ? ui.look.order.map((e) => displayedHash(e, ui.look.flips))
      : ui.grid.unitHashes,
  );
  const index = $derived(ui.viewMode === "look" ? ui.look.index : ui.grid.sel.focus);
  const gridByHash = $derived(new Map(ui.grid.items.map((i) => [i.hash, i])));

  // Scroll/size facts feeding the pure window math.
  let stripEl: HTMLDivElement | undefined = $state();
  let stripW = $state(0);
  let stripH = $state(0);
  let scrollLeft = $state(0);
  const cell = $derived(Math.max(1, stripH - VPAD));
  const pitch = $derived(cell + GAP);
  const win = $derived(stripWindow(scrollLeft, stripW, pitch, order.length));

  // The centering contract's moving parts. Plain fields, not $state:
  // nothing renders from them — they steer the effects below.
  let userOverride = false; // a manual scroll holds until the next selection
  let programmatic = false; // our own centering echo in onscroll, not the user

  function center() {
    if (stripEl === undefined || index < 0) return;
    const target = centerOn(index, stripW, pitch, order.length);
    // No-op assignments fire no scroll event — only flag a real move, or
    // the stale flag would swallow the user's NEXT genuine scroll.
    if (Math.abs(stripEl.scrollLeft - target) >= 1) {
      programmatic = true;
      stripEl.scrollLeft = target;
    }
  }

  // Selection moved (arrows, mouse, Look nav): snap back to center and
  // clear any manual-scroll override. Geometry reads are untracked so
  // ONLY an index change re-runs this arm.
  $effect(() => {
    if (index < 0) return;
    userOverride = false;
    untrack(center);
  });

  // Geometry changed (panel drag, sidebar toggle, window resize): keep
  // the selected cell centered — unless the user scrolled away on
  // purpose; their position is theirs until the next selection.
  $effect(() => {
    void stripW;
    void pitch;
    void order.length;
    if (!userOverride) untrack(center);
  });

  function onScroll() {
    scrollLeft = stripEl?.scrollLeft ?? 0;
    if (programmatic) {
      programmatic = false;
    } else {
      userOverride = true;
    }
  }

  function onPick(absIndex: number) {
    if (ui.viewMode === "look") {
      ui.look.index = absIndex;
      void ui.reportScope();
    } else {
      // The same selection verb a grid thumb click performs: a single
      // click makes the cell active (focus ≡ active, featureset §1).
      void ui.applySelection(sel.click(ui.grid.sel, ui.grid.unitHashes, absIndex));
    }
  }
</script>

<Panel id="filmstrip" edge="bottom" open={ui.look.filmstrip} label="Filmstrip">
  <div
    class="filmstrip"
    bind:this={stripEl}
    bind:clientWidth={stripW}
    bind:clientHeight={stripH}
    onscroll={onScroll}
  >
    <div class="spacer" style:width="{win.leadPx}px"></div>
    {#each order.slice(win.first, win.first + win.count) as shown, i (win.first + i)}
      {@const absIndex = win.first + i}
      {@const item = gridByHash.get(shown)}
      <button
        class="strip-thumb"
        style:width="{cell}px"
        class:current={absIndex === index}
        onclick={() => onPick(absIndex)}
        aria-label="Show image"
      >
        <img src={thumbUrl(shown)} alt="" draggable="false" loading="lazy" />
        {#if item?.hasJournal}<span class="journal-dot"></span>{/if}
        {#if item?.offline}<span class="offline-badge"><Unplug size={9} /></span>{/if}
      </button>
    {/each}
    <div class="spacer" style:width="{win.tailPx}px"></div>
  </div>
</Panel>

<style>
  .filmstrip {
    height: 100%;
    display: flex;
    align-items: stretch;
    padding: 6px 8px;
    overflow-x: auto;
    overflow-y: hidden;
  }
  .spacer {
    flex: 0 0 auto;
  }
  .strip-thumb {
    position: relative;
    flex: 0 0 auto;
    /* Square at the panel's dragged height; the GAP lives in the margin
     * so spacer math is one pitch (flex `gap` would also pad the
     * spacers and break the virtual offsets). */
    height: 100%;
    margin-right: 4px;
    padding: 0;
    border: 1px solid transparent;
    background: var(--bg-raised);
    border-radius: 2px;
    overflow: hidden;
  }
  .strip-thumb.current {
    border-color: var(--selection);
  }
  .strip-thumb img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }
  .journal-dot {
    position: absolute;
    right: 4px;
    bottom: 4px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--journal-dot);
  }
  .offline-badge {
    position: absolute;
    right: 3px;
    top: 1px;
    color: var(--text-dim);
    display: flex; /* size the badge box to the svg, no baseline gap */
  }
</style>
