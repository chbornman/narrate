import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  AppSettings,
  CollectionDto,
  GridItem,
  IngestStatus,
  RootDto,
  RuntimeStatus,
  ScopeView,
  TopicDto,
} from "../src/lib/types/dto";

const backend = vi.hoisted(() => ({
  failed: new Set<string>(),
  unsupported: new Set<string>(),
  calls: [] as string[],
  rootsGate: null as Promise<unknown> | null,
  stateSnapshot: null as unknown,
}));

const root: RootDto = {
  rootId: "r1",
  displayName: "Pictures",
  relPath: "",
  volumeId: "v1",
  online: true,
  absPath: "/pictures",
  archived: false,
};
const newerRoot: RootDto = {
  ...root,
  rootId: "r2",
  displayName: "New volume",
  absPath: "/new-volume",
};

const item: GridItem = {
  hash: "h1",
  fileName: "one.jpg",
  relPath: "one.jpg",
  captureTs: null,
  addedTs: "2026-07-26T00:00:00Z",
  hasJournal: false,
  rating: null,
  offline: false,
};

const ingest: IngestStatus = {
  running: false,
  done: 4,
  total: 4,
  errors: 0,
  passes: [],
  scanning: false,
  discovered: 4,
  offlineVolumes: [],
  vectorsVersion: 3,
  imagesVersion: 4,
  journalVersion: 2,
};

const runtime: RuntimeStatus = {
  asrReady: false,
  llmReady: false,
  asrBlocked: null,
  llmBlocked: null,
  clipReady: true,
  textEmbedderReady: true,
  clip: { state: "ready", attemptId: 1, modelId: "clip", generation: 1, startedAt: "2026-01-01T00:00:00.000Z", error: null },
  textEmbedder: { state: "ready", attemptId: 2, modelId: "text", generation: 1, startedAt: "2026-01-01T00:00:00.000Z", error: null },
  capabilityState: "ready",
  capabilitySummary: null,
  capabilityAdapters: [],
  capabilityDetectedAt: null,
  tierDetected: 1,
  tierEffective: 1,
  tierOverriddenAbove: false,
  consent: "download",
  consentOfferBytes: 0,
  models: [],
  instanceLockHeld: true,
  controlFiles: [],
};

const settings: AppSettings = {
  lastExportTs: null,
  stackDisplay: "raw",
  externalEditor: null,
  previewCacheBudgetBytes: 20_000_000_000,
};

const collection: CollectionDto = {
  id: "c1",
  name: "Portfolio",
  description: "",
  status: "active",
  createdTs: "2026-07-26T00:00:00Z",
  updatedTs: "2026-07-26T00:00:00Z",
  memberCount: 1,
  noteCount: 0,
};

const topic: TopicDto = {
  id: "t1",
  phrase: "blue hour",
  space: "blend",
  createdTs: "2026-07-26T00:00:00Z",
};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    backend.calls.push(cmd);
    if (backend.unsupported.has(cmd))
      throw new Error(`unknown command: ${cmd}`);
    if (backend.failed.has(cmd)) throw new Error(`${cmd} unavailable`);
    switch (cmd) {
      case "bootstrap_status":
        return { state: "ready", error: null };
      case "application_state_snapshot":
        return backend.stateSnapshot;
      case "settings_get":
        return settings;
      case "list_roots":
        return backend.rootsGate ?? [root];
      case "list_archived_roots":
      case "folder_tree":
        return [];
      case "list_folder":
        return [item];
      case "ingest_status":
        return ingest;
      case "runtime_status":
        return runtime;
      case "list_collections":
        return [collection];
      case "list_topics":
        return [topic];
      case "set_scope":
        return {
          kind: "session",
          count: 0,
          previewHashes: [],
        } satisfies ScopeView;
      case "image_collection_ids":
        return [];
      default:
        return args === undefined ? null : null;
    }
  }),
  convertFileSrc: (path: string, protocol = "asset") =>
    `${protocol}://localhost/${path}`,
}));

import BootStatus from "../src/lib/components/shell/BootStatus.svelte";
import {
  Ui,
  type BootStatus as BootStatusValue,
  type BootSubsystem,
} from "../src/lib/state/app.svelte";

const commandFor: Record<Exclude<BootSubsystem, "events">, string> = {
  bootstrap: "bootstrap_status",
  settings: "settings_get",
  roots: "list_roots",
  "archived-roots": "list_archived_roots",
  folder: "list_folder",
  ingest: "ingest_status",
  runtime: "runtime_status",
  collections: "list_collections",
  topics: "list_topics",
};

beforeEach(() => {
  backend.failed.clear();
  backend.unsupported.clear();
  backend.calls.length = 0;
  backend.rootsGate = null;
  backend.stateSnapshot = null;
  localStorage.clear();
});

