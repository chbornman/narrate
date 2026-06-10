<script lang="ts">
  /**
   * The stack expand/collapse CONTROL (featureset §5 — a control, not a
   * badge): the one extra click zone a cell may carry (§8 guardrail keeps
   * the rest of the cell a single zone). Collapsed pairs show ⌄ (expand);
   * expanded members show ⌃ (re-collapse) — live, reversible, per pair —
   * next to the member count (dogfood round 1: the control shows the pair
   * count). The verb itself is the registry's stack-toggle-active row; the
   * tooltip resolves from it (the fourth rendering of the one table).
   */
  import { tooltip } from "../../primitives/tooltip";

  let {
    collapsed,
    count,
    onactivate,
  }: {
    collapsed: boolean;
    /** Members in the stack (D1: always 2 — RAW+JPEG pairs only). */
    count: number;
    onactivate: () => void;
  } = $props();
</script>

<button
  class="chevron"
  aria-label={collapsed ? `Expand stack of ${count}` : `Collapse stack of ${count}`}
  onclick={(e) => {
    e.stopPropagation(); // the cell click zone stays selection-only
    onactivate();
  }}
  ondblclick={(e) => e.stopPropagation()}
  {@attach tooltip({ actionId: "stack-toggle-active" })}
>
  <span class="count">{count}</span>{collapsed ? "⌄" : "⌃"}
</button>

<style>
  .chevron {
    position: absolute;
    left: 3px;
    top: 3px;
    min-width: 16px;
    height: 16px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 2px;
    padding: 0 3px;
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
  .count {
    font-size: 10px;
    color: var(--text-faint);
  }
  .chevron:hover .count {
    color: var(--text-dim);
  }
</style>
