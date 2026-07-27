/** IPC DTOs (camelCase) — twins of `src-tauri/src/dto.rs`. */

export interface RootDto {
  rootId: string;
  displayName: string;
  relPath: string;
  volumeId: string;
  online: boolean;
  absPath: string | null;
  /** Archived (lifecycle): hidden from the active rail, kept whole.
   * `listRoots` returns only active roots (false); the archived snapshot
   * from `listArchivedRoots` carries the archived ones (true). */
  archived: boolean;
}

export type ApplicationStateDomain =
  | "settings"
  | "roots"
  | "collections"
  | "topics"
  | "runtime"
  | "preview-cache";

export interface ApplicationStateChanged {
  revision: number;
  domains: ApplicationStateDomain[];
}

export interface ApplicationStateRevisions {
  settings: number;
  roots: number;
  collections: number;
  topics: number;
  runtime: number;
  previewCache: number;
}

/** Retained post-launch truth for one externally editable native control. */
export interface LiveControlStatus {
  name: "settings" | "config" | "tuning";
  lastAttemptedAtMs: number | null;
  lastAppliedAtMs: number | null;
  lastRecoveredAtMs: number | null;
  retainedError: string | null;
  recoverySource: string | null;
  quarantined: string[];
  warnings: string[];
}

/** Coherent catch-up snapshot tagged by the backend's process-monotone state
 * clock. Windows apply only strictly newer revisions. */
export interface ApplicationStateSnapshot {
  revision: number;
  revisions: ApplicationStateRevisions;
  settings: AppSettings;
  liveControls: LiveControlStatus[];
  roots: RootDto[];
  archivedRoots: RootDto[];
  collections: CollectionDto[];
  topics: TopicDto[];
  runtime: RuntimeStatus;
  previewCache: PreviewCacheStatsDto;
}

export interface UpdateMetadata {
  version: string;
  currentVersion: string;
  notes: string | null;
  publishedAt: string | null;
}

/** Signed application updater state. `enabled` is false in developer and
 * unsigned CI bundles; those builds cannot contact or install from a feed. */
export interface UpdateStatus {
  enabled: boolean;
  currentVersion: string;
  phase:
    | "disabled"
    | "idle"
    | "checking"
    | "current"
    | "available"
    | "downloading"
    | "verified"
    | "stopping"
    | "restarting"
    | "failed";
  available: UpdateMetadata | null;
  downloadedBytes: number;
  totalBytes: number | null;
  error: string | null;
}

/** Outcome of an `addRoot` attempt — the refuse + alias contract (twin of
 * `AddRootOutcome` in dto.rs). A clean add returns `added`; a folder that
 * overlaps an existing active root returns `overlap` carrying that root's id
 * so the rail can NAVIGATE there instead of double-ingesting. */
export type AddRootOutcome =
  | { kind: "added"; root: RootDto }
  | { kind: "overlap"; existingRootId: string };

export interface FolderNode {
  name: string;
  relPath: string;
  children: FolderNode[];
}

export interface GridItem {
  hash: string;
  fileName: string;
  relPath: string;
  /** Root the representative path sits under — stack pair identity
   * includes it (logic/stacks.ts): a COLLECTION grid mixes roots (B71),
   * and identical camera paths across two roots are unrelated images
   * that must never collapse into one cell. Optional so test fixtures
   * predate it; null = the path row has no root. */
  rootId?: string | null;
  captureTs: string | null;
  addedTs: string;
  hasJournal: boolean;
  /** Folded rating — DATA only; never rendered on thumbnails (UI §3.5). */
  rating: number | null;
  offline: boolean;
  /** A thumb artifact exists in the cache: the grid requests the protocol
   * URL only when true — mid-scan on a network volume, eager requests are
   * thousands of doomed 404 round-trips (founder, SMB, June 2026).
   * Optional so test fixtures predate it; absent = true (request as
   * before — the retry/heal machinery still backstops a 404). */
  previewReady?: boolean;
}

/** Revisioned, folder-scoped catalog catch-up. `reset` means `upserts` is the
 * complete current folder snapshot; otherwise the two arrays are a coalesced
 * delta since `fromRevision`. */
export interface FolderDelta {
  fromRevision: number;
  toRevision: number;
  reset: boolean;
  upserts: GridItem[];
  removedHashes: string[];
}

export type ScopeKind = "single" | "multi" | "session";

export interface ScopeView {
  kind: ScopeKind;
  count: number;
  previewHashes: string[];
  /** DESIGN-VOICE-SUBJECTS.md: when dictation targets a collection/topic note
   * log (a subject detail open, no image focused) rather than an image, this
   * names which kind; absent for the ordinary image/session scope. */
  subject?: "collection" | "topic" | null;
  /** Subject display name (collection name / topic phrase) for the
   * "noting: <name>" indicator copy. */
  subjectName?: string | null;
}

