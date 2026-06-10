<script lang="ts">
  /**
   * THE one context-menu host: renders shell.contextMenu (any seat, and
   * the sort ▾ pseudo-seat) via Popover + Menu over the actions/menus.ts
   * model. Items carry Actions into ui.perform — menus mirror every
   * keyboard verb BY CONSTRUCTION (featureset §6). Esc closes via escape
   * layer 2; outside pointer dismisses via the Popover.
   */
  import { ui } from "../../state/app.svelte";
  import { menuModel } from "../../actions/menus";
  import Popover from "../../primitives/Popover.svelte";
  import Menu from "../../primitives/Menu.svelte";

  const open = $derived(ui.shell.contextMenu);
  const model = $derived(
    open === null ? null : menuModel(open.seat, ui.actionContext(), open.arg),
  );
</script>

{#if open !== null && model !== null && model.rows.length > 0}
  <Popover anchor={open.anchor} ondismiss={() => ui.shell.closeContextMenu()}>
    <Menu
      {model}
      onaction={(a) => void ui.perform(a)}
      onclose={() => ui.shell.closeContextMenu()}
    />
  </Popover>
{/if}