describe("frontend boot dependency settlement", () => {
  for (const subsystem of Object.keys(commandFor) as Exclude<
    BootSubsystem,
    "events"
  >[]) {
    it(`${subsystem} failure is explicit and retryable`, async () => {
      const command = commandFor[subsystem];
      backend.failed.add(command);
      const ui = new Ui();

      await ui.init();

      expect(ui.boot.phase).toBe(
        subsystem === "bootstrap" || subsystem === "roots" ? "fatal" : "degraded",
      );
      expect(ui.boot.failures.map((failure) => failure.subsystem)).toContain(subsystem);

      // Every independent success remains available even though one peer
      // failed. A bootstrap failure means the backend App does not exist, so
      // no App-owned command is admitted. A root failure is otherwise the
      // only case where its dependent folder cannot have loaded yet.
      if (subsystem !== "bootstrap") {
        if (subsystem !== "collections") expect(ui.collections).toEqual([collection]);
        if (subsystem !== "topics") expect(ui.topics).toEqual([topic]);
        if (subsystem !== "runtime") expect(ui.shell.runtime).toEqual(runtime);
        if (subsystem !== "ingest") expect(ui.shell.ingest).toEqual(ingest);
        if (subsystem !== "roots") expect(ui.roots).toEqual([root]);
      }

      backend.failed.delete(command);
      await ui.retryBoot();

      expect(ui.boot).toMatchObject({
        phase: "usable",
        retrying: false,
        failures: [],
      });
      expect(ui.grid.rawItems).toEqual([item]);
      expect(ui.grid.stackDisplay).toBe("raw");
    });
  }

  it("does not refetch successful snapshots while retrying a failed peer", async () => {
    backend.failed.add("runtime_status");
    const ui = new Ui();
    await ui.init();
    const rootsCalls = backend.calls.filter((cmd) => cmd === "list_roots").length;
    const collectionCalls = backend.calls.filter(
      (cmd) => cmd === "list_collections",
    ).length;

    backend.failed.delete("runtime_status");
    await ui.retryBoot();

    expect(backend.calls.filter((cmd) => cmd === "list_roots")).toHaveLength(
      rootsCalls,
    );
    expect(
      backend.calls.filter((cmd) => cmd === "list_collections"),
    ).toHaveLength(collectionCalls);
    expect(backend.calls.filter((cmd) => cmd === "runtime_status")).toHaveLength(2);
  });

  it("starts independent reads while roots and the initial folder are pending", async () => {
    let releaseRoots: (value: RootDto[]) => void = () => {};
    backend.rootsGate = new Promise<RootDto[]>((resolve) => {
      releaseRoots = resolve;
    });
    const ui = new Ui();
    const opening = ui.init();
    await vi.waitFor(() => {
      expect(backend.calls).toContain("list_roots");
    });

    expect(backend.calls).toEqual(
      expect.arrayContaining([
        "settings_get",
        "list_roots",
        "ingest_status",
        "runtime_status",
        "list_collections",
        "list_topics",
      ]),
    );
    expect(backend.calls).not.toContain("list_folder");

    releaseRoots([root]);
    await opening;
    expect(backend.calls).toContain("list_folder");
    expect(ui.boot.phase).toBe("usable");
  });

  it("never lets a slow cold root read overwrite a newer event snapshot", async () => {
    let releaseRoots: (value: RootDto[]) => void = () => {};
    backend.rootsGate = new Promise<RootDto[]>((resolve) => {
      releaseRoots = resolve;
    });
    const ui = new Ui();
    const opening = ui.init();
    await vi.waitFor(() => expect(backend.calls).toContain("list_roots"));

    await ui.onRootsChanged([newerRoot]);
    releaseRoots([root]);
    await opening;

    expect(ui.roots).toEqual([newerRoot]);
    expect(ui.grid.rootId).toBe(newerRoot.rootId);
  });

  it("treats a folder-tree failure as part of the retryable folder snapshot", async () => {
    backend.failed.add("folder_tree");
    const ui = new Ui();
    const prior = { ...item, hash: "prior", fileName: "prior.jpg" };
    ui.grid.setItems([prior]);
    await ui.init();
    expect(ui.boot).toMatchObject({
      phase: "degraded",
      failures: [
        { subsystem: "folder", message: "folder_tree unavailable" },
      ],
    });
    // Folder navigation is one product-state commit: a successful list read
    // cannot replace the current grid when its matching tree read failed.
    expect(ui.grid.rawItems).toEqual([prior]);

    backend.failed.delete("folder_tree");
    await ui.retryBoot();
    expect(ui.boot.phase).toBe("usable");
    expect(ui.grid.rawItems).toEqual([item]);
    expect(ui.tree).toEqual([]);
  });

  it("surfaces a failed live-event subscription as degraded boot health", async () => {
    const ui = new Ui();
    await ui.init();
    ui.eventListenersFailed(new Error("runtime-status listener unavailable"));
    expect(ui.boot.phase).toBe("degraded");
    expect(ui.boot.failures).toContainEqual({
      subsystem: "events",
      message: "runtime-status listener unavailable",
    });
    ui.eventListenersReady();
    expect(ui.boot.phase).toBe("usable");
  });

  it("accepts an explicitly unsupported archived-roots command for old backends", async () => {
    backend.unsupported.add("list_archived_roots");
    const ui = new Ui();

    await ui.init();

    expect(ui.boot.phase).toBe("usable");
    expect(ui.boot.failures).toEqual([]);
    expect(ui.archivedRootsSupported).toBe(false);
    expect(ui.archivedRoots).toEqual([]);
  });

  it("does not misreport a transient archived-roots failure as an empty healthy archive", async () => {
    backend.failed.add("list_archived_roots");
    const ui = new Ui();

    await ui.init();

    expect(ui.boot.phase).toBe("degraded");
    expect(ui.boot.failures).toContainEqual({
      subsystem: "archived-roots",
      message: "list_archived_roots unavailable",
    });
    expect(ui.archivedRootsSupported).not.toBe(false);

    backend.failed.delete("list_archived_roots");
    await ui.retryBoot();
    expect(ui.boot.phase).toBe("usable");
    expect(ui.archivedRootsSupported).toBe(true);
  });

  it("applies only newer backend domain revisions and catches up across an event gap", async () => {
    const ui = new Ui();
    await ui.init();
    backend.stateSnapshot = {
      revision: 2,
      revisions: {
        settings: 2,
        roots: 2,
        collections: 2,
        topics: 2,
        runtime: 2,
        previewCache: 2,
      },
      settings,
      roots: [newerRoot],
      archivedRoots: [],
      collections: [collection],
      topics: [topic],
      runtime,
      previewCache: {
        fullBytes: 0,
        fullFiles: 0,
        totalBytes: 0,
        budgetBytes: 20_000_000_000,
      },
    };
    await ui.catchUpApplicationState();
    expect(ui.roots).toEqual([newerRoot]);
    expect(ui.topics).toEqual([topic]);

    backend.stateSnapshot = {
      ...(backend.stateSnapshot as Record<string, unknown>),
      revision: 1,
      revisions: {
        settings: 1,
        roots: 1,
        collections: 1,
        topics: 1,
        runtime: 1,
        previewCache: 1,
      },
      roots: [root],
    };
    await ui.catchUpApplicationState();
    expect(ui.roots).toEqual([newerRoot]);

    backend.stateSnapshot = {
      ...(backend.stateSnapshot as Record<string, unknown>),
      revision: 4,
      revisions: {
        settings: 2,
        roots: 4,
        collections: 2,
        topics: 4,
        runtime: 4,
        previewCache: 2,
      },
      roots: [root],
      topics: [],
    };
    await ui.onApplicationStateChanged({
      revision: 4,
      domains: ["runtime"],
    });
    expect(ui.roots).toEqual([root]);
    expect(ui.topics).toEqual([]);
  });
});