/** DESIGN-VOICE-SUBJECTS.md: the optional non-image dictation subject the
 * frontend reports alongside `targets`. Present ONLY when a collection/topic
 * detail is open AND no image is focused (targets empty then). */
export interface ScopeSubject {
  kind: "collection" | "topic";
  /** The subject's id (ULID). */
  id: string;
  /** Display name echoed straight back for the indicator. */
  name: string;
}

export interface IndicatorState {
  currentScope: ScopeView;
  mic: "disarmed" | "arming" | "armedIdle" | "armedSpeaking" | "disarmedError";
  streamingUtterance: { boundScope: ScopeView; startedAt: string } | null;
  degraded: { asrUnavailable: boolean };
}

export interface IndicatorPulse {
  eventKind: string;
}

/** `journal-changed` payload: the images whose journal truth a committed
 * mutation touched. Open surfaces (journal panel, grid badges, the Look
 * overlay) refresh from this — the seam M2b voice events will ride. */
export interface JournalChanged {
  hashes: string[];
}

/** `previews-changed` payload: the images whose preview artifacts landed
 * in an ingest drain. Thumbs that exhausted their 404 retry budget heal
 * off this instead of staying blank until restart.
 *
 * An EMPTY `hashes` is the GLOBAL signal (manual cache clear emits it):
 * every visible thumb bumps its cache-bust param and re-requests, dropping
 * the webview's immutable-cached bytes for the just-deleted artifacts. */
export interface PreviewsChanged {
  hashes: string[];
}

export interface IngestStatus {
  running: boolean;
  /** Persisted process-wide background-work policy. Optional only for legacy
   * fixtures; the backend always sends both fields. */
  processingPaused?: boolean;
  processingIntensity?: "eco" | "balanced" | "max";
  done: number;
  total: number;
  errors: number;
  /** Pass kinds with work still queued (pending + running), queue-spelled
   * names, deterministic order — the Library-status header indicator's
   * per-stage breakdown (BACKLOG "digest visibility"). Each pass now carries
   * its own progress + a smoothed rate so the header can show per-stage
   * fractions and ETAs:
   *   · done / total — completed vs known units for THIS pass.
   *   · remaining — total - done (the backend ships it explicitly so the
   *     frontend never has to recompute from a possibly-stale total).
   *   · ratePerSec — smoothed items/sec; 0 when unknown OR paused (the
   *     backend FREEZES it during capture/offline so a paused stage shows
   *     no misleading ETA). */
  passes: {
    name: string;
    remaining: number;
    done: number;
    total: number;
    ratePerSec: number;
  }[];
  /** A filesystem walk (add-root initial scan / rescan) is in flight. Pass
   * counters lag the walk — rows only appear at hash time — so this is
   * what keeps `running` true through the walk's dark window (founder,
   * June 2026: "No photographs" over a folder busily being scanned). */
  scanning: boolean;
  /** Files discovered so far by in-flight walks (after exclusions) — the
   * empty grid's quiet "N photographs found so far…" line. */
  discovered: number;
  /** OFFLINE volumes the library has photos on (founder: warn + pause). Each
   * carries its label + how many images it hides, so the shell can warn
   * "drive disconnected, N photos unavailable" instead of stalling silently.
   * Empty in the normal all-online case. */
  offlineVolumes: { label: string; images: number }[];
  /** Monotonic vector-store version (Seam 1, ARCHITECTURE-CONTRACTS.md): bumped
   * once per committed vector write, riding this event so views refresh when
   * their data advances instead of polling. The visualizer compares it against
   * the version it last rendered against and re-fetches only on an advance —
   * this retires the old self-heal poll. Serialized as `vectorsVersion`. */
  vectorsVersion: number;
  /** Monotonic image-set version (Seam 1, sibling of `vectorsVersion`): bumped
   * once per committed NEW image row. The grid re-lists when it advances —
   * replacing the retired 2s wall-clock throttle + the redundant App.svelte
   * setInterval. Serialized as `imagesVersion`. */
  imagesVersion: number;
  /** Monotonic journal version (Seam 1, sibling of `vectorsVersion`): bumped
   * once per minted / redacted / merged journal event. The inspector re-reads
   * when it advances. Serialized as `journalVersion`. */
  journalVersion: number;
}

