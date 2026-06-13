<script lang="ts">
  /**
   * The one-time model-consent card (UI §9.1.3 / RUNTIME §10.2): a quiet
   * panel after the first root — never a modal gate; journaling continues
   * behind it untouched. Detected tier, ONE decision (Download now /
   * Later / Never), per-model license display with required acceptances,
   * and the live manifest disk budget. No fanfare on any path.
   */
  import * as ipc from "../../ipc/commands";
  import { ui } from "../../state/app.svelte";
  import { consentCard, formatGb } from "../../logic/consent";

  const model = $derived(consentCard(ui.shell.runtime, ui.roots.length > 0));

  async function decide(decision: "download" | "later" | "never") {
    ui.shell.onRuntimeStatus(await ipc.runtimeConsent(decision));
  }

  async function accept(modelId: string) {
    ui.shell.onRuntimeStatus(await ipc.runtimeAcceptLicense(modelId));
  }
</script>

{#if model !== null}
  <aside class="consent" aria-label="Local models">
    <p>
      This machine can run Photoproof's local voice &amp; search models
      ({formatGb(model.totalBytes)} download).
    </p>
    <p class="dim">
      Skipping changes nothing about journaling - typed notes, the pencil,
      ratings, and keyword search are fully functional without any models;
      voice capture and semantic search light up later if models are added
      (Settings → Models).
    </p>
    <ul>
      {#each model.models as m (m.id)}
        <li>
          <span class="name">{m.id}</span>
          <span class="size">{formatGb(m.totalBytes)}</span>
          <a href={m.licenseUrl} target="_blank" rel="noreferrer">{m.licenseName}</a>
          {#if m.acceptanceRequired}
            {#if m.accepted}
              <span class="dim">accepted</span>
            {:else}
              <button class="quiet" onclick={() => void accept(m.id)}>
                Accept license
              </button>
            {/if}
          {/if}
        </li>
      {/each}
    </ul>
    <div class="actions">
      <button disabled={!model.downloadReady} onclick={() => void decide("download")}>
        Download now
      </button>
      <button class="quiet" onclick={() => void decide("later")}>Later</button>
      <button class="quiet" onclick={() => void decide("never")}>Never</button>
    </div>
  </aside>
{/if}

<style>
  .consent {
    position: fixed;
    right: 16px;
    bottom: 48px;
    width: 380px;
    background: var(--bg-overlay);
    border: 1px solid var(--chrome);
    border-radius: 6px;
    padding: 14px 16px;
    z-index: 50;
    font-size: 12px;
    color: var(--text);
  }
  p {
    margin: 0 0 8px;
  }
  .dim {
    color: var(--text-faint);
  }
  ul {
    list-style: none;
    margin: 0 0 10px;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 0;
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .size {
    color: var(--text-faint);
  }
  a {
    color: var(--text-dim);
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .quiet {
    background: transparent;
    border-color: transparent;
    color: var(--text-dim);
  }
</style>
