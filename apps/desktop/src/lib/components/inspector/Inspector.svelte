<script lang="ts">
  /**
   * The right inspector (featureset §3, D2) — STAGE C OWNS inspector/*
   * from parallel kickoff. Panel(right) + its own small inline tab strip
   * (I = Metadata, J = Journal; M5 adds Partner as one more tab — the
   * right edge is RESERVED for per-image truth). No Tabs primitive: one
   * consumer. Push, resizable, width persists; openness does not
   * (DECISIONS 3). Esc closes it from Grid (escape layer 12); leaving
   * Look keeps it open on the still-active image (founder, June 2026).
   *
   * Stage C: tabs filled (MetadataTab/JournalTab), journal flows wired,
   * key hints on the tab strip (KeyHint — the one component that may
   * render key names).
   */
  import { ui } from "../../state/app.svelte";
  import { INSPECTOR_WIDTH_KEY } from "../../state/prefs";
  import Panel from "../../primitives/Panel.svelte";
  import KeyHint from "../../primitives/KeyHint.svelte";
  import MetadataTab from "./MetadataTab.svelte";
  import JournalTab from "./JournalTab.svelte";
  import RedactionModal from "./RedactionModal.svelte";
</script>

<Panel
  side="right"
  open={ui.inspector.open !== false && !ui.shell.chromeHidden}
  size={320}
  minSize={260}
  maxSize={520}
  persistKey={INSPECTOR_WIDTH_KEY}
  label="Inspector"
>
  <div class="tabs" role="tablist">
    <button
      role="tab"
      aria-selected={ui.inspector.open === "metadata"}
      class:active={ui.inspector.open === "metadata"}
      onclick={() => void ui.perform({ kind: "open-inspector", tab: "metadata" })}
    >
      Metadata <KeyHint chord={{ key: "i" }} />
    </button>
    <button
      role="tab"
      aria-selected={ui.inspector.open === "journal"}
      class:active={ui.inspector.open === "journal"}
      onclick={() => void ui.perform({ kind: "open-inspector", tab: "journal" })}
    >
      Journal <KeyHint chord={{ key: "j" }} />
    </button>
    <!-- M5: Partner joins this strip -->
  </div>
  {#if ui.inspector.open === "metadata"}
    <MetadataTab />
  {:else if ui.inspector.open === "journal"}
    <JournalTab />
  {/if}
</Panel>

<RedactionModal />

<style>
  .tabs {
    display: flex;
    gap: 2px;
    padding: 6px 8px 0;
    border-bottom: 1px solid var(--chrome);
  }
  .tabs button {
    border: none;
    background: transparent;
    color: var(--text-faint);
    padding: 4px 8px;
    border-radius: 3px 3px 0 0;
  }
  .tabs button.active {
    color: var(--text);
    background: var(--bg-raised);
  }
</style>
