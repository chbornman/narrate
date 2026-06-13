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
  import * as prefs from "../../state/prefs";
  import {
    computeTopicFields,
    topicHue,
    type FieldNode,
    type TopicFields,
  } from "../../logic/topicfield";
  import {
    aggregateToSuperNodes,
    coolHeat,
    expandSuperNode,
    REHEAT_START,
    ringAnchors,
    screenToSim,
    seedNodes,
    shouldUseLod,
    simToScreen,
    step,
    type ForceConfig,
    type ImageNode,
    type TopicAnchor,
    type ViewTransform,
  } from "../../logic/forcegraph";
  import {
    engagedTopics,
    nodeOverlay,
    overlookedTopics,
    type OverlayMode,
    type TopicAttention,
  } from "../../logic/synthesis";
  import { GraphThumbCache, nodeBaseSizePx } from "../../logic/graphthumbs";
  import {
    countAboveThreshold,
    DEFAULT_BAKE_THRESHOLD,
    selectedAboveThreshold,
    type ScoredImage,
  } from "../../logic/topicbake";

  // -- topic + scope state ----------------------------------------------------
  let topics = $state<string[]>([]);
  let topicInput = $state("");
  let suggestions = $state<ipc.TopicSuggestion[]>([]);
  /** v2 note-grounded auto-topics (k-means cluster labels) for the rail. These
   * sit ABOVE the cheap n-gram/collection suggestions: smarter, "connected to
   * notes". */
  let clusterSuggestions = $state<ipc.ClusterTopic[]>([]);
  /** v3 SEAM: LLM-suggested theme phrases. Stays empty until the Gemma connector
   * is wired (the backend returns "unavailable" meanwhile), so the rail is
   * hidden and the cluster + n-gram suggestions stand in. */
  let llmSuggestions = $state<string[]>([]);
  let alpha = $state(0.5);
  /** Point the lens at the WHOLE library (the scale spike) vs the current grid
   * scope. The founder flips this to "feel the scale wall" (DESIGN §scale). */
  let fullLibrary = $state(false);

  // -- readiness / telemetry (surfaced, never hidden) -------------------------
  let loading = $state(false);
  let nodeCount = $state(0);
  let visualReady = $state(false);
  let annotationReady = $state(false);
  /** A visible note about the layout: past the LOD threshold it reads "LOD
   * active (showing N clusters of M images)"; below it is null (full detail). */
  let scaleNote = $state<string | null>(null);
  /** True when the lens is running in LOD mode (super-nodes), so the banner +
   * interaction (click-to-expand) switch on. */
  let lodActive = $state(false);

  // -- sim data ---------------------------------------------------------------
  let tuning = $state<ipc.GraphTuning | null>(null);
  let nodes: ImageNode[] = [];
  let anchors: TopicAnchor[] = [];
  /** hash -> per-topic affinity row, matching the topics array order. The FULL
   * detail set (every image), kept even in LOD mode so a super-node can expand
   * back into its members with their real per-image affinity. */
  let affinity = new Map<string, number[]>();
  /** The total image count behind the current layout (M), even when the sim runs
   * over far fewer super-nodes (N = nodeCount). The status reports both. */
  let imageTotal = $state(0);

  // -- attention overlay (heatmap x graph synthesis) --------------------------
  // The three-state Attention control (Off / Engaged / Overlooked) overlays the
  // attention FIELD (per-image intensity from the heatmap) onto the semantic
  // STRUCTURE (this graph). The persisted mode lives on the ui store; the
  // intensity fetch (REUSING the heatmap `image_intensity` command) and the pure
  // synthesis math (logic/synthesis.ts) live here so TopicGraph stays the thin
  // renderer. The draw() loop reads `intensity` + the per-mode mapping per node.
  const overlay = (): OverlayMode => ui.graphAttention;
  /** hash -> normalized [0,1] engagement intensity for the in-scope set, fetched
   * once per scope/mode/all-time change (NOT per frame). Empty until fetched or
   * when the overlay is off. */
  let intensity = $state<Map<string, number>>(new Map());
  /** The ranked readout the header lists: "Engaged: <topic> ..." or
   * "Overlooked: <topic> ...". Recomputed when the overlay, intensity, or layout
   * changes. */
  let attentionRanked = $state<TopicAttention[]>([]);
  /** Per-topic overlooked-ness (normalized [0,1], indexed by topic) so a node can
   * inherit its dominant topic's overlooked score for a cluster-level glow that
   * agrees with the readout. Empty outside Overlooked mode. */
  let overlookedByTopic: number[] = [];

  // -- the slider-to-collection bake (the signature gesture) ------------------
  // Clicking a topic anchor SELECTS it for baking: a threshold slider (0..1)
  // appears, the images with blended affinity >= threshold GLOW in the graph
  // (the "nearby set lighting up"), a live count updates, and "Make a
  // collection" commits that threshold selection into an evented collection
  // (ui.bakeTopicCollection -> createCollectionFromTopic; the ONE-WAY bake,
  // provenance recorded by the backend). The threshold -> selected-set math is
  // the shared pure logic/topicbake module, so the glow set here and the Topics
  // tab's count and the bake membership agree exactly.
  let selectedTopic = $state<number | null>(null);
  let bakeThreshold = $state(DEFAULT_BAKE_THRESHOLD);
  let bakeName = $state("");
  let baking = $state(false);

  /** Every in-scope image's blended affinity to the SELECTED topic (the column
   * of the affinity matrix for that anchor), as ScoredImage. The threshold math
   * runs over this so the glow + count + bake all key off the same numbers the
   * graph already computed (DESIGN: "reuse the affinity data the graph has"). */
  let bakeScored = $derived<ScoredImage[]>(
    selectedTopic === null
      ? []
      : [...affinity.entries()].map(([hash, row]) => ({
          hash,
          score: row[selectedTopic!] ?? 0,
        })),
  );
  /** The hashes glowing right now (affinity >= threshold for the selected
   * topic). A Set for O(1) per-node lookup in the draw loop. */
  let glowSet = $derived(
    new Set(selectedAboveThreshold(bakeScored, bakeThreshold)),
  );
  /** The live "N photos" count the bake panel shows. */
  let glowCount = $derived(countAboveThreshold(bakeScored, bakeThreshold));

  /** Select a topic anchor for baking: open the slider panel, default the
   * threshold + the collection name (to the phrase), and repaint so the glow
   * appears. A re-click on the same anchor closes it (toggle). */
  function selectTopicForBake(topicIndex: number) {
    if (selectedTopic === topicIndex) {
      selectedTopic = null;
    } else {
      selectedTopic = topicIndex;
      bakeThreshold = DEFAULT_BAKE_THRESHOLD;
      bakeName = topics[topicIndex] ?? "";
    }
    draw();
  }

  /** Commit the current threshold selection into a collection (the bake). Uses
   * the SAME ui.bakeTopicCollection the Topics tab uses, so the graph and the
   * tab share one bake path + provenance. On success, surface a station pop and
   * close the panel (the new collection appears in the rail via the snapshot
   * refresh). */
  async function commitBake() {
    if (selectedTopic === null || baking) return;
    const phrase = topics[selectedTopic];
    baking = true;
    try {
      const created = await ui.bakeTopicCollection(phrase, bakeThreshold, bakeName);
      if (created !== null) {
        ui.shell.stationPop(`Collection - ${created.name}`);
        selectedTopic = null;
        draw();
      }
    } finally {
      baking = false;
    }
  }

  // A topic removed while selected drops the bake panel (the index would dangle).
  $effect(() => {
    if (selectedTopic !== null && selectedTopic >= topics.length) {
      selectedTopic = null;
    }
  });

  // -- canvas -----------------------------------------------------------------
  let canvasEl: HTMLCanvasElement | null = $state(null);
  let width = $state(800);
  let height = $state(600);
  let raf = 0;

  // -- per-topic INFLUENCE FIELD (founder, June 2026) -------------------------
  // A soft colored glow behind the nodes showing each topic's "power level"
  // WHERE its strong images actually landed. Non-arbitrary by construction: we
  // splat each node's REAL per-topic affinity into a low-res scalar buffer at
  // the node's CURRENT (physics-placed) sim position, accumulate + blur (the
  // pure logic/topicfield module). Because positions emerge from ALL topics'
  // pulls, the field reflects the cross-pull reality and fades with distance.
  // Each topic gets a distinct HUE; the per-topic fields COMPOSITE (lighten on
  // dark chrome / multiply on light) so overlapping topics BLEND their hues into
  // the "bridge" zones. It is a SEPARATE layer from the Attention overlay (that
  // is attention; this is topic-affinity power) and the two coexist.
  //
  // Performance: a low-res grid (96x96) recomputed only on SETTLE / throttled,
  // never per frame, rendered to a CACHED texture canvas; the field-layer draw
  // is one scaled drawImage per topic in sim-space (transformed like the nodes).
  let fieldEl: HTMLCanvasElement | null = $state(null);
  /** The influence-field layer toggle. Persisted via prefs like the other graph
   * toggles (no app.svelte.ts edit needed). Default OFF so the plain graph stays
   * the first read and the founder opts INTO the painted view. */
  let fieldOn = $state(prefs.loadGraphField());
  /** The most recent computed per-topic fields (cached between recomputes), and
   * a cached texture canvas PER topic so the per-frame draw is just placing the
   * textures into the view transform, never re-running splat/blur. */
  let topicFields: TopicFields | null = null;
  let fieldTextures: HTMLCanvasElement[] = [];
  /** Throttle handle so a flurry of settle/recompute calls coalesces into one
   * field recompute (it is the expensive part), not one per frame. */
  let fieldTimer: ReturnType<typeof setTimeout> | null = null;
  const FIELD_GRID = 96;
  /** Recompute throttle: long enough to skip the busy settling frames, short
   * enough that the field appears promptly once the layout calms. */
  const FIELD_THROTTLE_MS = 150;
  // Pan/zoom view transform (sim-space -> screen). Centered on the canvas.
  let zoom = 1;
  let panX = 0;
  let panY = 0;
  // Reheat energy (founder dogfood): a topic-set / alpha change boosts this
  // above 1 so the layout SETTLES fast; the rAF loop cools it back toward 1 each
  // frame. forceConfig() feeds it into the sim as the attraction multiplier.
  let heat = 1;

  // -- thumbnail cache --------------------------------------------------------
  // Each image node draws as its TINY preview thumbnail instead of a plain dot
  // (founder: "the node graph should show tiny previews as the markers"). The
  // cache owns the lazy-load + bounded-concurrency (logic/graphthumbs.ts): the
  // draw loop requests the thumbs nearest the viewport first and draws the cached
  // image when ready; until then the node's placeholder DOT stands in (no layout
  // jump). A completed load notifies us to repaint OFF the render path (it does
  // not run mid-frame), so cache hits keep per-frame drawImage cheap. A LOD
  // super-node loads ONE representative member's thumb (not all members), so the
  // load budget stays bounded at scale.
  let thumbReadyTick = $state(0);
  const thumbs = new GraphThumbCache(() => {
    // A thumb finished loading: bump a tick so a settled (non-ticking) sim still
    // repaints to swap the placeholder for the image, and request a frame.
    thumbReadyTick++;
    requestAnimationFrame(() => draw());
  });

  /** The hash whose thumbnail REPRESENTS a node on the canvas: a single image is
   * itself; a LOD super-node shows its highest-affinity member (its strongest
   * exemplar — the most on-topic image of the cluster), falling back to the first
   * member, so a cluster still reads as a real photo, not an abstract blob. */
  function repHash(n: ImageNode): string {
    if (n.members === undefined || n.members.length === 0) return n.hash;
    let best = n.members[0];
    let bestAff = -Infinity;
    for (const h of n.members) {
      const row = affinity.get(h);
      // Peak per-image affinity = how strongly this image holds to its dominant
      // topic; the strongest exemplar best represents the cluster.
      const peak = row ? Math.max(...row) : 0;
      if (peak > bestAff) {
        bestAff = peak;
        best = h;
      }
    }
    return best;
  }

  // The LOD threshold comes from GraphTuning (graph.lod_threshold, default
  // 1500). Past it the lens AGGREGATES images into super-nodes so the live sim +
  // O(N^2) repulsion stay within the budget the v1 scale spike measured, instead
  // of running over every image and straining. A fallback const covers the
  // pre-tuning-load window.
  const LOD_THRESHOLD_FALLBACK = 1500;
  const lodThreshold = (): number => tuning?.lod_threshold ?? LOD_THRESHOLD_FALLBACK;

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
      // Force-placed anchors (founder dogfood): the anchors gain their own pull
      // toward their images' centroid + mutual repulsion, both tunable knobs.
      anchorAttraction: t?.anchor_attraction ?? 0.08,
      anchorRepulsion: t?.anchor_repulsion ?? 60000,
      anchorDamping: t?.anchor_damping ?? 0.8,
      // The live reheat multiplier on the attraction terms; cooled each frame in
      // the rAF loop back toward the 1.0 steady state.
      heat,
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

    affinity = new Map(report.images.map((i) => [i.image_hash, i.scores.map((s) => s.affinity)]));
    const hashes = report.images.map((i) => i.image_hash);
    imageTotal = hashes.length;
    anchors = ringAnchors(topics.length, forceConfig().ringRadius);

    // LOD (DESIGN v2): past the threshold, AGGREGATE images into super-nodes so
    // the live sim + O(N^2) repulsion stay within the budget the v1 scale spike
    // measured, instead of running over every image. Below the threshold the v1
    // full-detail layout runs unchanged.
    const fullNodes = seedNodes(hashes, affinity, topics.length);
    lodActive = shouldUseLod(imageTotal, lodThreshold());
    if (lodActive) {
      nodes = aggregateToSuperNodes(fullNodes, topics.length);
      // Banner now reports the LOD state, not a "scale spike" warning: it is
      // handled, not just named. Keeps the telemetry (N clusters of M images +
      // scan time), mirroring the backend log.
      scaleNote = `LOD active (showing ${nodes.length.toLocaleString()} clusters of ${imageTotal.toLocaleString()} images, affinity scan ${elapsed} ms). Click a cluster to expand it.`;
    } else {
      nodes = fullNodes;
      scaleNote = null;
    }
    nodeCount = nodes.length;

    // A fresh node set (scope/topic/alpha change): drop stale PENDING thumb
    // requests so the new layout's nearest nodes load first. Already-loaded
    // thumbs stay cached (content-addressed by hash), so a return to a prior
    // scope is instant.
    thumbs.clearPending();

    loading = false;
    // The overlay reads per-image intensity; a fresh affinity set means a fresh
    // in-scope hash set, so re-fetch + re-rank against it.
    void refreshOverlay();
    // A fresh topic-set / alpha / scope change REHEATS the sim so the new layout
    // snaps into place in about a second instead of oozing in (founder dogfood).
    reheat();
    restartLoop();
  }

  /** Boost the sim energy so the layout SETTLES fast then cools (founder
   * dogfood): a topic added/removed, the blend slider moved, or a fresh scope
   * re-seeds the heat high. The rAF loop cools it back toward 1.0 each frame. */
  function reheat() {
    heat = REHEAT_START;
  }

  /** Toggle the influence-field layer + persist it (prefs, like the other graph
   * toggles). Turning it ON triggers an immediate recompute from the current
   * positions; turning it OFF clears the cached textures and the layer. */
  function toggleField() {
    fieldOn = !fieldOn;
    prefs.saveGraphField(fieldOn);
    if (fieldOn) {
      recomputeField();
    } else {
      topicFields = null;
      fieldTextures = [];
      if (fieldTimer !== null) {
        clearTimeout(fieldTimer);
        fieldTimer = null;
      }
      drawField();
    }
  }

  // -- attention overlay: intensity fetch + synthesis -------------------------
  /** Monotone token so a slow intensity fetch cannot overwrite a newer scope's
   * (mirrors the heatmap's heatLoad guard). */
  let overlayLoad = 0;

  /** Fetch per-image intensity for the in-scope set and recompute the ranked
   * readout + per-node mapping. REUSES the heatmap `image_intensity` command (no
   * second intensity definition) over the FULL detail hashes (so a super-node's
   * members resolve), honoring the heatmap's existing All-time concept via the
   * shared ui.heatAllTime flag. Off mode clears the overlay. */
  async function refreshOverlay() {
    if (overlay() === "off") {
      intensity = new Map();
      attentionRanked = [];
      overlookedByTopic = [];
      draw();
      return;
    }
    // The full detail hashes (every image), so a super-node's members resolve to
    // their own intensity even while the sim runs over the aggregates.
    const hashes = [...affinity.keys()];
    const load = ++overlayLoad;
    if (hashes.length === 0) {
      intensity = new Map();
    } else {
      try {
        const scores = await ipc.imageIntensity(hashes, ui.heatAllTime);
        if (load !== overlayLoad) return; // a newer scope won
        intensity = new Map(scores.map((s) => [s.hash, s.intensity]));
      } catch {
        // A degraded/unreachable backend leaves the overlay dark rather than
        // throwing under the user (the graceful posture, like recompute()).
        intensity = new Map();
      }
    }
    recomputeRanking();
    draw();
  }

  /** Recompute the ranked readout + the per-topic overlooked scores from the
   * current nodes + intensity (pure synthesis math). Runs over the FULL detail
   * nodes so the aggregation is over every image, not just the visible
   * super-nodes. */
  function recomputeRanking() {
    const mode = overlay();
    if (mode === "off") {
      attentionRanked = [];
      overlookedByTopic = [];
      return;
    }
    // Aggregate over every image (the full detail set), so the readout reflects
    // the whole scope even in LOD mode. A single-image node per hash with its
    // real affinity is exactly what the synthesis math wants.
    const detail: ImageNode[] = [...affinity.entries()].map(([hash, aff]) => ({
      hash,
      x: 0,
      y: 0,
      vx: 0,
      vy: 0,
      affinity: aff,
    }));
    if (mode === "engaged") {
      attentionRanked = engagedTopics(detail, intensity, topics.length);
      overlookedByTopic = [];
    } else {
      const ranked = overlookedTopics(detail, intensity, topics.length);
      attentionRanked = ranked;
      // Index the overlooked score by topic so a node inherits its cluster's glow.
      const byTopic = new Array(topics.length).fill(0);
      for (const r of ranked) byTopic[r.topic] = r.score;
      overlookedByTopic = byTopic;
    }
  }

  async function loadSuggestions() {
    const sc = scope();
    // Cheap candidates (note n-grams + collection names) + v2 cluster auto-labels
    // in parallel. Each degrades to empty on its own so a slow/absent half never
    // blocks the rail.
    const [cheap, clusters, llm] = await Promise.all([
      ipc.suggestTopics(sc).catch(() => [] as ipc.TopicSuggestion[]),
      ipc.clusterTopics(sc).catch(() => [] as ipc.ClusterTopic[]),
      ipc
        .suggestTopicsLlm(sc)
        .catch(() => ({ state: "unavailable", reason: "" }) as ipc.LlmSuggestions),
    ]);
    suggestions = cheap;
    clusterSuggestions = clusters;
    // v3 seam: only surface LLM themes when the connector is REAL (state ready).
    // Until then the rail stays empty and the cluster + n-gram rails stand in.
    llmSuggestions = llm.state === "ready" ? llm.topics : [];
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
      // Cool the reheat one frame toward the 1.0 steady state (pure helper, so
      // the curve is unit-testable). While heat is still elevated we keep
      // ticking even if the energy momentarily dips, so a reheat fully plays out.
      heat = coolHeat(heat);
      draw();
      // Stop ticking once the layout is at rest AND the reheat has fully cooled
      // (saves battery); a fresh recompute or a drag restarts it. A few settle
      // frames guard against an early-zero on the opening frame.
      if (energy < 1e-2 && heat <= 1.0001 && ++cool > 30) {
        cancelAnimationFrame(raf);
        // The layout has SETTLED: recompute the influence field from the final
        // node positions (throttled, so a quick re-settle coalesces). This is
        // the cheap, non-per-frame recompute point the design calls for.
        scheduleFieldRecompute();
        return;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
  }

  // -- rendering --------------------------------------------------------------
  // The view transform (sim-space ↔ screen) is the pure forcegraph helper, so
  // the pan/zoom math has one unit-tested source of truth.
  const view = (): ViewTransform => ({ width, height, zoom, panX, panY });
  function toScreen(x: number, y: number): [number, number] {
    return simToScreen(x, y, view());
  }
  function fromScreen(sx: number, sy: number): [number, number] {
    return screenToSim(sx, sy, view());
  }

  /** Read the canvas colors from the theme TOKENS (getComputedStyle), so the
   * graph honors the chrome theme (a light mode just landed) instead of hardcoded
   * hex. Cached per draw; the few token reads are cheap and the theme rarely
   * changes mid-frame. The overlay GLOW uses --red-pencil, the one saturated
   * accent (DECISIONS I5) reused as the attention highlight. */
  interface CanvasColors {
    superFill: string;
    dragFill: string;
    nodeFill: string;
    anchorFill: string;
    stroke: string;
    text: string;
    glow: string;
  }
  function canvasColors(): CanvasColors {
    const cs = canvasEl ? getComputedStyle(canvasEl) : null;
    const tok = (name: string, fallback: string): string => {
      const v = cs?.getPropertyValue(name).trim();
      return v !== undefined && v !== "" ? v : fallback;
    };
    return {
      superFill: tok("--chrome", "#2a2a2a"),
      dragFill: tok("--text", "#d6d6d6"),
      nodeFill: tok("--text-dim", "#8a8a8a"),
      anchorFill: tok("--bg-raised", "#161616"),
      stroke: tok("--focus", "#cfcfcf"),
      text: tok("--text", "#d6d6d6"),
      glow: tok("--red-pencil", "#e03131"),
    };
  }

  /** Trace a rounded-rectangle path (the thumbnail clip + ring shape). Kept here
   * (not Path2D round-rect) so it works across the webview without a polyfill. */
  function roundedRectPath(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    w: number,
    h: number,
    r: number,
  ) {
    const rad = Math.min(r, w / 2, h / 2);
    ctx.moveTo(x + rad, y);
    ctx.arcTo(x + w, y, x + w, y + h, rad);
    ctx.arcTo(x + w, y + h, x, y + h, rad);
    ctx.arcTo(x, y + h, x, y, rad);
    ctx.arcTo(x, y, x + w, y, rad);
    ctx.closePath();
  }

  /** Two offset rounded squares BEHIND a super-node's thumbnail, so a cluster
   * reads as a pile of images (a subtle stacked-card look). */
  function drawCardBack(
    ctx: CanvasRenderingContext2D,
    sx: number,
    sy: number,
    half: number,
    fill: string,
    stroke: string,
  ) {
    for (const off of [6, 3]) {
      ctx.beginPath();
      roundedRectPath(
        ctx,
        sx - half + off,
        sy - half - off,
        half * 2,
        half * 2,
        Math.min(8, half),
      );
      ctx.fillStyle = fill;
      ctx.fill();
      ctx.strokeStyle = stroke;
      ctx.lineWidth = 1;
      ctx.stroke();
    }
  }

  /** A small member-count pill at a super-node's top-right corner. */
  function drawCountBadge(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    count: number,
    c: CanvasColors,
  ) {
    ctx.save();
    const label = count.toLocaleString();
    const padX = 4;
    ctx.font = "10px system-ui, sans-serif";
    const w = ctx.measureText(label).width + padX * 2;
    const h = 14;
    ctx.globalAlpha = 1;
    ctx.beginPath();
    roundedRectPath(ctx, x - w, y, w, h, 7);
    ctx.fillStyle = c.anchorFill;
    ctx.fill();
    ctx.strokeStyle = c.stroke;
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.fillStyle = c.text;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(label, x - w / 2, y + h / 2);
    ctx.restore();
  }

  // -- influence field: recompute (throttled, on settle) + render ------------

  /** Is the chrome theme DARK? We read the canvas background luminance from the
   * theme tokens so the field compositing is theme-aware: on dark chrome we
   * LIGHTEN (the hues glow out of the dark), on light chrome we MULTIPLY (the
   * hues tint down into the light without washing out). One source: the same
   * --bg token the rest of the app themes from. */
  function isDarkTheme(): boolean {
    const cs = canvasEl ? getComputedStyle(canvasEl) : null;
    const bg = cs?.getPropertyValue("--bg").trim();
    return luminanceOf(bg) < 0.5;
  }

  /** Rough relative luminance of a #rgb/#rrggbb token, 0..1; unknown ⇒ dark. */
  function luminanceOf(hex: string | undefined): number {
    if (hex === undefined || hex === "") return 0;
    const m = hex.replace("#", "");
    const full =
      m.length === 3
        ? m
            .split("")
            .map((c) => c + c)
            .join("")
        : m;
    if (full.length < 6) return 0;
    const r = parseInt(full.slice(0, 2), 16) / 255;
    const g = parseInt(full.slice(2, 4), 16) / 255;
    const b = parseInt(full.slice(4, 6), 16) / 255;
    return 0.2126 * r + 0.7152 * g + 0.4126 * b;
  }

  /** HSL (h in degrees, s/l in [0,1]) -> [r,g,b] bytes, for the per-topic field
   * hue. Pure helper so the texture bake stays a tight loop (no per-cell CSS
   * color parsing). */
  function hslToRgb(h: number, s: number, l: number): [number, number, number] {
    const c = (1 - Math.abs(2 * l - 1)) * s;
    const hp = (((h % 360) + 360) % 360) / 60;
    const x = c * (1 - Math.abs((hp % 2) - 1));
    let r = 0;
    let g = 0;
    let b = 0;
    if (hp < 1) [r, g, b] = [c, x, 0];
    else if (hp < 2) [r, g, b] = [x, c, 0];
    else if (hp < 3) [r, g, b] = [0, c, x];
    else if (hp < 4) [r, g, b] = [0, x, c];
    else if (hp < 5) [r, g, b] = [x, 0, c];
    else [r, g, b] = [c, 0, x];
    const m = l - c / 2;
    return [
      Math.round((r + m) * 255),
      Math.round((g + m) * 255),
      Math.round((b + m) * 255),
    ];
  }

  /** Schedule a throttled field recompute. Called when the layout SETTLES, when
   * the field toggles on, and on a fresh node set. Coalesces a burst of calls
   * into one recompute so the splat/blur runs once per settle, not per frame. */
  function scheduleFieldRecompute() {
    if (!fieldOn) return;
    if (fieldTimer !== null) clearTimeout(fieldTimer);
    fieldTimer = setTimeout(() => {
      fieldTimer = null;
      recomputeField();
    }, FIELD_THROTTLE_MS);
  }

  /** Recompute the per-topic fields from the CURRENT node positions + real
   * affinities (the pure module), then bake each topic's scalar field into a
   * cached hue texture canvas. Runs the FULL detail set so a super-node's
   * members each splat their own affinity at the cluster's position (the field
   * reads the whole scope even in LOD mode). Off / no-topics ⇒ clear. */
  function recomputeField() {
    if (!fieldOn || topics.length === 0 || nodes.length === 0) {
      topicFields = null;
      fieldTextures = [];
      drawField();
      return;
    }
    // Splat from the LIVE node positions: a super-node carries its members'
    // mean affinity at where the physics placed it, so the field still reflects
    // where the cluster's strength landed (cheap, no per-member fan-out here).
    const fieldNodes: FieldNode[] = nodes.map((n) => ({
      x: n.x,
      y: n.y,
      affinity: n.affinity,
    }));
    topicFields = computeTopicFields(fieldNodes, topics.length, {
      grid: FIELD_GRID,
    });
    bakeFieldTextures();
    drawField();
  }

  /** Bake each topic's low-res scalar field into a small hue texture canvas
   * (one per topic), so the per-frame draw is just placing the cached textures
   * into the view transform. Alpha is the per-topic-normalized field value times
   * a gentle ceiling, so a topic's glow reads as its power level without
   * drowning the nodes. */
  function bakeFieldTextures() {
    const tf = topicFields;
    if (tf === null) {
      fieldTextures = [];
      return;
    }
    const textures: HTMLCanvasElement[] = [];
    // The translucent glow ceiling: the strongest field cell paints at this
    // alpha, fading to 0 at the field edges. Soft on purpose (it sits UNDER the
    // nodes and must not fight them).
    const ALPHA_CEIL = 0.5;
    for (let t = 0; t < tf.topicCount; t++) {
      const tex = document.createElement("canvas");
      tex.width = tf.width;
      tex.height = tf.height;
      const tctx = tex.getContext("2d");
      if (tctx === null) {
        textures.push(tex);
        continue;
      }
      const peak = tf.max[t];
      const img = tctx.createImageData(tf.width, tf.height);
      const [r, g, b] = hslToRgb(topicHue(t), 0.7, 0.55);
      const field = tf.fields[t];
      for (let i = 0; i < field.length; i++) {
        // Per-topic normalized intensity in [0,1] -> alpha; the hue is constant
        // per topic so overlapping topics blend by their composited alphas.
        const a = peak > 0 ? (field[i] / peak) * ALPHA_CEIL : 0;
        const p = i * 4;
        img.data[p] = r;
        img.data[p + 1] = g;
        img.data[p + 2] = b;
        img.data[p + 3] = Math.round(a * 255);
      }
      tctx.putImageData(img, 0, 0);
      textures.push(tex);
    }
    fieldTextures = textures;
  }

  /** Draw the cached field textures into the field-layer canvas, transformed to
   * sim-space (panned + zoomed exactly like the nodes), compositing the topics
   * so overlapping fields BLEND their hues. Cheap: one scaled drawImage per
   * topic from the cached texture, no splat/blur on this path. */
  function drawField() {
    const ctx = fieldEl?.getContext("2d");
    if (ctx === null || ctx === undefined) return;
    ctx.clearRect(0, 0, width, height);
    const tf = topicFields;
    if (!fieldOn || tf === null || fieldTextures.length === 0) return;
    // The field's sim-space rect mapped to screen, so it tracks pan/zoom with
    // the nodes (the field IS in sim-space).
    const [x0, y0] = toScreen(tf.bounds.minX, tf.bounds.minY);
    const [x1, y1] = toScreen(tf.bounds.maxX, tf.bounds.maxY);
    const dw = x1 - x0;
    const dh = y1 - y0;
    ctx.save();
    // Composite the per-topic hues: LIGHTEN on dark chrome (hues glow out of the
    // dark), MULTIPLY on light chrome (hues tint down without washing white).
    ctx.globalCompositeOperation = isDarkTheme() ? "lighten" : "multiply";
    // Smooth the low-res texture as it scales up into a soft continuous glow.
    ctx.imageSmoothingEnabled = true;
    for (const tex of fieldTextures) {
      ctx.drawImage(tex, x0, y0, dw, dh);
    }
    ctx.restore();
  }

  function draw() {
    // A finished thumb load bumps thumbReadyTick; reference it so a settled sim's
    // reactive redraw path observes new thumbnails (read is intentional).
    void thumbReadyTick;
    // The influence-field layer (bottom): cheap cached-texture redraw so the
    // field tracks pan/zoom + the live layout every frame; the EXPENSIVE
    // splat/blur only re-runs on settle (scheduleFieldRecompute), never here.
    drawField();
    const ctx = canvasEl?.getContext("2d");
    if (!ctx) return;
    const c = canvasColors();
    const mode = overlay();
    const overlayOn = mode !== "off";
    // The bake selection takes visual priority over the attention overlay: when
    // a topic anchor is selected, the above-threshold images GLOW and the rest
    // recede (the signature "nearby set lighting up"). Computed once per frame.
    const bakeActive = selectedTopic !== null;
    ctx.clearRect(0, 0, width, height);
    // image nodes. Each draws as its TINY preview thumbnail (founder: "tiny
    // previews as the markers"); until the thumb loads, the old colored DOT is
    // the placeholder (no layout jump on swap). A LOD super-node draws its
    // representative member's thumb with a stacked-card backing + a member-count
    // badge, so a cluster still reads as images sized by how many it stands for.
    // The Attention overlay COMPOSITES onto the thumbnail: a hotter node is a
    // bigger thumb with a stronger colored halo/ring; Engaged glows where
    // attention lives, Overlooked glows coherent-but-cold, the rest recedes.
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.font = "11px system-ui, sans-serif";
    // The viewport center in sim-space, so thumb loads are requested
    // NEAREST-FIRST (the visible region fills before off-screen nodes).
    const [cx, cy] = fromScreen(width / 2, height / 2);
    for (const n of nodes) {
      const [sx, sy] = toScreen(n.x, n.y);
      const isSuper = n.members !== undefined;
      // Base drawn SIDE (px) from the pure mapping; the overlay scales it, and
      // its half is the hit radius — one source of truth for draw + pick.
      const baseSide = nodeBaseSizePx({
        isSuper,
        memberCount: isSuper ? n.members!.length : 0,
        isDragged: n.fixed === true,
      });
      // The per-node overlay mapping (glow + size). Off mode is a no-op
      // (glow 0, sizeScale 1) so the plain graph sizing is unchanged.
      const ov = overlayOn
        ? nodeOverlay(n, intensity, mode, overlookedByTopic)
        : { glow: 0, sizeScale: 1, intensity: 0 };
      // Bake glow: when a topic anchor is selected, a node above the threshold
      // glows full and the rest recede. A super-node glows if ANY member is in
      // the set (its representative thumbnail still stands for the cluster). The
      // bake glow OVERRIDES the attention overlay glow while a topic is selected.
      const inGlow = bakeActive
        ? n.members !== undefined
          ? n.members.some((h) => glowSet.has(h))
          : glowSet.has(n.hash)
        : false;
      const effGlow = bakeActive ? (inGlow ? 1 : 0) : ov.glow;
      const side = baseSide * ov.sizeScale;
      const half = side / 2;

      // Request this node's representative thumbnail, prioritized by squared
      // distance to the viewport center (nearest loads first). A super-node only
      // requests ITS ONE rep, never all members, so the budget stays bounded.
      const rh = repHash(n);
      const dx = n.x - cx;
      const dy = n.y - cy;
      thumbs.request(rh, dx * dx + dy * dy);
      const img = thumbs.get(rh);

      // A node is "lit" when it carries glow (attention overlay weight, or the
      // bake's above-threshold membership) and "dimmed" when an overlay/bake is
      // active but this node is not lit. The plain (no overlay, no bake) node is
      // fully opaque. `effGlow` already folds bake-priority over the overlay.
      const dimActive = overlayOn || bakeActive;
      const bodyAlpha =
        effGlow > 0 ? 0.6 + 0.4 * effGlow : dimActive ? 0.45 : 1;

      ctx.save();
      // The GLOW becomes a soft colored HALO behind the thumbnail, scaled by
      // glow weight (a hotter / above-threshold node halos stronger).
      if (effGlow > 0) {
        ctx.shadowColor = c.glow;
        ctx.shadowBlur = 6 + 16 * effGlow;
      }
      ctx.globalAlpha = bodyAlpha;

      // A LOD super-node gets a subtle STACKED-CARD backing (two offset
      // rounded squares) so it reads as a pile of images, not a single one.
      if (isSuper) {
        drawCardBack(ctx, sx, sy, half, c.superFill, c.stroke);
      }

      // The thumbnail, clipped to a rounded square centered on the node. Until
      // it loads, the placeholder DOT (the old marker) fills the same box.
      ctx.beginPath();
      roundedRectPath(ctx, sx - half, sy - half, side, side, Math.min(8, half));
      if (img !== null) {
        ctx.save();
        ctx.clip();
        // Cover-fit: scale the (square-ish) thumb to fill the box, center-cropped.
        ctx.drawImage(img, sx - half, sy - half, side, side);
        ctx.restore();
      } else {
        // Placeholder: the prior dot, tinted by glow/super/drag state.
        ctx.fillStyle =
          effGlow > 0
            ? c.glow
            : isSuper
              ? c.superFill
              : n.fixed === true
                ? c.dragFill
                : c.nodeFill;
        ctx.fill();
      }

      // The affinity/selection BORDER RING on the thumbnail. A glowing node
      // (attention or bake) rings in the accent (agreeing with the halo);
      // otherwise a super-node / dragged node rings to read as distinct, a plain
      // node gets a hairline so the thumbnail has a crisp edge in both themes.
      ctx.shadowBlur = 0;
      ctx.globalAlpha = Math.min(1, bodyAlpha + 0.2);
      ctx.beginPath();
      roundedRectPath(ctx, sx - half, sy - half, side, side, Math.min(8, half));
      ctx.strokeStyle =
        effGlow > 0
          ? c.glow
          : isSuper || n.fixed === true
            ? c.stroke
            : c.nodeFill;
      ctx.lineWidth = isSuper || effGlow > 0 ? 2 : 1;
      ctx.stroke();
      ctx.restore();

      // The member-count badge on a super-node, so the aggregation stays legible
      // even as a thumbnail (DESIGN: "a super-node's size reflects its count").
      if (isSuper) {
        drawCountBadge(ctx, sx + half - 2, sy - half + 2, n.members!.length, c);
      }
    }
    // topic anchors (drawn on top, with labels). In an overlay mode an anchor
    // glows by its ranked attention score, so the readout and the ring agree.
    for (const a of anchors) {
      const [sx, sy] = toScreen(a.x, a.y);
      const score = overlayOn ? (anchorScore.get(a.topic) ?? 0) : 0;
      if (overlayOn && score > 0) {
        ctx.save();
        ctx.shadowColor = c.glow;
        ctx.shadowBlur = 4 + 14 * score;
      }
      // The anchor SELECTED for baking reads as distinct: a larger accent ring
      // (it is the topic whose threshold is lighting the graph up).
      const isBakeSel = selectedTopic === a.topic;
      ctx.beginPath();
      ctx.arc(sx, sy, isBakeSel ? 10 : 8, 0, Math.PI * 2);
      ctx.fillStyle = c.anchorFill;
      ctx.strokeStyle = isBakeSel || (overlayOn && score > 0) ? c.glow : c.stroke;
      ctx.lineWidth = isBakeSel ? 3 : 2;
      ctx.fill();
      ctx.stroke();
      if (overlayOn && score > 0) ctx.restore();
      // A PINNED anchor (user-placed) reads as distinct: a small filled inner
      // dot, so the founder can see which topics they have fixed in place.
      if (a.pinned === true) {
        ctx.beginPath();
        ctx.arc(sx, sy, 3, 0, Math.PI * 2);
        ctx.fillStyle = c.text;
        ctx.fill();
      }
      ctx.fillStyle = c.text;
      ctx.fillText(topics[a.topic] ?? "", sx, sy - 16);
    }
  }

  /** topic -> its current ranked score (Engaged total or Overlooked-ness),
   * indexed for the anchor glow. Derived from the readout so the ring and the
   * "Engaged: ... / Overlooked: ..." list agree. */
  let anchorScore = $derived(
    new Map(attentionRanked.map((r) => [r.topic, r.score])),
  );

  /** The ranked readout, trimmed to the topics that actually carry a non-zero
   * score (strongest first), capped so a long topic list stays readable. */
  const READOUT_MAX = 6;
  let rankedNonzero = $derived(
    attentionRanked.filter((r) => r.score > 1e-6).slice(0, READOUT_MAX),
  );

  // -- pointer interaction ----------------------------------------------------
  // A pointer-drag is one of THREE gestures, decided at pointer-down by the hit
  // test (founder dogfood): on a NODE it drags that node; on a topic ANCHOR it
  // drags (and on release PINS) that anchor; on EMPTY canvas it PANS the view.
  let dragging: ImageNode | null = null;
  let draggingAnchor: TopicAnchor | null = null;
  /** Background pan in progress: the screen point and the pan offset at the
   * moment of pointer-down, so move() can apply the raw delta to panX/panY. */
  let panning: { startX: number; startY: number; panX0: number; panY0: number } | null = null;
  /** Whether the current drag actually MOVED (past a small threshold). A
   * press-release that did not move is a CLICK (scope-to-topic / open Look);
   * a move is a drag (pin the anchor / reposition the node). */
  let moved = false;

  /** A node's drawn half-side in SCREEN px (its hit radius): the same base-size
   * mapping the draw loop uses, times the overlay size scale, times zoom — so
   * the pickable area matches exactly what is drawn, thumbnail and all. */
  function nodeHitHalf(n: ImageNode): number {
    const isSuper = n.members !== undefined;
    const base = nodeBaseSizePx({
      isSuper,
      memberCount: isSuper ? n.members!.length : 0,
      isDragged: n.fixed === true,
    });
    const ov =
      overlay() !== "off"
        ? nodeOverlay(n, intensity, overlay(), overlookedByTopic)
        : { sizeScale: 1 };
    // Drawn side is in screen px already (the box is not scaled by zoom in the
    // draw loop); use half-side as the square's pick radius.
    return (base * ov.sizeScale) / 2;
  }

  function pickNode(sx: number, sy: number): ImageNode | null {
    // Topmost-drawn wins on overlap: iterate in reverse draw order and take the
    // first node whose drawn box contains the point (square hit-test matching the
    // rounded-square thumbnail).
    for (let i = nodes.length - 1; i >= 0; i--) {
      const n = nodes[i];
      const [px, py] = toScreen(n.x, n.y);
      const half = nodeHitHalf(n);
      if (Math.abs(px - sx) <= half && Math.abs(py - sy) <= half) return n;
    }
    return null;
  }
  function pickAnchor(sx: number, sy: number): TopicAnchor | null {
    for (const a of anchors) {
      const [px, py] = toScreen(a.x, a.y);
      if ((px - sx) ** 2 + (py - sy) ** 2 < 14 * 14) return a;
    }
    return null;
  }

  function localXY(e: MouseEvent): [number, number] {
    const r = canvasEl!.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  }

  function onPointerDown(e: PointerEvent) {
    const [sx, sy] = localXY(e);
    moved = false;
    // Anchors hit-test first (drawn on top). A press on an anchor begins an
    // anchor drag; a release without movement still scopes to that topic.
    const anchor = pickAnchor(sx, sy);
    if (anchor) {
      draggingAnchor = anchor;
      anchor.fixed = true; // hold it under the pointer during the drag
      canvasEl?.setPointerCapture(e.pointerId);
      restartLoop();
      return;
    }
    // Then image nodes: a press on a node drags that node.
    const node = pickNode(sx, sy);
    if (node) {
      dragging = node;
      node.fixed = true;
      canvasEl?.setPointerCapture(e.pointerId);
      restartLoop();
      return;
    }
    // Empty canvas: begin a background pan (the gesture the founder was missing).
    panning = { startX: sx, startY: sy, panX0: panX, panY0: panY };
    canvasEl?.setPointerCapture(e.pointerId);
  }

  /** Expand a LOD super-node into its member image nodes (DESIGN v2). The
   * members spill out from the super-node's position with their real per-image
   * affinity; the sim re-runs over the mixed set (this cluster expanded, the
   * rest still aggregated), staying within the budget. */
  function expandSuper(node: ImageNode) {
    nodes = expandSuperNode(nodes, node.hash, affinity, topics.length);
    nodeCount = nodes.length;
    restartLoop();
  }
  /** A small movement threshold (screen px) so a tiny jitter during a click is
   * still read as a click, not a drag. */
  const MOVE_THRESHOLD = 4;

  function onPointerMove(e: PointerEvent) {
    const [sx, sy] = localXY(e);
    if (panning) {
      // Pan the view by the raw screen delta (pan is in screen px, so no zoom
      // division). Past the threshold this counts as a real gesture.
      const dx = sx - panning.startX;
      const dy = sy - panning.startY;
      if (Math.abs(dx) > MOVE_THRESHOLD || Math.abs(dy) > MOVE_THRESHOLD) moved = true;
      panX = panning.panX0 + dx;
      panY = panning.panY0 + dy;
      draw();
      return;
    }
    const [x, y] = fromScreen(sx, sy);
    if (draggingAnchor) {
      draggingAnchor.x = x;
      draggingAnchor.y = y;
      moved = true;
      return;
    }
    if (dragging) {
      dragging.x = x;
      dragging.y = y;
      moved = true;
    }
  }
  let downAt = 0;
  function onPointerUp(e: PointerEvent) {
    if (panning) {
      // End the pan; nothing else to do (the view already moved live).
      panning = null;
      canvasEl?.releasePointerCapture(e.pointerId);
      return;
    }
    if (draggingAnchor) {
      const anchor = draggingAnchor;
      anchor.fixed = false;
      draggingAnchor = null;
      canvasEl?.releasePointerCapture(e.pointerId);
      const quick = performance.now() - downAt < 250;
      if (!moved && quick) {
        // A quick press-release with no movement on an anchor SELECTS it for
        // baking (DESIGN-TOPICS-COLLECTIONS.md, the signature gesture): the
        // threshold slider appears, the above-threshold images glow, and "Make
        // a collection" bakes the selection. Scoping the grid to a topic's
        // ranked images is the Topics rail tab's job (ui.openTopic); the graph
        // is the spatial bake surface.
        selectTopicForBake(anchor.topic);
      } else {
        // A dragged anchor stays where the user put it: PIN it so the anchor
        // forces leave it alone until the user unpins (double-click).
        anchor.pinned = true;
        restartLoop();
      }
      return;
    }
    if (!dragging) return;
    const node = dragging;
    node.fixed = false;
    dragging = null;
    canvasEl?.releasePointerCapture(e.pointerId);
    // A quick press-release with little movement is a CLICK. On a LOD super-node
    // a click EXPANDS it into its members; on a single image it opens Look. A
    // drag just releases the node back into the physics.
    if (!moved && performance.now() - downAt < 250) {
      if (node.members !== undefined) {
        expandSuper(node);
      } else {
        void ui.openFromGraph(node.hash);
      }
    }
  }

  /** Double-click a PINNED anchor to release it back into the physics (founder
   * dogfood: "double-click to unpin"). A reheat lets it find its new home
   * quickly. Double-click elsewhere is a no-op. */
  function onDblClick(e: MouseEvent) {
    const [sx, sy] = localXY(e);
    const anchor = pickAnchor(sx, sy);
    if (anchor && anchor.pinned === true) {
      anchor.pinned = false;
      reheat();
      restartLoop();
    }
  }

  // Zoom level past which LOD super-nodes auto-expand into members, and below
  // which an expanded view re-collapses to super-nodes (DESIGN v2: "expand past
  // a zoom threshold; collapse on zoom-out"). Only acts while LOD is active.
  const LOD_ZOOM_EXPAND = 2.0;
  const LOD_ZOOM_COLLAPSE = 1.2;

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
    const zoom0 = zoom;
    const zoom1 = Math.max(0.2, Math.min(5, zoom0 * factor));
    // Zoom toward the CURSOR, not the canvas center (founder dogfood): keep the
    // sim point currently under the pointer fixed on screen by adjusting the pan
    // for the zoom ratio. screen = center + pan + sim*zoom, so the offset of the
    // cursor from center scales by zoom1/zoom0 around the cursor.
    const [cx, cy] = localXY(e);
    const ratio = zoom1 / zoom0;
    panX = cx - width / 2 - (cx - width / 2 - panX) * ratio;
    panY = cy - height / 2 - (cy - height / 2 - panY) * ratio;
    zoom = zoom1;
    if (lodActive) {
      const anyExpanded = nodes.some((n) => n.members === undefined);
      const anyAggregated = nodes.some((n) => n.members !== undefined);
      if (zoom >= LOD_ZOOM_EXPAND && anyAggregated) {
        // Zoomed in: expand every remaining super-node into its members.
        for (const n of [...nodes]) {
          if (n.members !== undefined) {
            nodes = expandSuperNode(nodes, n.hash, affinity, topics.length);
          }
        }
        nodeCount = nodes.length;
        restartLoop();
      } else if (zoom <= LOD_ZOOM_COLLAPSE && anyExpanded) {
        // Zoomed out: re-aggregate the full detail set back into super-nodes.
        const fullNodes = seedNodes(
          [...affinity.keys()],
          affinity,
          topics.length,
        );
        nodes = aggregateToSuperNodes(fullNodes, topics.length);
        nodeCount = nodes.length;
        restartLoop();
      }
    }
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
    return () => {
      cancelAnimationFrame(raf);
      // Detach in-flight thumb loads so a completing load can't repaint a dead
      // canvas after teardown.
      thumbs.dispose();
    };
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

  // The Attention overlay reacts to the persisted mode (Off / Engaged /
  // Overlooked) and the heatmap's All-time flag: a change re-fetches intensity
  // (reusing image_intensity) + re-ranks. untrack so only those two retrigger it,
  // not the whole reactive surface.
  $effect(() => {
    void ui.graphAttention;
    void ui.heatAllTime;
    untrack(() => {
      if (tuning !== null) void refreshOverlay();
    });
  });

  // Keep both canvas backing stores (node layer + field layer) sized to the box.
  $effect(() => {
    if (canvasEl) {
      canvasEl.width = width;
      canvasEl.height = height;
    }
    if (fieldEl) {
      fieldEl.width = width;
      fieldEl.height = height;
    }
    if (canvasEl) draw();
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

    <!-- Attention overlay: Off / Engaged / Overlooked (heatmap x graph
         synthesis). Engaged shows where attention LIVES; Overlooked shows the
         coherent bodies of work you've barely touched. Persisted on the ui store. -->
    <div class="attention" role="group" aria-label="Attention overlay">
      <span class="attn-label">Attention</span>
      <div class="attn-seg">
        <button
          class="attn-btn"
          class:on={ui.graphAttention === "off"}
          aria-pressed={ui.graphAttention === "off"}
          onclick={() => ui.setAttention("off")}>Off</button
        >
        <button
          class="attn-btn"
          class:on={ui.graphAttention === "engaged"}
          aria-pressed={ui.graphAttention === "engaged"}
          title="Where your attention lives"
          onclick={() => ui.setAttention("engaged")}>Engaged</button
        >
        <button
          class="attn-btn"
          class:on={ui.graphAttention === "overlooked"}
          aria-pressed={ui.graphAttention === "overlooked"}
          title="Coherent bodies of work you've barely touched"
          onclick={() => ui.setAttention("overlooked")}>Overlooked</button
        >
      </div>
    </div>

    <!-- Influence field layer toggle (DESIGN-SEMANTIC-GRAPH.md, founder June
         2026): a soft colored glow behind the nodes showing each topic's power
         level where its strong images landed. A SEPARATE layer from the
         Attention overlay; the two coexist. Default off. Persisted via prefs. -->
    <label class="field-toggle">
      <input
        type="checkbox"
        checked={fieldOn}
        onchange={toggleField}
        aria-label="Topic influence field"
      />
      Field
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

  <!-- v2 cluster auto-labels: note-grounded auto-topics, click to add as an
       anchor. Shown above the cheap rail (smarter, "connected to notes"). -->
  {#if clusterSuggestions.length > 0}
    <div class="rail cluster-rail" aria-label="Auto topics from clusters">
      <span class="rail-label">auto topics</span>
      {#each clusterSuggestions as c (c.label)}
        {#if !topics.includes(c.label)}
          <button
            class="chip suggest cluster"
            onclick={() => addTopic(c.label)}
            title={`cluster of ${c.size} images (tightness ${c.centroid_affinity.toFixed(2)})`}
          >
            {c.label}
            <span class="chip-count">{c.size}</span>
          </button>
        {/if}
      {/each}
    </div>
  {/if}

  <!-- v3 SEAM: the LLM topic-suggestion rail. The Gemma connector is not wired
       yet (mocked in M1), so this degrades to a one-line note and the cluster +
       n-gram suggestions stand in. It appears as real suggestions only once the
       connector lands. -->
  {#if llmSuggestions.length > 0}
    <div class="rail llm-rail" aria-label="LLM suggested topics">
      <span class="rail-label">themes</span>
      {#each llmSuggestions as phrase (phrase)}
        {#if !topics.includes(phrase)}
          <button class="chip suggest" onclick={() => addTopic(phrase)}>{phrase}</button>
        {/if}
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

  <!-- readiness honesty + LOD note -->
  <div class="status">
    {#if loading}
      <span>computing affinities...</span>
    {:else}
      <span>{imageTotal.toLocaleString()} images</span>
      {#if lodActive}
        <span class="dim">rendering {nodeCount.toLocaleString()} nodes</span>
      {/if}
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

  <!-- Attention readout: ranks the topics by where attention lives (Engaged) or
       which coherent themes are cold (Overlooked). Only the topics carrying a
       non-zero score are listed, strongest first. -->
  {#if ui.graphAttention !== "off" && topics.length > 0}
    <div class="readout" aria-label="Attention readout">
      <span class="readout-label">
        {ui.graphAttention === "engaged" ? "Engaged" : "Overlooked"}:
      </span>
      {#if rankedNonzero.length > 0}
        {#each rankedNonzero as r, i (r.topic)}
          <span class="readout-topic">
            {topics[r.topic]}{i < rankedNonzero.length - 1 ? " ·" : ""}
          </span>
        {/each}
      {:else}
        <span class="dim">
          {ui.graphAttention === "engaged"
            ? "no attention recorded in this scope yet"
            : "nothing coherent and cold here yet"}
        </span>
      {/if}
    </div>
  {/if}

  <div
    class="canvas-wrap"
    bind:clientWidth={width}
    bind:clientHeight={height}
  >
    <!-- The influence-field layer: the BOTTOM canvas, drawn under the nodes +
         overlay. Pointer-transparent so all gestures reach the node canvas on
         top. Shows each topic's affinity power as a soft hued glow where its
         strong images landed; topics composite so overlaps blend hues. -->
    <canvas class="field-layer" bind:this={fieldEl}></canvas>

    <canvas
      bind:this={canvasEl}
      onpointerdown={(e) => {
        downAt = performance.now();
        onPointerDown(e);
      }}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
      onpointercancel={onPointerUp}
      ondblclick={onDblClick}
      onwheel={onWheel}
    ></canvas>

    <!-- The slider-to-collection bake panel (the signature gesture): appears
         when a topic anchor is selected. Drag the threshold -> the
         above-threshold images glow in the graph -> "Make a collection" bakes
         the selection. Floats over the canvas, theme-aware. -->
    {#if selectedTopic !== null}
      <div class="bake-panel" aria-label="Bake topic into a collection">
        <div class="bake-head">
          <span class="bake-topic">{topics[selectedTopic] ?? ""}</span>
          <button
            class="bake-x"
            aria-label="Close bake panel"
            onclick={() => selectTopicForBake(selectedTopic!)}>x</button
          >
        </div>
        <label class="bake-slider">
          <span class="lbl">threshold</span>
          <input
            type="range"
            min="0"
            max="1"
            step="0.01"
            bind:value={bakeThreshold}
            aria-label="Affinity threshold"
          />
          <span class="val">{bakeThreshold.toFixed(2)}</span>
        </label>
        <div class="bake-count">
          {glowCount.toLocaleString()} {glowCount === 1 ? "photo" : "photos"}
        </div>
        <div class="bake-commit">
          <input
            class="bake-name"
            bind:value={bakeName}
            placeholder="Collection name"
            aria-label="New collection name"
          />
          <button
            class="bake-go"
            disabled={baking || glowCount === 0 || bakeName.trim() === ""}
            onclick={() => void commitBake()}
          >
            {baking ? "Baking..." : "Make a collection"}
          </button>
        </div>
      </div>
    {/if}
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
  .field-toggle {
    display: flex;
    align-items: center;
    gap: 4px;
    color: var(--text-dim);
    font-size: 12px;
  }
  .attention {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
  }
  .attn-label {
    color: var(--text-dim);
  }
  .attn-seg {
    display: inline-flex;
    border: 1px solid var(--chrome);
    border-radius: 6px;
    overflow: hidden;
  }
  .attn-btn {
    border: none;
    border-radius: 0;
    border-left: 1px solid var(--chrome);
    background: var(--bg-raised);
    color: var(--text-dim);
    padding: 3px 8px;
  }
  .attn-btn:first-child {
    border-left: none;
  }
  .attn-btn.on {
    background: var(--chrome-strong);
    color: var(--text);
  }
  .readout {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 4px 12px;
    font-size: 12px;
    color: var(--text);
    border-bottom: 1px solid var(--chrome);
    flex-wrap: wrap;
  }
  .readout-label {
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
  }
  .readout .dim {
    color: var(--text-faint);
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
  .rail-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-faint);
    align-self: center;
  }
  .chip.cluster {
    border-color: var(--chrome-strong);
  }
  .chip-count {
    font-size: 10px;
    color: var(--text-faint);
    background: var(--bg);
    border-radius: 999px;
    padding: 0 5px;
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
  /* The influence-field layer sits UNDER the node canvas (DOM order = stacking
     for these absolutely-positioned peers) and never intercepts pointer events,
     so every gesture reaches the node canvas on top. */
  .field-layer {
    pointer-events: none;
  }
  /* The slider-to-collection bake panel: floats over the canvas (top-right),
     theme-aware (tokens only). */
  .bake-panel {
    position: absolute;
    top: 12px;
    right: 12px;
    width: 240px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    background: var(--bg-raised);
    border: 1px solid var(--chrome-strong);
    border-radius: 8px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.3);
    font-size: 12px;
    color: var(--text);
  }
  .bake-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .bake-topic {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bake-x {
    all: unset;
    cursor: pointer;
    color: var(--text-faint);
    padding: 0 4px;
  }
  .bake-x:hover {
    color: var(--text);
  }
  .bake-slider {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-dim);
  }
  .bake-slider .lbl {
    color: var(--text-faint);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
  }
  .bake-slider input[type="range"] {
    flex: 1;
    min-width: 0;
  }
  .bake-slider .val {
    color: var(--text);
    font-variant-numeric: tabular-nums;
    min-width: 2.4em;
    text-align: right;
  }
  .bake-count {
    color: var(--text);
    font-weight: 600;
  }
  .bake-commit {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .bake-name {
    background: var(--bg);
    border: 1px solid var(--chrome);
    border-radius: 4px;
    color: var(--text);
    padding: 4px 8px;
    font-size: 12px;
  }
  .bake-go {
    background: var(--bg);
    border: 1px solid var(--chrome-strong);
    color: var(--text);
    padding: 5px 10px;
    border-radius: 4px;
    cursor: pointer;
  }
  .bake-go:hover:not(:disabled) {
    border-color: var(--focus);
  }
  .bake-go:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