/** Per-role embedder slot state (twin of `dto.rs`): the HONEST lifecycle of
 * one embedder lane (CLIP image / EmbeddingGemma text), so a FAILED lane can
 * be told apart from one still warming up or one simply not configured:
 *   · idle     — not configured/active on this build; do NOT nag (the M1
 *     text lane sits here when unlit).
 *   · queued   — waiting for the serialized native-build lane.
 *   · building — sessions constructing; a transient "loading" state.
 *   · ready    — sessions constructed; the lane serves search.
 *   · failed   — construction errored; the lane is DEGRADED (a real,
 *     surfaced fault, not a forever-loading lie).
 *   · stopping — shutdown has invalidated this lane; stale native landings
 *     cannot make it ready again. */
export type EmbedderState =
  | "idle"
  | "queued"
  | "building"
  | "ready"
  | "failed"
  | "stopping";

/** One embedder lane's state + an optional human error. `error` is non-null
 * ONLY when `state === "failed"` (the construction fault to surface). */
export interface EmbedderSlot {
  state: EmbedderState;
  attemptId: number | null;
  modelId: string | null;
  generation: number;
  startedAt: string | null;
  error: string | null;
  /** Added by the native runtime for Ready slots. Optional keeps old persisted
   * debug snapshots/test recordings readable; live payloads always send it. */
  execution?: ModelExecution | null;
}

export type ExecutionSelection = "cpu" | "core-ml" | "cuda" | "tensor-rt" | "unknown";

export interface SessionExecution {
  requested: string[];
  available: string[];
  registered: string[];
  /** Provider selected/configured at session construction. This is not proof
   * that graph nodes executed there. */
  selected: ExecutionSelection;
  /** Providers observed executing graph nodes. Empty means actual execution
   * remains unproved; mixed CPU/accelerator execution retains both names. */
  actual: string[];
  fallbackReason: string | null;
  measurement: string;
  profilePath: string | null;
}

export interface ModelExecution {
  modelId: string;
  sessions: SessionExecution[];
}

/** RUNTIME (P6.2): tier + consent + per-model license/progress rows.
 * `asrReady`/`llmReady` are the §8.3 readiness gates — false until a
 * supervised child reports Ready (never true before P6.3 vendors real
 * binaries); features light up individually and silently as they flip. */
export interface RuntimeStatus {
  asrReady: boolean;
  llmReady: boolean;
  /** Plan says Run but the binary could not be resolved (e.g. a dev
   * target prune ate pp-asr-server — founder incident, June 2026): the
   * human reason. Distinct from `asrReady === false`, which also covers
   * the normal silent warm-up; blocked means it will NEVER flip ready
   * until the binary returns. null = not blocked. */
  asrBlocked: string | null;
  llmBlocked: string | null;
  /** P7.4 §3.3: in-process embedder readiness — true once the ort sessions
   * are constructed. Additive; like asr/llm they light up silently and gate
   * the semantic-search backfill, never blocking the journal. */
  clipReady: boolean;
  textEmbedderReady: boolean;
  /** Per-role embedder slot state — the HONEST lifecycle the Library-status
   * indicator reads (idle/queued/building/ready/failed/stopping), distinct from the coarse
   * `clipReady`/`textEmbedderReady` bools (kept for other consumers). A failed
   * lane reads as DEGRADED here where the bool collapsed it to "never ready". */
  clip: EmbedderSlot;
  textEmbedder: EmbedderSlot;
  /** Capability discovery never blocks first paint: launch begins with a
   * safe/cached provisional decision, a managed task transitions through
   * detecting, then atomically publishes ready or failed. */
  capabilityState: "provisional" | "detecting" | "ready" | "failed";
  capabilitySummary: string | null;
  capabilityAdapters: {
    name: string;
    backend: string;
    vendorId?: number | null;
    deviceId?: number | null;
    driver?: string | null;
    driverInfo?: string | null;
    vramBytes: number | null;
  }[];
  capabilityDetectedAt: string | null;
  capabilities?: RuntimeCapabilities | null;
  tierDetected: number;
  /** After the always-winning user override (§6.2). */
  tierEffective: number;
  /** Overriding ABOVE detected hardware: the one-time plain warning. */
  tierOverriddenAbove: boolean;
  /** "undecided" | "later" | "never" | "download" (§10.3). */
  consent: string;
  /** Live manifest byte sum at the effective tier (§5.4). */
  consentOfferBytes: number;
  models: ModelRowDto[];
  instanceLockHeld: boolean;
  /** Recovery/durability truth for config, consent, acceptances, tier cache,
   * manifest publication, and the child-process crash registry. */
  controlFiles: RuntimeControlFileStatus[];
}

/** A consent write and its optional automatic download dispatch settle
 * independently. A resolved response always means the consent is durable;
 * `operationError` means model work did not start and may be retried without
 * pretending the saved choice rolled back. */
