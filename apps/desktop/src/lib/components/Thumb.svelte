<script lang="ts">
  /**
   * One grid cell. Exactly two badges may appear (UI §3.5): the has-journal
   * dot (dulled red, bottom-right) and the offline-volume badge (top-right).
   * Explicitly forbidden: rating stars, labels, filename/EXIF overlays.
   * Selection = thin light border + slight lift; no checkmarks, no counts.
   *
   * The <img> elements are recycled by the virtualizer (keyed by pool slot,
   * not item) — this component only swaps `src`.
   */
  import { thumbUrl } from "../ipc/urls";

  let {
    hash,
    hasJournal,
    offline,
    selected,
    focused,
    size,
    onpointerselect,
  }: {
    hash: string;
    hasJournal: boolean;
    offline: boolean;
    selected: boolean;
    focused: boolean;
    size: number;
    onpointerselect: (e: MouseEvent) => void;
  } = $props();
</script>

<div
  class="thumb"
  class:selected
  class:focused
  style:width="{size}px"
  style:height="{size}px"
  onclick={onpointerselect}
  onkeydown={() => {
    /* keyboard selection is global (UI §11); cells are not tab stops */
  }}
  role="gridcell"
  tabindex="-1"
>
  <img src={thumbUrl(hash)} alt="" draggable="false" loading="eager" decoding="async" />
  {#if hasJournal}<span class="journal-dot"></span>{/if}
  {#if offline}<span class="offline-badge">⏏</span>{/if}
</div>

<style>
  .thumb {
    position: relative;
    background: var(--bg-raised); /* neutral placeholder, no layout shift */
    border: 1px solid transparent;
    border-radius: 2px;
    overflow: hidden;
    cursor: default;
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }
  .thumb.selected {
    border-color: var(--focus);
    transform: translateY(-1px);
  }
  .thumb.focused {
    outline: 1px solid var(--text-dim);
    outline-offset: 1px;
  }
  .journal-dot {
    position: absolute;
    right: 6px;
    bottom: 6px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--red-dull);
  }
  .offline-badge {
    position: absolute;
    right: 5px;
    top: 3px;
    color: var(--text-dim);
    font-size: 11px;
  }
</style>
