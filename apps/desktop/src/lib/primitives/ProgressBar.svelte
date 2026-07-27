<script lang="ts">
  /**
   * One accessible determinate progress treatment for downloads and other
   * measurable work. The visible copy belongs to the caller; this primitive
   * owns bar geometry and theme color.
   */
  let {
    value,
    max,
    label,
    tone = "working",
  }: {
    value: number;
    max: number;
    label: string;
    tone?: "working" | "ready" | "warning";
  } = $props();
</script>

<progress aria-label={label} {max} {value} class="tone-{tone}"></progress>

<style>
  progress {
    display: block;
    width: 100%;
    height: 8px;
    accent-color: var(--status-working);
  }
  .tone-ready {
    accent-color: var(--status-ready);
  }
  .tone-warning {
    accent-color: var(--status-warning);
  }
</style>
