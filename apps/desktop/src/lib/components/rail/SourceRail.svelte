<script lang="ts">
  /**
   * The left source rail (featureset §3): a PUSH Panel — resizable,
   * width-persisted, `\` toggles (D5). THREE PEER tabs (founder, June 2026;
   * Topics added June 13 2026): Folders, Collections, Topics. Collections are
   * the point (intent, gathered context); folders are just where files happen
   * to sit; topics are the machine's fuzzy lenses (saved phrases + cluster
   * suggestions). Folder rows render through SourceList over logic/sources.ts
   * sections; collection rows through CollectionList; topic rows through
   * TopicList (manual + suggested). Right-click on a folder row opens the
   * rail-folder seat. The footer carries the visible tab's one standing
   * affordance: "Add folder…", the inline "New collection…" input, or the
   * inline "Add topic…" input (same idiom across all three).
   */
  import { tick } from "svelte";
  import { ui } from "../../state/app.svelte";
  import { collectionKey } from "../../logic/sources";
  import type { CollectionRow, SourceRow } from "../../logic/sources";
  import Panel from "../../primitives/Panel.svelte";
  import SourceList from "./SourceList.svelte";
  import CollectionList from "./CollectionList.svelte";
  import CollectionNotes from "./CollectionNotes.svelte";
  import TopicList from "./TopicList.svelte";

  const secs = $derived(ui.railSections());
  const collectionRows = $derived(ui.railCollectionRows());
  const tab = $derived(ui.shell.railTab);

  function onOpen(row: SourceRow) {
    ui.shell.railFocusKey = row.key;
    ui.shell.railFocused = true;
    void ui.perform({ kind: "rail-folder-open", rootId: row.rootId, folder: row.folder });
  }

  function onContextMenu(row: SourceRow, x: number, y: number) {
    ui.shell.openContextMenu("rail-folder", { x, y }, {
      rootId: row.rootId,
      folder: row.folder,
    });
  }

  function onOpenCollection(row: CollectionRow) {
    ui.shell.railFocusKey = row.key;
    ui.shell.railFocused = true;
    void ui.perform({ kind: "collection-open", id: row.id });
  }

  // ---- inline create (the rail's footer affordance, collections tab) -----
  let creating = $state(false);
  let draft = $state("");
  let inputEl: HTMLInputElement | undefined = $state();

  // The thumb menu's "New collection…" arms a mint-and-add: shell stashes
  // the stack-expanded targets and flips us to this tab. We auto-open the
  // SAME input the footer button opens (one create UX, the founder's ask) —
  // committing then drops those targets into the fresh collection. Holding
  // the targets in a local snapshot keeps the commit honest even if the
  // stash is cleared mid-typing. null ⇒ a plain create (no add).
  let pendingTargets = $state<string[] | null>(null);

  // Watch the shell's stash: when it arms, mount + focus the input with an
  // empty draft and remember the targets to add on commit. effect (not a
  // derived) because opening the input is an imperative, one-shot action.
  $effect(() => {
    const pending = ui.shell.pendingNewCollection;
    if (pending !== null && !creating) {
      pendingTargets = pending;
      void beginCreate();
    }
  });

  async function beginCreate() {
    creating = true;
    draft = "";
    await tick(); // the input mounts on the flag flip
    inputEl?.focus();
  }

  /** Tear the input down and clear BOTH the local target snapshot and the
   * shell stash — every exit path (commit, Esc, blur) routes here so a
   * stale pending-add can never linger to poison the next plain create. */
  function endCreate() {
    creating = false;
    draft = "";
    pendingTargets = null;
    ui.shell.clearPendingNewCollection();
  }

  function onCreateKeydown(e: KeyboardEvent) {
    // The input owns its keys entirely: Enter commits, Esc cancels, and
    // nothing leaks to the global keymap (the WelcomeCard containment
    // pattern — without this, Esc would also peel an escape layer).
    e.stopPropagation();
    if (e.key === "Enter") {
      const name = draft;
      const targets = pendingTargets;
      endCreate();
      // Armed by the thumb menu ⇒ create-then-add in one step; otherwise
      // the rail's plain create. Both flow through ui.createCollection, so
      // there is exactly one create path.
      if (targets !== null) void ui.createCollectionAndAdd(targets, name);
      else void ui.createCollection(name);
    } else if (e.key === "Escape") {
      endCreate();
    }
  }

  // ---- inline add-topic (the rail's footer affordance, Topics tab) -------
  // The collection-creator idiom, text instead of a picker: a saved manual
  // topic is a phrase (DESIGN-TOPICS-COLLECTIONS.md). Kept separate from the
  // collection-create state so the two footers never alias.
  let addingTopic = $state(false);
  let topicDraft = $state("");
  let topicInputEl: HTMLInputElement | undefined = $state();

  async function beginAddTopic() {
    addingTopic = true;
    topicDraft = "";
    await tick();
    topicInputEl?.focus();
  }

  function endAddTopic() {
    addingTopic = false;
    topicDraft = "";
  }

  function onTopicKeydown(e: KeyboardEvent) {
    e.stopPropagation(); // the input owns its keys (the collection-creator pattern)
    if (e.key === "Enter") {
      const phrase = topicDraft;
      endAddTopic();
      void ui.addTopic(phrase); // a blank/duplicate phrase is a no-op in the store
    } else if (e.key === "Escape") {
      endAddTopic();
    }
  }
