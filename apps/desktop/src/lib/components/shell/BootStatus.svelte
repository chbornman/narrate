<script lang="ts">
  import type { BootStatus } from "../../state/app.svelte";

  let {
    status,
    recoveryAction = null,
    onretry,
  }: {
    status: BootStatus;
    recoveryAction?: "reset-device-identity" | null;
    onretry: () => void;
  } = $props();

  const subsystemName: Record<string, string> = {
    bootstrap: "application data",
    events: "live updates",
    settings: "settings",
    roots: "library",
    "archived-roots": "archived folders",
    folder: "current folder",
    ingest: "indexing status",
    runtime: "models and hardware",
    collections: "collections",
    topics: "topics",
  };

  const failureLine = $derived(
    status.failures
      .map(({ subsystem }) => subsystemName[subsystem] ?? subsystem)
      .join(", "),
  );
  const failureDetails = $derived(
    status.failures.map(({ message }) => message).filter(Boolean).join(" "),
  );
  const requiresRelaunch = $derived(
    status.failures.some(({ subsystem }) => subsystem === "bootstrap"),
  );
</script>

{#if status.phase === "loading"}
  <section class="boot boot-loading" role="status" aria-live="polite">
    <p>Opening your library…</p>
  </section>
{:else if status.phase === "fatal"}
  <section class="boot boot-fatal" role="alert">
    <div>
      <h1>Photoproof could not open the library</h1>
      <p>
        Your files have not been changed. Check that the application data is
        available, then try again.
      </p>
      {#if failureLine !== ""}
        <p class="detail">Unavailable: {failureLine}.</p>
      {/if}
      {#if failureDetails !== ""}
        <p class="detail">{failureDetails}</p>
      {/if}
      <button type="button" onclick={onretry} disabled={status.retrying}>
        {status.retrying
          ? "Trying again…"
          : recoveryAction === "reset-device-identity"
            ? "Reset device identity and relaunch"
          : requiresRelaunch
            ? "Relaunch Photoproof"
            : "Try again"}
      </button>
    </div>
  </section>
{:else if status.phase === "degraded"}
  <aside class="degraded" role="status" aria-live="polite">
    <span>
      Photoproof is open, but {failureLine}
      {status.failures.length === 1 ? " is" : " are"} temporarily unavailable.
    </span>
    <button type="button" onclick={onretry} disabled={status.retrying}>
      {status.retrying ? "Trying again…" : "Retry"}
    </button>
  </aside>
{/if}

<style>
  .boot {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: grid;
    place-items: center;
    padding: 32px;
    color: var(--text, #e9e9e9);
    background: var(--background, #151515);
  }
  .boot-loading p {
    margin: 0;
    color: var(--text-dim, #a8a8a8);
  }
  .boot-fatal > div {
    width: min(460px, 100%);
  }
  h1 {
    margin: 0 0 12px;
    font-size: 20px;
    font-weight: 600;
  }
  p {
    line-height: 1.5;
  }
  .detail {
    color: var(--text-dim, #a8a8a8);
  }
  button {
    margin-top: 8px;
  }
  .degraded {
    position: fixed;
    z-index: 900;
    right: 16px;
    bottom: 16px;
    max-width: min(560px, calc(100vw - 32px));
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 10px 12px;
    border: 1px solid var(--line, #4c4c4c);
    border-radius: 6px;
    color: var(--text, #e9e9e9);
    background: var(--panel, #252525);
    box-shadow: 0 8px 24px rgb(0 0 0 / 24%);
  }
  .degraded span {
    flex: 1;
    line-height: 1.35;
  }
  .degraded button {
    flex: none;
    margin: 0;
  }
</style>