export interface RuntimeConsentOutcome {
  status: RuntimeStatus;
  consentCommitted: true;
  operationError: string | null;
  operationRetryable: boolean;
}

export interface RuntimeCapabilities {
  reportSchemaVersion: number;
  detectedAt: string;
  hardwareFingerprint: string;
  os: string;
  architecture: string;
  totalMemoryBytes: number | null;
  appleUnifiedBytes: number | null;
  adapters: RuntimeStatus["capabilityAdapters"];
  providers: {
    provider: string;
    compiled: boolean;
    runtimeAvailable: boolean | null;
    error: string | null;
  }[];
  runtimeLibraryAvailable: boolean;
  modelCompatibility: {
    modelId: string;
    compatible: boolean;
    compatibleProviders: string[];
    reason: string;
  }[];
}

export interface RuntimeControlFileStatus {
  name: string;
  recovery: {
    source: "primary" | "lastKnownGood" | "missing";
    quarantined: string[];
    warnings: RuntimeControlFileError[];
  } | null;
  errors: RuntimeControlFileError[];
  validationWarnings: string[];
}

export interface RuntimeControlFileError {
  kind: "missing" | "corrupt" | "permissionDenied" | "io";
  path: string;
  detail: string;
  quarantinedPath: string | null;
}

/** Backend-originated process health. This joins the authoritative lifecycle,
 * root/watcher, task, ingest, and runtime snapshots at one observation time so
 * recovery UI does not infer subsystem state independently. */
