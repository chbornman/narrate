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
   */
  // Lucide Unplug = the offline-volume badge (same glyph as the Grid).
  import Unplug from "@lucide/svelte/icons/unplug";
  import { ui } from "../../state/app.svelte";
  import * as sel from "../../logic/selection";
  import { thumbUrl } from "../../ipc/urls";
  import { displayedHash } from "../../logic/looknav";
  import Panel from "../../primitives/Panel.svelte";

  // Render window: the current image roughly centered with STRIP_RADIUS
  // neighbors each side; the window width is DERIVED (2r + 1) so the
  // centering invariant cannot drift when the radius changes.
  const STRIP_RADIUS = 8;

  /** The surface's shared order, as displayed hashes (the grid's units
   * and Look's entries already agree on order by construction). */
  const order = $derived(
    ui.surface === "look"
      ? ui.look.order.map((e) => displayedHash(e, ui.look.flips))
      : ui.grid.unitHashes,
  );
  const index = $derived(ui.surface === "look" ? ui.look.index : ui.grid.sel.focus);
  const stripStart = $derived(Math.max(0, index - STRIP_RADIUS));
  const stripHashes = $derived(
    order.slice(stripStart, stripStart + STRIP_RADIUS * 2 + 1),
  );
  const gridByHash = $derived(new Map(ui.grid.items.map((i) => [i.hash, i])));

  function onPick(absIndex: number) {
    if (ui.surface === "look") {
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
  <div class="filmstrip">
    {#each stripHashes as shown, i (shown)}
      {@const item = gridByHash.get(shown)}
      <button
        class="strip-thumb"
        class:current={stripStart + i === index}
        onclick={() => onPick(stripStart + i)}
        aria-label="Show image"
      >
        <img src={thumbUrl(shown)} alt="" draggable="false" />
        {#if item?.hasJournal}<span class="journal-dot"></span>{/if}
        {#if item?.offline}<span class="offline-badge"><Unplug size={9} /></span>{/if}
      </button>
    {/each}
  </div>
</Panel>

<style>
  .filmstrip {
    height: 100%;
    display: flex;
    gap: 4px;
    align-items: stretch;
    padding: 6px 8px;
    overflow-x: auto;
  }
  .strip-thumb {
    position: relative;
    flex: 0 0 auto;
    /* Cells track the panel's dragged height (square, edge-to-edge). */
    aspect-ratio: 1;
    height: 100%;
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
