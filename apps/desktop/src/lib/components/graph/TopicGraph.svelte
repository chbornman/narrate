<script lang="ts">
  /**
   * Semantic topic-graph lens (DESIGN-SEMANTIC-GRAPH.md, v1): a force-directed
   * map of the current scope. Topic ANCHOR nodes sit on a ring; image nodes are
   * pulled toward each topic by their blended (looks-vs-said) affinity, plus
   * mutual repulsion. The layout IS a semantic map.
   *
   * Affinities are computed ONCE per topic-set / alpha change (the backend
   * `topic_affinities` command) and fed into the pure force sim
   * (logic/forcegraph.ts); the rAF loop only re-runs the physics, never the
   * affinity scan (DESIGN: "NOT per-frame recompute of affinities"). Rendered to
   * CANVAS so it handles many nodes; the full-library scale spike runs the same
   * path unoptimized to feel the wall.
   *
   * Interactions (DESIGN — it is a navigation surface): click a topic anchor →
   * scope the grid to it (ui.scopeToTopic); click an image node → open it in
   * Look (ui.openFromGraph); drag nodes to explore. No em-dashes in any
   * user-visible copy (gate: check:emdash).
   */
  import { onMount, untrack } from "svelte";
  import { ui } from "../../state/app.svelte";
  import * as ipc from "../../ipc/commands";
  import {
    ringAnchors,
    seedNodes,
    step,
    type ForceConfig,
    type ImageNode,
    type TopicAnchor,
  } from "../../logic/forcegraph";

  // -- topic + scope state ----------------------------------------------------
  let topics = $state<string[]>([]);
  let topicInput = $state("");
  let suggestions = $state<ipc.TopicSuggestion[]>([]);
  let alpha = $state(0.5);
  /** Point the lens at the WHOLE library (the scale spike) vs the current grid
   * scope. The founder flips this to "feel the scale wall" (DESIGN §scale). */
  let fullLibrary = $state(false);

  // -- readiness / telemetry (surfaced, never hidden) -------------------------
  let loading = $state(false);
  let nodeCount = $state(0);
  let visualReady = $state(false);
  let annotationReady = $state(false);
  /** A visible note when the scope is large enough that the lens struggles (the
   * scale spike: do NOT silently cap; SAY where it falls over). */
  let scaleNote = $state<string | null>(null);

  // -- sim data ---------------------------------------------------------------
  let tuning = $state<ipc.GraphTuning | null>(null);
  let nodes: ImageNode[] = [];
  let anchors: TopicAnchor[] = [];
  /** hash -> per-topic affinity row, matching the topics array order. */
  let affinity = new Map<string, number[]>();

  // -- canvas -----------------------------------------------------------------
  let canvasEl: HTMLCanvasElement | null = $state(null);
  let width = $state(800);
  let height = $state(600);
  let raf = 0;
  // Pan/zoom view transform (sim-space -> screen). Centered on the canvas.
  let zoom = 1;
  let panX = 0;
  let panY = 0;

  // A node-count past which the live sim + N^2 repulsion visibly strain on a
  // typical machine. NOT a cap (we still run it); just the honesty threshold for
  // the scale-note banner (DESIGN wants the wall felt and named, at what N).
  const STRAIN_NODES = 1200;

  const scope = (): ipc.GraphScope =>
    fullLibrary ? { kind: "library" } : ui.graphScope();

  function forceConfig(): ForceConfig {
    const t = tuning;
    return {
      attraction: t?.attraction ?? 0.02,
      repulsion: t?.repulsion ?? 800,
      damping: t?.damping ?? 0.85,
      centering: t?.centering ?? 0.01,
      ringRadius: t?.ring_radius ?? 320,
    };
  }

  // -- affinity fetch + (re)seed ----------------------------------------------
  // Recomputed only when the topic SET, alpha, or scope changes (a topic-set or
  // alpha change is the DESIGN trigger; the rAF loop never calls this).
  async function recompute() {
    loading = true;
    scaleNote = null;
    const sc = scope();
    const t0 = performance.now();
    let report: ipc.AffinityReport;
    try {
      report = await ipc.topicAffinities(sc, topics, alpha);
    } catch {
      // A degraded/unreachable backend leaves an empty, honest layout rather
      // than throwing under the user (the graceful M1 posture).
      report = { images: [], visual_ready: false, annotation_ready: false };
    }
    const elapsed = Math.round(performance.now() - t0);
    visualReady = report.visual_ready;
    annotationReady = report.annotation_ready;
    nodeCount = report.images.length;

    affinity = new Map(report.images.map((i) => [i.image_hash, i.scores.map((s) => s.affinity)]));
    const hashes = report.images.map((i) => i.image_hash);
    anchors = ringAnchors(topics.length, forceConfig().ringRadius);
    nodes = seedNodes(hashes, affinity, topics.length);

    // Scale spike: SAY when it struggles, and at what N + scan cost (the wall,
    // named, not hidden). Mirrors the backend's logged telemetry.
    if (nodeCount >= STRAIN_NODES) {
      scaleNote = `${nodeCount.toLocaleString()} images in scope (affinity scan ${elapsed} ms). The live force layout runs unoptimized past ~${STRAIN_NODES.toLocaleString()} nodes and will feel heavy. This is the scale spike.`;
    }
    loading = false;
    restartLoop();
  }

  async function loadSuggestions() {
    try {
      suggestions = await ipc.suggestTopics(scope());
    } catch {
      suggestions = [];
    }
  }

  // -- topic mutations --------------------------------------------------------
  function addTopic(phrase: string) {
    const p = phrase.trim();
    if (p === "" || topics.includes(p)) return;
    topics = [...topics, p];
    void recompute();
  }
  function removeTopic(i: number) {
    topics = topics.filter((_, j) => j !== i);
    void recompute();
  }
  function onSubmitTopic(e: SubmitEvent) {
    e.preventDefault();
    addTopic(topicInput);
    topicInput = "";
  }

  // -- the rAF physics loop ---------------------------------------------------
  function restartLoop() {
    cancelAnimationFrame(raf);
    let cool = 0;
    const tick = () => {
      const energy = step(nodes, anchors, forceConfig());
      draw();
      // Stop ticking once the layout is at rest (saves battery); a fresh
      // recompute or a drag restarts it. A few settle frames guard against an
      // early-zero on the opening frame.
      if (energy < 1e-2 && ++cool > 30) {
        cancelAnimationFrame(raf);
        return;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
  }

  // -- rendering --------------------------------------------------------------
  function toScreen(x: number, y: number): [number, number] {
    return [width / 2 + panX + x * zoom, height / 2 + panY + y * zoom];
  }
  function fromScreen(sx: number, sy: number): [number, number] {
    return [(sx - width / 2 - panX) / zoom, (sy - height / 2 - panY) / zoom];
  }

  function draw() {
    const ctx = canvasEl?.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, width, height);
    // image nodes
    for (const n of nodes) {
      const [sx, sy] = toScreen(n.x, n.y);
      ctx.beginPath();
      ctx.arc(sx, sy, n.fixed === true ? 5 : 3.2, 0, Math.PI * 2);
      ctx.fillStyle = n.fixed === true ? "#d6d6d6" : "#8a8a8a";
      ctx.fill();
    }
    // topic anchors (drawn on top, with labels)
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.font = "12px system-ui, sans-serif";
    for (const a of anchors) {
      const [sx, sy] = toScreen(a.x, a.y);
      ctx.beginPath();
      ctx.arc(sx, sy, 8, 0, Math.PI * 2);
      ctx.fillStyle = "#161616";
      ctx.strokeStyle = "#cfcfcf";
      ctx.lineWidth = 2;
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = "#d6d6d6";
      ctx.fillText(topics[a.topic] ?? "", sx, sy - 16);
    }
  }

  // -- pointer interaction ----------------------------------------------------
  let dragging: ImageNode | null = null;
  const HIT_R = 10; // screen-space pick radius

  function pickNode(sx: number, sy: number): ImageNode | null {
    let best: ImageNode | null = null;
    let bestD = HIT_R * HIT_R;
    for (const n of nodes) {
      const [px, py] = toScreen(n.x, n.y);
      const d = (px - sx) * (px - sx) + (py - sy) * (py - sy);
      if (d < bestD) {
        bestD = d;
        best = n;
      }
    }
    return best;
  }
  function pickAnchor(sx: number, sy: number): TopicAnchor | null {
    for (const a of anchors) {
      const [px, py] = toScreen(a.x, a.y);
      if ((px - sx) ** 2 + (py - sy) ** 2 < 14 * 14) return a;
    }
    return null;
  }

  function localXY(e: PointerEvent): [number, number] {
    const r = canvasEl!.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  }

  function onPointerDown(e: PointerEvent) {
    const [sx, sy] = localXY(e);
    const anchor = pickAnchor(sx, sy);
    if (anchor) {
      // Click a topic anchor -> scope the grid to that topic.
      void ui.scopeToTopic(topics[anchor.topic]);
      return;
    }
    const node = pickNode(sx, sy);
    if (node) {
      dragging = node;
      node.fixed = true;
      canvasEl?.setPointerCapture(e.pointerId);
      restartLoop();
    }
  }
  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    const [sx, sy] = localXY(e);
    const [x, y] = fromScreen(sx, sy);
    dragging.x = x;
    dragging.y = y;
  }
  let downAt = 0;
  function onPointerUp(e: PointerEvent) {
    if (!dragging) return;
    const node = dragging;
    node.fixed = false;
    dragging = null;
    canvasEl?.releasePointerCapture(e.pointerId);
    // A quick press-release with little movement is a CLICK -> open in Look;
    // a drag just releases the node back into the physics.
    if (performance.now() - downAt < 250) void ui.openFromGraph(node.hash);
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
    zoom = Math.max(0.2, Math.min(5, zoom * factor));
    draw();
  }

  // -- lifecycle --------------------------------------------------------------
  onMount(() => {
    void (async () => {
      try {
        tuning = await ipc.graphTuning();
        alpha = tuning.alpha_default;
      } catch {
        /* defaults stand (forceConfig falls back) */
      }
      await loadSuggestions();
      await recompute();
    })();
    return () => cancelAnimationFrame(raf);
  });

  // Re-blend live when the alpha slider moves (DESIGN: "re-blend affinities
  // live"). untrack the body so only `alpha` retriggers it.
  $effect(() => {
    void alpha;
    untrack(() => {
      if (tuning !== null) void recompute();
    });
  });

  // Flipping the full-library scale spike re-fetches against the new scope.
  $effect(() => {
    void fullLibrary;
    untrack(() => {
      if (tuning !== null) {
        void loadSuggestions();
        void recompute();
      }
    });
  });

  // Keep the canvas backing store sized to its box.
  $effect(() => {
    if (canvasEl) {
      canvasEl.width = width;
      canvasEl.height = height;
      draw();
    }
  });
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape") ui.closeGraph();
  }}