export interface ApplicationHealth {
  observedAtMs: number;
  phase:
    | "cold"
    | "opening-data"
    | "usable"
    | "reconciling"
    | "ready"
    | "stopping";
  /** Product-facing projection of every currently unhealthy authority. Unlike
   * the detailed diagnostics below, every row carries one real safe action. */
  issues: {
    id: string;
    subsystem: string;
    title: string;
    blocking: boolean;
    summary: string;
    lastError: string | null;
    lastErrorAtMs: number | null;
    action: {
      kind:
        | "retry-root"
        | "retry-roots"
        | "retry-runtime"
        | "retry-repair"
        | "redetect-runtime"
        | "verify-model"
        | "download-model"
        | "accept-model-license"
        | "rebuild-previews"
        | "restore-controls"
        | "reveal-logs";
      label: string;
      targetId: string | null;
    };
  }[];
  phaseTimings: {
    phase: ApplicationHealth["phase"];
    enteredAtMs: number;
    elapsedMs: number;
  }[];
  subsystems: {
    name: string;
    state: "unknown" | "healthy" | "degraded" | "unavailable";
    blocking: boolean;
    summary: string | null;
    action: string | null;
  }[];
  database: {
    state: "healthy" | "unavailable";
    schemaVersion: number | null;
    expectedSchemaVersion: number;
    error: string | null;
  };
  volumes: {
    volumeId: string;
    label: string;
    online: boolean;
    readOnly: boolean;
    fsType: string | null;
    mountPoint: string | null;
  }[];
  volumeInventoryError: string | null;
  roots: {
    rootId: string;
    displayName: string;
    volumeId: string;
    online: boolean;
    watcherActive: boolean;
    lifecycleState: string;
    state: "healthy" | "archived" | "degraded" | "unavailable";
    summary: string | null;
    action: string | null;
  }[];
  tasks: {
    owner: string;
    key: string;
    priority: "background" | "maintenance";
    state: "running" | "completed" | "failed" | "cancelled";
    startedAtMs: number;
    endedAtMs: number | null;
    progress: number | null;
    progressMessage: string | null;
    lastError: string | null;
  }[];
  commandWork: {
    id: number;
    name: string;
    class: "read" | "mutation";
    startedAtMs: number;
  }[];
  controlFiles: {
    name: string;
    state: "healthy" | "degraded" | "unavailable";
    source: "primary" | "last-known-good" | "missing-default" | "created" | null;
    quarantined: string[];
    warnings: string[];
    error: string | null;
    action: string | null;
  }[];
  disk: {
    observedAtMs: number;
    state: "healthy" | "warning" | "critical" | "unknown";
    appDataState: "healthy" | "warning" | "critical" | "unknown";
    modelsState: "healthy" | "warning" | "critical" | "unknown";
    derivedWorkPaused: boolean;
    warningFreeBytes: number;
    criticalFreeBytes: number;
    wal: {
      path: string;
      sizeBytes: number | null;
      modifiedAtMs: number | null;
      ageMs: number | null;
      state: "healthy" | "warning" | "critical" | "blocked" | "unknown";
      warningBytes: number;
      criticalBytes: number;
      warningAgeMs: number;
      criticalAgeMs: number;
      inventoryError: string | null;
      lastMaintenanceAttemptAtMs: number | null;
      lastMaintenanceSuccessAtMs: number | null;
      lastMaintenanceFailureAtMs: number | null;
      lastMaintenanceError: string | null;
      blockedByReader: boolean;
    };
    stores: {
      name:
        | "database-and-wal"
        | "previews"
        | "full-decode-cache"
        | "vectors"
        | "models"
        | "download-parts";
      path: string;
      usedBytes: number | null;
      fileCount: number | null;
      availableBytes: number | null;
      state: "healthy" | "warning" | "critical" | "unknown";
      inventoryErrors: number;
    }[];
  };
  resources: {
    intensity: "eco" | "balanced" | "max";
    paused: boolean;
    budget: {
      totalConcurrency: number;
      ingestConcurrency: number;
      ingestBatch: number;
      embeddingBatch: number;
      rawBatch: number;
    };
    activeTotal: number;
    lanes: {
      lane:
        | "interactiveRaw"
        | "liveIngest"
        | "preview"
        | "modelDownload"
        | "embedding"
        | "rootScan"
        | "startupIo"
        | "repair"
        | "maintenance";
      active: number;
      waiting: number;
    }[];
  };
  diagnostics: {
    buildVersion: string;
    previousUncleanLaunch: boolean;
    logsDir: string | null;
    currentLog: string | null;
    error: string | null;
  };
  /** Retained startup repair outcome; unlike managed task history this remains
   * available after the integrity task reaches terminal state. */
  repairIntegrity: {
    state: "pending" | "running" | "completed" | "degraded" | "cancelled";
    startedAtMs: number | null;
    completedAtMs: number | null;
    vectorReconciled: boolean | null;
    orphanedPassesSkipped: number | null;
    retention: {
      repended: number;
      staleOrphans: number;
      orphanImages: number;
      retentionEligible: number;
      retentionDeferredRecent: number;
      retentionDeferredUnknownTimestamp: number;
      retentionDeferredBusy: number;
      retentionDryRun: boolean;
      reclaimedImages: number;
      previewRowsReclaimed: number;
      previewFilesReclaimed: number;
      previewBytesReclaimed: number;
      vectorRowsReclaimed: number;
      vectorSpacesCompacted: number;
      journalVectorRowsRetained: number;
      tempsSwept: number;
    } | null;
    roots: {
      totalRoots: number;
      scannedRoots: number;
      degradedRoots: number;
      newImages: number;
      superseded: number;
      relinked: number;
      retentionRepairsRevived: number;
      wentStale: number;
      ioErrors: number;
    } | null;
    errors: string[];
  };
  /** Local-only structured journey percentiles plus always-on ingest/preview
   * stage histograms. Optional for older fixtures; current backends send it. */
  performance?: {
    journeys: import("../performance").PerformanceSnapshot;
    previewProtocol: {
      initialized: boolean;
      workers: number;
      queueCapacity: number;
      queued: number;
      peakQueued: number;
      running: number;
      peakRunning: number;
      interactive: PreviewProtocolPriorityMetrics;
      thumbnail: PreviewProtocolPriorityMetrics;
    };
    ingestStages: {
      stage: string;
      count: number;
      totalMs: number;
      meanMs: number;
      p50Ms: number;
      p95Ms: number;
      p99Ms: number;
      maxMs: number;
    }[];
    /** Fixed-label shared SQLite catalog timings. `.wait` measures mutex
     * acquisition; `.operation` starts after the lane is held. */
    catalogLanes: {
      stage: string;
      count: number;
      totalMs: number;
      meanMs: number;
      p50Ms: number;
      p95Ms: number;
      p99Ms: number;
      maxMs: number;
    }[];
  };
  ingest: IngestStatus;
  runtime: RuntimeStatus;
}

export interface PreviewProtocolPriorityMetrics {
  accepted: number;
  completed: number;
  overloaded: number;
  superseded: number;
  meanQueueWaitMs: number;
  queueWaitP50Ms: number | null;
  queueWaitP95Ms: number | null;
  queueWaitP99Ms: number | null;
  maxQueueWaitMs: number;
  meanServiceMs: number;
  serviceP50Ms: number | null;
  serviceP95Ms: number | null;
  serviceP99Ms: number | null;
  maxServiceMs: number;
}

/** Minimal process-open result. This remains queryable even when the full
 * application state could not open its database or migrate authored data. */
export interface BootstrapStatus {
  state: "opening" | "ready" | "fatal";
  error: string | null;
  recoveryAction: "reset-device-identity" | null;
}

