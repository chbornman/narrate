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
  let decisionFailure = $state<{
    consentCommitted: boolean;
    decision: "download" | "later" | "never";
    message: string;
  } | null>(null);
  let committing = $state(false);

  async function decide(decision: "download" | "later" | "never") {
    if (committing) return;
    committing = true;
    decisionFailure = null;
    try {
      const outcome = await ipc.runtimeConsent(decision);
      ui.shell.onRuntimeStatus(outcome.status);
      if (outcome.consentCommitted && outcome.operationRetryable) {
        decisionFailure = {
          consentCommitted: true,
          decision,
          message: outcome.operationError ?? "The model download did not start. You can retry.",
        };
      }
    } catch (error) {
      decisionFailure = {
        consentCommitted: false,
        decision,
        message:
          error instanceof Error ? error.message : "The consent decision could not be saved.",
      };
    } finally {
      committing = false;
    }
  }

  async function accept(modelId: string) {
    ui.shell.onRuntimeStatus(await ipc.runtimeAcceptLicense(modelId));
  }

  function retryDecisionFailure() {
    const failure = decisionFailure;
    if (failure !== null) void decide(failure.decision);
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
      <button disabled={!model.downloadReady || committing} onclick={() => void decide("download")}>
        Download now
      </button>
      <button class="quiet" disabled={committing} onclick={() => void decide("later")}>Later</button>
      <button class="quiet" disabled={committing} onclick={() => void decide("never")}>Never</button>
    </div>
    {#if decisionFailure !== null}
      <div class="decision-failure" role="alert">
        <p>Your model preference could not be saved.</p>
        <p class="dim">{decisionFailure.message}</p>
        <button
          class="quiet"
          disabled={committing}
          onclick={() => (decisionFailure = null)}>Dismiss</button
        >
      </div>
    {/if}
  </aside>
{/if}
{#if model === null && decisionFailure !== null}
  <aside class="consent operation-failure" aria-label="Model download">
    <p>
      {decisionFailure.consentCommitted
        ? "Your model preference was saved, but the download did not start."
        : "Your model preference could not be saved."}
    </p>
    <p class="dim">{decisionFailure.message}</p>
    <div class="actions">
      <button disabled={committing} onclick={retryDecisionFailure}>
        Retry
      </button>
      <button class="quiet" disabled={committing} onclick={() => (decisionFailure = null)}>
        Dismiss
      </button>
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

  .operation-failure {
    width: 360px;
  }

  .decision-failure {
    margin-top: 10px;
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
