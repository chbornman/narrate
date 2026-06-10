<script lang="ts">
  /**
   * The stack expand/collapse CONTROL (featureset §5 — a control, not a
   * badge): the one extra click zone a cell may carry (§8 guardrail keeps
   * the rest of the cell a single zone). Collapsed pairs show ⌄ (expand);
   * expanded members show ⌃ (re-collapse) — live, reversible, per pair.
   * The verb itself is the registry's stack-toggle-active row; the tooltip
   * resolves from it (the fourth rendering of the one table).
   */
  import { tooltip } from "../../primitives/tooltip";

  let {
    collapsed,
    onactivate,
  }: {
    collapsed: boolean;
    onactivate: () => void;
  } = $props();
</script>

<button
  class="chevron"
  aria-label={collapsed ? "Expand stack" : "Collapse stack"}
  onclick={(e) => {
    e.stopPropagation(); // the cell click zone stays selection-only
    onactivate();
  }}
  ondblclick={(e) => e.stopPropagation()}
  {@attach tooltip({ actionId: "stack-toggle-active" })}
>
  {collapsed ? "⌄" : "⌃"}
</button>

<style>
  .chevron {
    position: absolute;
    left: 3px;
    top: 3px;
    width: 16px;
    height: 16px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: none;
    border-radius: 2px;
    background: var(--bg-overlay);
    color: var(--text-dim);
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
  }
  .chevron:hover {
    color: var(--text);
  }
</style>