export interface ModelRowDto {
  id: string;
  role: string;
  /** Included in the backend-selected first-run consent set. */
  defaultOffer: boolean;
  /** Compatible explicit alternative, never auto-enqueued by consent. */
  advancedAvailable: boolean;
  compatible: boolean;
  compatibilityReason: string;
  compatibleProviders: string[];
  /** Backend-joined desired/active/runtime/provider truth for every consumer. */
  consumers: Array<{
    role: string;
    desired: boolean;
    active: boolean;
    state: string;
    retryable: boolean;
    error: string | null;
    requestedProvider: string | null;
    actualProvider: string | null;
    fallbackReason: string | null;
  }>;
  /** "not-offered" | "not-downloaded" | "downloading" | "installed" | "failed". */
  state: string;
  totalBytes: number;
  /** Model-cumulative: bytes of this model on disk, never per-file. */
  downloadedBytes: number;
  licenseName: string;
  licenseUrl: string;
  acceptanceRequired: boolean;
  accepted: boolean;
  error: string | null;
  /**
   * Set while an interrupted transfer is auto-retrying: the row stays
   * "downloading" (error is terminal, written only after the retry
   * schedule is exhausted) and this names the retry in flight.
   */
  retryHint: string | null;
  /** Serialized backend lifecycle operation currently owning this model. */
  operation: string | null;
  /** Latest sequenced operation transition, retained across event gaps. */
  operationEvent: {
    attemptId: string;
    sequence: number;
    phase: string;
    terminal: boolean;
    error: string | null;
  } | null;
  /** Durable index/filesystem disagreement, independent of transfer errors. */
  registryError: string | null;
}

export interface AppSettings {
  lastExportTs: string | null;
  /** Which member a collapsed RAW+JPEG stack displays (featureset §5
   * dogfood amendment; backend settings.rs StackDisplay twin). */
  stackDisplay: "jpeg" | "raw";
  /** Configurable external editor (BACKLOG "Configurable external editor,
   * D4 revisit"): the app name (macOS) / executable (Win/Linux) the
   * "Open in external editor" verb hands the original off to. null = the
   * OS default handler (settings.rs external_editor twin). */
  externalEditor: string | null;
  /** 1:1 preview cache budget in BYTES (DESIGN-PREVIEW-POLICY.md): keep
   * full-res 1:1 develop artifacts until the on-disk cache exceeds this, then
   * evict least-recently-viewed. The UI edits it in GB; defaults to 20 GB
   * (settings.rs previewCacheBudgetBytes twin). */
  previewCacheBudgetBytes: number;
  /** Process-wide CPU/RAM/I/O budget. Optional only for old test fixtures;
   * the backend always sends it. */
  processingIntensity?: "eco" | "balanced" | "max";
  /** Explicit background-processing pause; interactive RAW develop stays live. */
  processingPaused?: boolean;
  /** Default behavior after registering a newly-added source folder. */
  newRootPolicy?: "process-now" | "preview-only" | "process-later";
  /** Effective policy frozen for each source when it was added. */
  rootProcessingPolicies?: Record<
    string,
    "process-now" | "preview-only" | "process-later"
  >;
  deferTextEmbeddings?: boolean;
  deferImageEmbeddings?: boolean;
}

/** Settings → Previews cache-size readout (DESIGN-PREVIEW-POLICY.md). All
 * sizes in bytes; the UI formats GB/MB. `full*` is the budgeted 1:1 tier,
 * `totalBytes` the whole previews footprint, `budgetBytes` the configured cap
 * (so the readout shows "X of Y" without a second round-trip). */
export interface PreviewCacheStatsDto {
  fullBytes: number;
  fullFiles: number;
  totalBytes: number;
  budgetBytes: number;
}

export interface ExportReportDto {
  dir: string;
  manifestPath: string;
  images: number;
  events: number;
  sessions: number;
}

/** Receipt written only by the offline helper after the live desktop process
 * has exited. A successful restore retains `rollbackPath` until the user has
 * verified the restored library. */
export interface OperationReceipt {
  operation: "backup" | "restore";
  succeeded: boolean;
  completedAt: string;
  backupPath: string;
  rollbackPath: string | null;
  detail: string;
}

export interface RebuildReportDto {
  filesScanned: number;
  filesParsed: number;
  failures: number;
}

// ---------------------------------------------------------------------------
// P4.2 additions (contracts frozen by FOUNDATIONS; bodies land with their
// stages: journal/metadata — Stage C, paths/OS — Stage A).
// ---------------------------------------------------------------------------