describe("boot recovery surface", () => {
  const state = (
    phase: BootStatusValue["phase"],
    failures: BootStatusValue["failures"] = [],
  ): BootStatusValue => ({
    phase,
    attempt: 1,
    retrying: false,
    failures,
  });

  it("renders an honest cold-open state", () => {
    render(BootStatus, {
      status: state("loading"),
      onretry: vi.fn(),
    });
    expect(screen.getByRole("status").textContent).toContain("Opening your library");
  });

  it("renders a blocking fatal state and invokes retry", async () => {
    const retry = vi.fn();
    render(BootStatus, {
      status: state("fatal", [
        { subsystem: "roots", message: "database unavailable" },
      ]),
      onretry: retry,
    });
    expect(screen.getByRole("alert").textContent).toContain(
      "could not open the library",
    );
    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(retry).toHaveBeenCalledOnce();
  });

  it("offers a relaunch when the backend App could not be constructed", () => {
    render(BootStatus, {
      status: state("fatal", [
        { subsystem: "bootstrap", message: "database migration failed" },
      ]),
      onretry: vi.fn(),
    });
    expect(
      screen.getByRole("button", { name: "Relaunch Photoproof" }),
    ).toBeTruthy();
  });

  it("offers an explicit identity reset instead of a relaunch loop", () => {
    render(BootStatus, {
      status: state("fatal", [
        { subsystem: "bootstrap", message: "device identity unavailable" },
      ]),
      recoveryAction: "reset-device-identity",
      onretry: vi.fn(),
    });
    expect(
      screen.getByRole("button", {
        name: "Reset device identity and relaunch",
      }),
    ).toBeTruthy();
  });

  it("renders degraded dependencies without covering the usable shell", () => {
    render(BootStatus, {
      status: state("degraded", [
        { subsystem: "runtime", message: "runtime unavailable" },
        { subsystem: "topics", message: "topics unavailable" },
      ]),
      onretry: vi.fn(),
    });
    expect(screen.getByRole("status").textContent).toContain(
      "models and hardware, topics",
    );
    expect(
      screen.getByRole<HTMLButtonElement>("button", { name: "Retry" }).disabled,
    ).toBe(false);
  });
});
