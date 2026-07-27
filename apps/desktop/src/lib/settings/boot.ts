import type {
  AppSettings,
  PreviewCacheStatsDto,
  RootDto,
  RuntimeStatus,
} from "../types/dto";

export type SettingsDataSource = "roots" | "runtime" | "settings" | "cache";
export type SettingsBootSource = SettingsDataSource | "events";
export type SettingsBootPhase = "loading" | "ready" | "degraded" | "fatal";

export interface SettingsBootIssue {
  source: SettingsBootSource;
  message: string;
}

export interface SettingsBootState {
  phase: SettingsBootPhase;
  issues: SettingsBootIssue[];
  hasSnapshot: boolean;
  attempt: number;
}

export const initialSettingsBootState: SettingsBootState = {
  phase: "loading",
  issues: [],
  hasSnapshot: false,
  attempt: 0,
};

export interface SettingsBootReads {
  roots(): Promise<RootDto[]>;
  runtime(): Promise<RuntimeStatus>;
  settings(): Promise<AppSettings>;
  cache(): Promise<PreviewCacheStatsDto>;
}

export interface SettingsBootSink {
  roots(value: RootDto[]): void;
  runtime(value: RuntimeStatus): void;
  settings(value: AppSettings): void;
  cache(value: PreviewCacheStatsDto): void;
  state(value: SettingsBootState): void;
}

type Versions = Record<SettingsDataSource, number>;
type Snapshots = Record<SettingsDataSource, boolean>;

const SOURCES: SettingsDataSource[] = ["roots", "runtime", "settings", "cache"];

function blankVersions(): Versions {
  return { roots: 0, runtime: 0, settings: 0, cache: 0 };
}

function blankSnapshots(): Snapshots {
  return { roots: false, runtime: false, settings: false, cache: false };
}

function message(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

/**
 * Coordinates the Settings window's four independent cold reads.
 *
 * Each source has a generation. A live event advances it before applying its
 * payload, so an older cold response (or failure) cannot overwrite/degrade a
 * newer event-delivered snapshot. Refresh attempts are also monotone: results
 * from an older retry are ignored after a newer retry begins.
 */
export class SettingsBootController {
  private readonly versions = blankVersions();
  private readonly snapshots = blankSnapshots();
  private dataIssues: SettingsBootIssue[] = [];
  private eventIssue: SettingsBootIssue | null = null;
  private eventsReady = false;
  private refreshing = false;
  private phase: SettingsBootPhase = "loading";
  private attempt = 0;

  constructor(
    private readonly reads: SettingsBootReads,
    private readonly sink: SettingsBootSink,
  ) {}

  async refresh(): Promise<void> {
    const attempt = ++this.attempt;
    const startedAt = { ...this.versions };
    this.refreshing = true;
    this.phase = "loading";
    this.dataIssues = [];
    this.publish();

    // Promise.resolve().then also turns a synchronously thrown injected reader
    // into a settled rejection, keeping refresh itself non-throwing.
    const results = await Promise.allSettled([
      Promise.resolve().then(() => this.reads.roots()),
      Promise.resolve().then(() => this.reads.runtime()),
      Promise.resolve().then(() => this.reads.settings()),
      Promise.resolve().then(() => this.reads.cache()),
    ]);
    if (attempt !== this.attempt) return;

    const issues: SettingsBootIssue[] = [];
    const apply = <T>(
      source: SettingsDataSource,
      result: PromiseSettledResult<T>,
      write: (value: T) => void,
    ) => {
      // A newer live event owns this source. Ignore both the stale response and
      // its stale error: neither may roll back or degrade newer truth.
      if (this.versions[source] !== startedAt[source]) return;
      if (result.status === "fulfilled") {
        write(result.value);
        this.snapshots[source] = true;
      } else {
        issues.push({ source, message: message(result.reason) });
      }
    };

    apply("roots", results[0], this.sink.roots);
    apply("runtime", results[1], this.sink.runtime);
    apply("settings", results[2], this.sink.settings);
    apply("cache", results[3], this.sink.cache);

    this.dataIssues = issues;
    this.refreshing = false;
    this.recompute();
  }

  liveRoots(value: RootDto[]): void {
    this.live("roots", () => this.sink.roots(value));
  }

  liveRuntime(value: RuntimeStatus): void {
    this.live("runtime", () => this.sink.runtime(value));
  }

  liveSettings(value: AppSettings): void {
    this.live("settings", () => this.sink.settings(value));
  }

  liveCache(value: PreviewCacheStatsDto): void {
    this.live("cache", () => this.sink.cache(value));
  }

  /** Listener installation is a first-class boot dependency. A rejected
   * subscription leaves the window stale, so it remains degraded and
   * retryable even when all four cold reads succeeded. */
  listenersFailed(error: unknown): void {
    this.eventsReady = false;
    this.eventIssue = { source: "events", message: message(error) };
    this.recompute();
  }

  listenersReady(): void {
    this.eventsReady = true;
    this.eventIssue = null;
    this.recompute();
  }

  private live(source: SettingsDataSource, write: () => void): void {
    this.versions[source] += 1;
    this.snapshots[source] = true;
    write();
    this.dataIssues = this.dataIssues.filter((issue) => issue.source !== source);
    this.recompute();
  }

  private hasSnapshot(): boolean {
    return SOURCES.some((source) => this.snapshots[source]);
  }

  private recompute(): void {
    if (this.refreshing) {
      this.phase = "loading";
    } else {
      const issues = this.issues();
      this.phase =
        issues.length > 0
          ? this.hasSnapshot()
            ? "degraded"
            : "fatal"
          : this.eventsReady
            ? "ready"
            : "loading";
    }
    this.publish();
  }

  private issues(): SettingsBootIssue[] {
    return this.eventIssue === null
      ? [...this.dataIssues]
      : [...this.dataIssues, this.eventIssue];
  }

  private publish(): void {
    this.sink.state({
      phase: this.phase,
      issues: this.issues(),
      hasSnapshot: this.hasSnapshot(),
      attempt: this.attempt,
    });
  }
}