/** Integer [x, y, p, t] stroke sample (EVENTS §3.3 wire form: x/y in
 * ten-thousandths of the display-oriented extent −2500..12500, p per-mille
 * with 1000 = device reports none, t = ms offset from pen-down). */
export type StrokeWirePoint = [number, number, number, number];

/** CAPTURE §8.2 stroke payload — `add_stroke`'s input (P5.1). Canonical
 * integers only; the Rust side validates, core re-validates on append. */
export interface StrokePayloadWire {
  baseW: number;
  orientation: number;
  points: StrokeWirePoint[];
  tool: "pencil";
}

/** `add_stroke`'s output: the minted event id plus the session it landed
 * in. The pencil undo stack is session-scoped (CAPTURE §8.5, DECISIONS C4
 * "this-session only"); session closure is lazy, so the echoed session id
 * is how the frontend observes a rotation and clears the stack. */
export interface StrokeCommitDto {
  id: string;
  sessionId: string;
}

/** Stroke geometry riding a journal row (the Look overlay and the journal
 * micro-previews render from this; `pencil` is the only tool in v1). */
export interface StrokeDto {
  baseW: number;
  orientation: number;
  points: StrokeWirePoint[];
}

/** One folded journal row (inspector Journal tab — featureset §3, D2).
 * Revisions/retractions never appear standalone (EVENTS folds); retracted
 * rows ARE included, flagged, for the per-session "show retracted" toggle. */
export interface JournalEntryDto {
  id: string;
  sessionId: string;
  /** RFC 3339. */
  ts: string;
  kind: "remark" | "rating" | "stroke" | "redacted";
  source: "voice" | "typed" | "system";
  /** Effective (folded) text for remarks; null for ratings/stubs. */
  text: string | null;
  /** Pre-revision original when corrected ("edited" expand affordance). */
  originalText: string | null;
  corrected: boolean;
  retracted: boolean;
  rating: number | null;
  targets: string[];
  linkedEvent: string | null;
  /** Stroke rows only; null/absent elsewhere (and on scrubbed strokes). */
  stroke?: StrokeDto | null;
}

/** Read-only EXIF subset + file identity (Metadata tab, K16 stands —
 * from the db's EXIF subset; no new parsing). */
export interface ImageMetadataDto {
  hash: string;
  fileName: string;
  relPath: string;
  absPath: string | null;
  byteSize: number;
  format: string;
  pixelWidth: number | null;
  pixelHeight: number | null;
  orientation: number;
  captureTs: string | null;
  cameraMake: string | null;
  cameraModel: string | null;
  lensModel: string | null;
  focalLengthMm: number | null;
  iso: number | null;
  fNumber: number | null;
  exposureTime: string | null;
  /** Formatted GPS text (UI renders text only). */
  gps: string | null;
  previewSource: string | null;
  /** Preview backfill still pending (e.g. RAW full-decode). */
  previewPending: boolean;
  firstIngestedAt: string;
}

/** redact_event outcome — drives the sanctioned "Redacted" toast copy,
 * including "— N offline sidecar(s) pending" (UI §7.5/§8.4). */
export interface RedactReportDto {
  /** Event ids scrubbed (target + revision chain). */
  redacted: string[];
  sidecarsUpdated: number;
  /** Labels of offline volumes whose sidecars are scrubbed on next mount. */
  offlinePending: string[];
}

/** One collection (RETRIEVAL §10, B71) as the rail's Collections tab and
 * the thumb menu's Add-to-collection submenu render it. Counts cover
 * CURRENT members (open intervals) only. Snapshots arrive whole on the
 * `collections-changed` event — the frontend never reconciles deltas. */
export interface CollectionDto {
  id: string;
  name: string;
  description: string;
  /** "active" | "shelved" | "done" — shelved/done rows render dimmed. */
  status: string;
  createdTs: string;
  updatedTs: string;
  memberCount: number;
  noteCount: number;
}

/** One append-only collection note (RETRIEVAL §10, B71; P7.3 store). A
 * collection note is about the GROUPING's intent (why these images are
 * together), a DELIBERATELY separate kind from per-image journal events —
 * it never targets a single image. Append-only, like the journal: no edit,
 * no delete (K14: the record preserves the user's own words). */
export interface CollectionNoteDto {
  id: string;
  /** RFC 3339. */
  ts: string;
  text: string;
}

/** One topic note (append-only, never edited or deleted) as the Topics rail
 * tab's note pane renders it. The CollectionNoteDto shape mirrored for topics:
 * a topic note is the user's authored text ABOUT the topic (its definition,
 * what it is for, the refinement intent), keyed to the topic id. */
export interface TopicNoteDto {
  id: string;
  /** RFC 3339. */
  ts: string;
  text: string;
}

