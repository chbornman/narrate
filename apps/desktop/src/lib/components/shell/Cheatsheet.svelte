<script lang="ts">
  /**
   * `?` / F1 keyboard-map overlay (featureset §6): the full key table as a
   * dismissable Sheet — no tour, no coach marks (I5 stands). Rows come
   * from actions/cheatsheet.ts over the registry; rows unavailable in the
   * current context render DIMMED, never hidden.
   */
  import { ui } from "../../state/app.svelte";
  import { cheatsheet } from "../../actions/cheatsheet";
  import Sheet from "../../primitives/Sheet.svelte";
  import KeyHint from "../../primitives/KeyHint.svelte";

  const groups = $derived(cheatsheet(ui.actionContext()));
</script>

<Sheet
  open={ui.shell.cheatsheetOpen}
  onclose={() => (ui.shell.cheatsheetOpen = false)}
  label="Keyboard map"
>
  <div class="sheet-body">
    {#each groups as group (group.group)}
      <section>
        <h2>{group.label}</h2>
        {#each group.rows as row (row.scope + ":" + row.id)}
          <div class="row" class:dimmed={!row.available}>
            <span class="verb">{row.label}</span>
            <span class="keys">
              {#each row.keys as chord, i (i)}
                <KeyHint {chord} />
              {:else}
                <span class="pointer">right-click</span>
              {/each}
            </span>
          </div>
        {/each}
      </section>
    {/each}
  </div>
</Sheet>

<style>
  .sheet-body {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 4px 28px;
    min-width: min(640px, 80vw);
  }
  h2 {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin: 10px 0 4px;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 2px 0;
  }
  .row.dimmed {
    opacity: 0.4; /* unavailable here — shown anyway, never hidden */
  }
  .verb {
    color: var(--text);
  }
  .keys {
    margin-left: auto;
    display: flex;
    gap: 4px;
  }
  .pointer {
    color: var(--text-faint);
    font-size: 11px;
  }
</style>