</script>

<!-- lights-out hides this through the root's panel snapshot, not a gate -->
<Panel id="rail" edge="left" open={ui.shell.railOpen} label="Sources">
  <div class="rail-body">
    <!-- three PEER tabs; the strip is the rail's one piece of standing chrome -->
    <div class="tabs" role="tablist" aria-label="Source kind">
      <button
        role="tab"
        aria-selected={tab === "folders"}
        class:current={tab === "folders"}
        onclick={() => ui.shell.setRailTab("folders")}
      >
        Folders
      </button>
      <button
        role="tab"
        aria-selected={tab === "collections"}
        class:current={tab === "collections"}
        onclick={() => ui.shell.setRailTab("collections")}
      >
        Collections
      </button>
      <button
        role="tab"
        aria-selected={tab === "topics"}
        class:current={tab === "topics"}
        onclick={() => ui.shell.setRailTab("topics")}
      >
        Topics
      </button>
    </div>

    <div class="rows">
      {#if tab === "folders"}
        <SourceList
          sections={secs}
          focusKey={ui.shell.railFocusKey}
          currentKey={ui.grid.rootId === null
            ? null
            : `folders:${ui.grid.rootId}:${ui.grid.folder}`}
          onopen={onOpen}
          oncontextmenu={onContextMenu}
        />
      {:else if tab === "collections"}
        <CollectionList
          rows={collectionRows}
          focusKey={ui.shell.railFocusKey}
          currentKey={ui.collectionId === null ? null : collectionKey(ui.collectionId)}
          onopen={onOpenCollection}
        />
      {:else}
        <TopicList />
      {/if}
    </div>

    <!-- the viewed collection's append-only notes (B71): sits below the
         rows on the Collections tab, a sibling of the inspector journal -->
    {#if tab === "collections"}
      <CollectionNotes />
    {/if}

    <!-- emphasized (a full bordered button against the quiet rows) but
         token-only; the folder verb is also seated on the rail-folder menu -->
    {#if tab === "folders"}
      <button class="footer-verb" onclick={() => void ui.perform({ kind: "add-root" })}>
        Add folder…
      </button>
    {:else if tab === "collections" && creating}
      <input
        bind:this={inputEl}
        bind:value={draft}
        class="footer-input"
        placeholder="Collection name"
        aria-label="New collection name"
        onkeydown={onCreateKeydown}
        onblur={endCreate}
      />
    {:else if tab === "collections"}
      <button class="footer-verb" onclick={() => void beginCreate()}>
        New collection…
      </button>
    {:else if addingTopic}
      <input
        bind:this={topicInputEl}
        bind:value={topicDraft}
        class="footer-input"
        placeholder="Topic phrase"
        aria-label="New topic phrase"
        onkeydown={onTopicKeydown}
        onblur={endAddTopic}
      />
    {:else}
      <button class="footer-verb" onclick={() => void beginAddTopic()}>
        Add topic…
      </button>
    {/if}
  </div>
</Panel>

<style>
  .rail-body {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .tabs {
    flex: 0 0 auto;
    display: flex;
    gap: 2px;
    padding: 6px 8px 0;
  }
  .tabs button {
    flex: 1;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    padding: 4px 6px;
    border-bottom: 1px solid transparent;
    border-radius: 0;
  }
  .tabs button:hover {
    color: var(--text-dim);
  }
  .tabs button.current {
    color: var(--text);
    border-bottom-color: var(--chrome-strong);
  }
  .rows {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .footer-verb {
    flex: 0 0 auto;
    margin: 8px;
    padding: 6px 10px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .footer-verb:hover {
    background: var(--bg-overlay);
  }
  .footer-input {
    flex: 0 0 auto;
    margin: 8px;
    padding: 6px 10px;
    background: var(--bg-raised);
    border: 1px solid var(--chrome-strong);
    border-radius: 4px;
    color: var(--text);
    font-size: 12px;
  }
</style>