/** One saved manual topic (DESIGN-TOPICS-COLLECTIONS.md) as the Topics rail
 * tab renders it. A topic is a saved phrase, like a saved search; its images
 * are ALWAYS computed affinity (`topicRankedImages`), never stored membership —
 * which is precisely what distinguishes a topic from a collection. */
export interface TopicDto {
  id: string;
  phrase: string;
  /** "blend" (both spaces at the configured alpha — the default) | "annotation"
   * (what you SAID) | "clip" (what it LOOKS like). */
  space: string;
  createdTs: string;
}

/** One in-scope image + its blended affinity to a selected topic phrase, for
 * the Topics tab's ranked grid and the threshold slider (descending `score`). */
export interface RankedImageDto {
  hash: string;
  /** Blended affinity (a cosine, roughly [-1, 1]); the slider thresholds on it. */
  score: number;
}

/** One proposed candidate GROUPING (DESIGN-TOPICS-COLLECTIONS.md, autosuggest
 * Phase 3) the Topics tab can offer the human to bake into a collection. K14:
 * the machine PROPOSES, the human commits via the existing bake. Computed on the
 * fly from existing signals (co-annotation, repeated phrases, time+folder
 * bursts); never stored. */
export interface CollectionCandidateDto {
  /** Human-readable name for the proposed grouping. */
  label: string;
  /** Candidate member image hashes (lowercase hex), capped + sorted. */
  members: string[];
  /** "co_annotation" | "repeated_phrase" | "time_folder" — the signal that
   * proposed this (the rail can style each source differently). */
  source: string;
  /** A coherence/size signal for ranking (bigger / tighter groupings first). */
  score: number;
}

/** One near-duplicate GROUP (twin of the Rust `DuplicateGroupDto`,
 * DESIGN-DEDUP-AND-SIMILARITY.md "Tier 1"): a transitive cluster of images whose
 * 64-bit perceptual hashes are within the Hamming threshold of one another, so
 * they are the SAME photo re-saved / resized / lightly edited (exact byte-dupes
 * already collapse to one hash upstream via BLAKE3 — these are the NEAR-dups).
 *
 * `imageHashes` are the member content hashes (lowercase BLAKE3 hex), sorted;
 * groups are always size >= 2 (a singleton is not a duplicate). `count` is the
 * member count, shipped so the "x3" per-group badge needs no `.length`. This is
 * DETECT + DISPLAY only — keep/cull is sidecar truth, deliberately deferred. */
export interface DuplicateGroupDto {
  imageHashes: string[];
  count: number;
}

/** One semantic-attraction edge for the Visualizer's force layout (twin of the
 * Rust `Neighbor`): a similar image and how strongly it should pull this one
 * toward it (`weight` is a cosine similarity in roughly [0,1], higher = more
 * alike; 0 = no pull). */
export interface Neighbor {
  hash: string;
  weight: number;
}

/** One in-scope image plus its top-k semantically-similar neighbors (twin of the
 * Rust `ImageNeighbors`) — the sparse k-NN graph the Visualizer's force sim reads
 * so CLIP/note-similar photos attract each other into clusters. */
export interface ImageNeighbors {
  hash: string;
  neighbors: Neighbor[];
}

/** `diversify_scope` result (DESIGN-DEDUP-AND-SIMILARITY.md, the
 * duplication-tolerance slider) — twin of the Rust `DiversifyReport` (camelCase
 * on the wire). The duplication-tolerance view filter answers, for a scope and a
 * single `tolerance ∈ [0,1]`, WHICH in-scope images are representatives (`shown`)
 * and which are redundant (`hidden`); `shown ∪ hidden` is exactly the scope. The
 * grid renders `shown` and folds `hidden` behind an unobtrusive count, so this is
 * a NON-DESTRUCTIVE display layer over the current scope, never a delete. */
export interface DiversifyReport {
  /** Representative image hashes the grid renders. */
  shown: string[];
  /** Redundant image hashes folded into a representative the grid hides. */
  hidden: string[];
  /** The cosine similarity cutoff the tolerance resolved to (two images at or
   * above it are treated as redundant) — surfaced for transparency/telemetry. */
  cutoff: number;
  /** True when no CLIP similarity signal existed (un-embedded / no model), so the
   * report is a trivial "all shown": the UI disables the slider and shows a calm
   * "embed to diversify" hint rather than implying the slider had no effect. */
  degraded: boolean;
}

/** image_abs_path result (D4: reveal / copy path / open-default). */
export interface ImagePathsDto {
  /** Best online absolute path, null when every path is offline. */
  absPath: string | null;
  relPath: string;
  volumeLabel: string | null;
  online: boolean;
}