/>

<div class="graph-lens" role="dialog" aria-label="Topic graph">
  <header class="controls">
    <div class="left">
      <strong>Topic graph</strong>
      <span class="scope-name">{fullLibrary ? "whole library" : ui.folderName}</span>
    </div>

    <form class="add-topic" onsubmit={onSubmitTopic}>
      <input
        bind:value={topicInput}
        placeholder="Add a topic"
        aria-label="Add a topic"
        spellcheck="false"
        autocomplete="off"
      />
      <button type="submit">Add</button>
    </form>

    <label class="alpha">
      <span>said</span>
      <input
        type="range"
        min="0"
        max="1"
        step="0.05"
        bind:value={alpha}
        aria-label="Looks versus said blend"
      />
      <span>looks</span>
    </label>

    <label class="full-lib">
      <input type="checkbox" bind:checked={fullLibrary} />
      whole library
    </label>

    <button class="close" onclick={() => ui.closeGraph()} aria-label="Close topic graph">
      Close
    </button>
  </header>

  <!-- active topic chips (removable) -->
  {#if topics.length > 0}
    <div class="active-topics">
      {#each topics as topic, i (topic)}
        <span class="chip active">
          {topic}
          <button onclick={() => removeTopic(i)} aria-label={`Remove topic ${topic}`}>x</button>
        </span>
      {/each}
    </div>
  {/if}

  <!-- suggestion chip rail: click to add as an anchor -->
  {#if suggestions.length > 0}
    <div class="rail" aria-label="Suggested topics">
      {#each suggestions as s (s.phrase)}
        {#if !topics.includes(s.phrase)}
          <button
            class="chip suggest"
            class:from-collection={s.source === "collection"}
            onclick={() => addTopic(s.phrase)}
            title={`${s.source === "collection" ? "collection" : "frequent note phrase"} (${s.count})`}
          >
            {s.phrase}
          </button>
        {/if}
      {/each}
    </div>
  {/if}

  <!-- readiness honesty + scale-spike note -->
  <div class="status">
    {#if loading}
      <span>computing affinities...</span>
    {:else}
      <span>{nodeCount.toLocaleString()} images</span>
      {#if topics.length === 0}
        <span class="dim">add a topic to pull the images apart</span>
      {:else}
        <span class="dim">
          {visualReady ? "visual ready" : "visual idle"} ·
          {annotationReady ? "annotation ready" : "annotation idle"}
        </span>
      {/if}
    {/if}
    {#if scaleNote !== null}
      <span class="scale-note">{scaleNote}</span>
    {/if}
  </div>

  <div
    class="canvas-wrap"
    bind:clientWidth={width}
    bind:clientHeight={height}
  >
    <canvas
      bind:this={canvasEl}
      onpointerdown={(e) => {
        downAt = performance.now();
        onPointerDown(e);
      }}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
      onwheel={onWheel}
    ></canvas>
  </div>
</div>

<style>
  .graph-lens {
    position: absolute;
    inset: 0;
    z-index: 40;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--text);
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--chrome);
    flex-wrap: wrap;
  }
  .controls .left {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .scope-name {
    color: var(--text-dim);
    font-size: 12px;
  }
  .add-topic {
    display: flex;
    gap: 4px;
  }
  .add-topic input {
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    color: var(--text);
    padding: 4px 8px;
    border-radius: 4px;
  }
  .alpha {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-dim);
    font-size: 12px;
  }
  .full-lib {
    display: flex;
    align-items: center;
    gap: 4px;
    color: var(--text-dim);
    font-size: 12px;
  }
  button {
    background: var(--bg-raised);
    border: 1px solid var(--chrome);
    color: var(--text);
    padding: 4px 8px;
    border-radius: 4px;
    cursor: pointer;
  }
  button:hover {
    border-color: var(--chrome-strong);
  }
  .close {
    margin-left: auto;
  }
  .active-topics,
  .rail {
    display: flex;
    gap: 6px;
    padding: 6px 12px;
    flex-wrap: wrap;
  }
  .rail {
    border-bottom: 1px solid var(--chrome);
  }
  .chip {
    font-size: 12px;
    padding: 3px 8px;
    border-radius: 999px;
    border: 1px solid var(--chrome);
    background: var(--bg-raised);
    color: var(--text-dim);
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .chip.active {
    color: var(--text);
    border-color: var(--chrome-strong);
  }
  .chip.active button {
    all: unset;
    cursor: pointer;
    color: var(--text-faint);
    padding: 0 2px;
  }
  .chip.suggest {
    cursor: pointer;
  }
  .chip.from-collection {
    border-style: dashed;
  }
  .status {
    display: flex;
    gap: 12px;
    align-items: center;
    padding: 4px 12px;
    font-size: 12px;
    color: var(--text-dim);
    flex-wrap: wrap;
  }
  .status .dim {
    color: var(--text-faint);
  }
  .scale-note {
    color: var(--red-pencil);
  }
  .canvas-wrap {
    flex: 1;
    position: relative;
    overflow: hidden;
  }
  canvas {
    position: absolute;
    inset: 0;
    touch-action: none;
  }
</style>
