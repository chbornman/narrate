import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";

const backend = vi.hoisted(() => ({
  failures: new Set<string>(),
  listenerFailures: new Set<string>(),
  updatesEnabled: false,
  health: null as Record<string, unknown> | null,
  openPath: null as string | null,
  savePath: null as string | null,
  runtimeModels: [] as Record<string, unknown>[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string) => {
    if (backend.failures.has(command)) throw new Error(`${command} unavailable`);
    switch (command) {
      case "list_roots":
        return [
          {
            rootId: "root-1",
            displayName: "Recovered photos",
            relPath: "",
            volumeId: "volume-1",
            online: true,
            absPath: "/photos",
            archived: false,
          },
        ];
      case "runtime_status":
      case "runtime_verify_model":
      case "runtime_select_model":
        return {
          asrReady: false,
          llmReady: false,
          asrBlocked: null,
          llmBlocked: null,
          clipReady: false,
          textEmbedderReady: false,
          clip: { state: "idle", attemptId: null, modelId: null, generation: 0, startedAt: null, error: null },
          textEmbedder: { state: "idle", attemptId: null, modelId: null, generation: 0, startedAt: null, error: null },
          capabilityState: "ready",
          capabilitySummary: null,
          capabilityAdapters: [],
          capabilityDetectedAt: null,
          tierDetected: 1,
          tierEffective: 1,
          tierOverriddenAbove: false,
          consent: "undecided",
          consentOfferBytes: 0,
          models: backend.runtimeModels,
          instanceLockHeld: true,
          controlFiles: [],
        };
      case "settings_get":
      case "set_processing_policy":
        return {
          lastExportTs: null,
          stackDisplay: "jpeg",
          externalEditor: null,
          previewCacheBudgetBytes: 20 * 1024 ** 3,
        };
      case "preview_cache_stats":
        return {
          fullBytes: 0,
          fullFiles: 0,
          totalBytes: 0,
          budgetBytes: 20 * 1024 ** 3,
        };
      case "update_status":
        return {
          enabled: backend.updatesEnabled,
          currentVersion: "0.1.0",
          phase: backend.updatesEnabled ? "idle" : "disabled",
          available: null,
          downloadedBytes: 0,
          totalBytes: null,
          error: null,
        };
      case "update_check":
        return {
          enabled: true,
          currentVersion: "0.1.0",
          phase: "available",
          available: {
            version: "0.1.1",
            currentVersion: "0.1.0",
            notes: "Startup and cache reliability fixes.",
            publishedAt: "2026-07-27T12:00:00Z",
          },
          downloadedBytes: 0,
          totalBytes: null,
          error: null,
        };
      case "update_install":
        return null;
      case "application_health":
        return backend.health;
      default:
        return null;
    }
  }),
  convertFileSrc: (path: string) => path,
}));

vi.mock("@tauri-apps/api/event", () => ({
  emit: vi.fn(async () => {}),
  listen: vi.fn(async (event: string) => {
    if (backend.listenerFailures.has(event))
      throw new Error(`${event} subscription unavailable`);
    return () => {};
  }),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    setTitle: async () => {},
    close: async () => {},
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => backend.openPath),
  save: vi.fn(async () => backend.savePath),
}));

import SettingsApp from "../src/lib/settings/SettingsApp.svelte";
import { invoke } from "@tauri-apps/api/core";

beforeEach(() => {
  backend.failures.clear();
  backend.listenerFailures.clear();
  backend.updatesEnabled = false;
  backend.health = null;
  backend.openPath = null;
  backend.savePath = null;
  backend.runtimeModels = [];
  vi.mocked(invoke).mockClear();
});

async function openSettingsTab(name: "Library" | "Appearance" | "Models" | "System") {
  await fireEvent.click(await screen.findByRole("tab", { name }));
}

const runtimeModelFixture = (
  id: string,
  over: Record<string, unknown> = {},
): Record<string, unknown> => ({
  id,
  role: "embedder",
  defaultOffer: true,
  advancedAvailable: false,
  compatible: true,
  compatibilityReason: "CPU-compatible model",
  compatibleProviders: ["CPU"],
  consumers: [],
  state: "not-downloaded",
  totalBytes: 100,
  downloadedBytes: 0,
  licenseName: "Fixture license",
  licenseUrl: "https://example.test/license",
  acceptanceRequired: false,
  accepted: true,
  error: null,
  retryHint: null,
  operation: null,
  operationEvent: null,
  registryError: null,
  ...over,
});

describe("Settings window boot states", () => {
  it("groups the long settings surface into keyboard-navigable tabs", async () => {
    render(SettingsApp);

    const tabs = await screen.findAllByRole("tab");
    expect(tabs.map((tab) => tab.textContent?.trim())).toEqual([
      "Library",
      "Appearance",
      "Models",
      "System",
    ]);
    expect(screen.getByRole("tab", { name: "Library" }).getAttribute("aria-selected")).toBe(
      "true",
    );
    expect(screen.getByRole("heading", { name: "Watched folders" })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Models" })).toBeNull();

    await openSettingsTab("Appearance");
    expect(screen.getByRole("heading", { name: "Appearance" })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Watched folders" })).toBeNull();

    await fireEvent.keyDown(screen.getByRole("tab", { name: "Appearance" }), {
      key: "ArrowRight",
    });
    expect(screen.getByRole("tab", { name: "Models" }).getAttribute("aria-selected")).toBe(
      "true",
    );
    expect(screen.getByRole("heading", { name: "Models" })).toBeTruthy();
  });

  it("explains background work in user-facing terms and exposes truthful toggle state", async () => {
    render(SettingsApp);

    expect(await screen.findByText("Background work")).toBeTruthy();
    expect(
      screen.getByRole("option", { name: "Balanced - recommended" }),
    ).toBeTruthy();
    expect(
      screen.getAllByRole("option", { name: "Watch without scanning" }),
    ).toHaveLength(2);
    expect(screen.getByText("Semantic search from notes")).toBeTruthy();
    expect(screen.getByText("Visual search from photos")).toBeTruthy();
    expect(
      screen.getByText(/Photos on screen are generated and loaded before off-screen runway/),
    ).toBeTruthy();

    const automatic = screen
      .getByText("Automatic background processing")
      .closest(".row");
    expect(automatic?.querySelector("button")?.getAttribute("aria-pressed")).toBe("true");
    expect(automatic?.querySelector("button")?.textContent?.trim()).toBe("On");
  });

  it("keeps successful sections usable through a partial failure and retry", async () => {
    backend.failures.add("runtime_status");
    render(SettingsApp);

    expect(await screen.findByText("Some settings are temporarily unavailable.")).toBeTruthy();
    expect(screen.getByText("Recovered photos")).toBeTruthy();
    expect(screen.getByText(/runtime: runtime_status unavailable/)).toBeTruthy();

    backend.failures.clear();
    await fireEvent.click(screen.getByRole("button", { name: "Retry unavailable sections" }));
    await waitFor(() => {
      expect(screen.queryByText("Some settings are temporarily unavailable.")).toBeNull();
    });
    await openSettingsTab("Models");
    expect(screen.getByText(/Hardware tier:/).textContent).toContain("Hardware tier: 1");
  });

  it("renders a fatal, inert state when every read fails and recovers on retry", async () => {
    for (const command of [
      "list_roots",
      "runtime_status",
      "settings_get",
      "preview_cache_stats",
    ]) {
      backend.failures.add(command);
    }
    const { container } = render(SettingsApp);

    expect(await screen.findByText("Settings could not be loaded.")).toBeTruthy();
    expect(container.querySelector(".settings-body")?.classList.contains("blocked")).toBe(true);

    backend.failures.clear();
    await fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Recovered photos")).toBeTruthy();
    await waitFor(() => {
      expect(container.querySelector(".settings-body")?.classList.contains("blocked")).toBe(false);
    });
  });

  it("keeps signed updates manual and requires exact-version confirmation", async () => {
    backend.updatesEnabled = true;
    render(SettingsApp);
    await openSettingsTab("System");

    const check = await screen.findByRole("button", { name: "Check for updates" });
    await fireEvent.click(check);
    expect(await screen.findByText("Version 0.1.1")).toBeTruthy();
    expect(screen.getByText("Startup and cache reliability fixes.")).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: "Review and install" }));
    await fireEvent.click(screen.getByRole("button", { name: "Install and restart" }));
    expect(invoke).toHaveBeenCalledWith("update_install", {
      expectedVersion: "0.1.1",
    });
  });

  it("labels journal portability honestly and requires confirmation for offline recovery", async () => {
    backend.savePath = "/backups/Photoproof Backup.ppbackup";
    render(SettingsApp);
    await openSettingsTab("System");

    expect(
      await screen.findByText(/Journal export includes sidecars, collections, saved topic phrases/),
    ).toBeTruthy();
    await fireEvent.click(
      screen.getByRole("button", { name: "Back up complete app state…" }),
    );
    await fireEvent.click(screen.getByRole("button", { name: "Back up and quit" }));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("backup_and_quit", {
        destination: "/backups/Photoproof Backup.ppbackup",
      });
    });

    backend.openPath = "/backups/Older.ppbackup";
    await fireEvent.click(
      screen.getByRole("button", { name: "Restore complete app state…" }),
    );
    expect(
      screen.getByText(/retain the current data directory as a rollback copy/),
    ).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Restore and restart" }));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("restore_and_restart", {
        backup: "/backups/Older.ppbackup",
      });
    });
  });

  it("surfaces and retries a failed live-event subscription", async () => {
    backend.listenerFailures.add("runtime-status");
    render(SettingsApp);

    expect(
      await screen.findByText("Some settings are temporarily unavailable."),
    ).toBeTruthy();
    expect(
      screen.getByText(/events: runtime-status: runtime-status subscription unavailable/),
    ).toBeTruthy();

    backend.listenerFailures.clear();
    await fireEvent.click(
      screen.getByRole("button", { name: "Retry unavailable sections" }),
    );
    await waitFor(() => {
      expect(
        screen.queryByText("Some settings are temporarily unavailable."),
      ).toBeNull();
    });
  });

  it("turns a rejected mutation into one retryable action state", async () => {
    backend.failures.add("remove_root");
    render(SettingsApp);
    const remove = await screen.findByRole("button", { name: "Remove" });

    await fireEvent.click(remove);
    const confirmations = screen.getAllByRole("button", { name: "Remove" });
    await fireEvent.click(confirmations.at(-1)!);
    await waitFor(() =>
      expect(
        vi
          .mocked(invoke)
          .mock.calls.some(([command]) => command === "remove_root"),
      ).toBe(true),
    );
    expect(
      await screen.findByText(
        "Removing folder failed: remove_root unavailable",
      ),
    ).toBeTruthy();

    backend.failures.delete("remove_root");
    await fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => {
      expect(
        screen.queryByText("Removing folder failed: remove_root unavailable"),
      ).toBeNull();
    });
    expect(
      vi
        .mocked(invoke)
        .mock.calls.filter(([command]) => command === "remove_root"),
    ).toHaveLength(2);
  });

  it("renders every backend model lifecycle and compatibility case without frontend inference", async () => {
    backend.runtimeModels = [
      runtimeModelFixture("failed-model", {
        state: "failed",
        error: "checksum mismatch",
      }),
      runtimeModelFixture("cancelled-model", {
        state: "cancelled",
        downloadedBytes: 25,
        operationEvent: {
          attemptId: "cancelled-attempt",
          sequence: 4,
          phase: "cancelled",
          terminal: true,
          error: null,
        },
      }),
      runtimeModelFixture("installing-model", {
        state: "installing",
        downloadedBytes: 100,
        operation: "installing",
        operationEvent: {
          attemptId: "install-attempt",
          sequence: 8,
          phase: "installing",
          terminal: false,
          error: null,
        },
      }),
      runtimeModelFixture("retrying-model", {
        state: "downloading",
        downloadedBytes: 60,
        operation: "downloading",
        retryHint: "connection interrupted, retrying (attempt 2 of 4)",
      }),
      runtimeModelFixture("provider-fallback-model", {
        state: "installed",
        downloadedBytes: 100,
        compatibleProviders: ["CPU", "CUDA"],
        consumers: [
          {
            role: "clip",
            desired: true,
            active: true,
            state: "failed",
            retryable: true,
            error: "CUDA provider initialization failed",
            requestedProvider: "CUDA",
            actualProvider: "CPU",
            fallbackReason: "graph fell back to CPU",
          },
        ],
      }),
      runtimeModelFixture("advanced-model", {
        defaultOffer: false,
        advancedAvailable: true,
        compatibleProviders: ["CoreML"],
      }),
      runtimeModelFixture("unsupported-model", {
        defaultOffer: false,
        compatible: false,
        compatibilityReason: "requires a matching Metal/CoreML runtime",
        compatibleProviders: [],
        state: "not-offered",
      }),
    ];

    render(SettingsApp);
    await openSettingsTab("Models");

    expect(await screen.findAllByText("failed-model")).toHaveLength(2);
    expect(screen.getByText(/checksum mismatch/)).toBeTruthy();
    expect(screen.getAllByText("cancelled-model")).toHaveLength(2);
    expect(screen.getAllByText("installing-model")).toHaveLength(2);
    expect(screen.getAllByText("retrying-model")).toHaveLength(2);
    expect(screen.getByText(/60% downloaded/)).toBeTruthy();
    expect(
      screen.getByText(/connection interrupted, retrying \(attempt 2 of 4\)/),
    ).toBeTruthy();
    expect(screen.getByText(/clip failed/)).toBeTruthy();
    expect(
      screen.getByText(
        /actual provider CPU \(requested CUDA\); graph fell back to CPU/,
      ),
    ).toBeTruthy();
    expect(screen.getByText(/CUDA provider initialization failed/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Download" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Resume download" })).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Pause" })).toHaveLength(2);

    await fireEvent.click(screen.getByRole("button", { name: "Show all model options" }));
    expect(await screen.findAllByText("advanced-model")).toHaveLength(2);
    expect(screen.getByText("Optional")).toBeTruthy();
    expect(screen.getByText("CoreML")).toBeTruthy();
    expect(screen.getAllByText("unsupported-model")).toHaveLength(2);
    expect(screen.getByText("Unavailable")).toBeTruthy();
    expect(
      screen.getByText(/requires a matching Metal\/CoreML runtime/),
    ).toBeTruthy();
  });

  it("renders backend health severity and invokes its targeted safe action", async () => {
    backend.health = {
      observedAtMs: 1_700_000_000_000,
      phase: "ready",
      issues: [
        {
          id: "model:clip",
          subsystem: "models",
          title: "Image search model needs verification",
          blocking: false,
          summary: "Installed bytes no longer match the manifest.",
          lastError: "checksum mismatch",
          lastErrorAtMs: 1_699_999_000_000,
          action: {
            kind: "verify-model",
            label: "Verify model",
            targetId: "clip",
          },
        },
      ],
      diagnostics: {
        buildVersion: "0.1.0",
        previousUncleanLaunch: false,
        logsDir: "/logs",
        currentLog: "/logs/current.log",
        error: null,
      },
    };
    render(SettingsApp);
    await openSettingsTab("Models");

    expect(
      await screen.findByText("Image search model needs verification"),
    ).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "Verify model" }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("runtime_verify_model", {
        modelId: "clip",
      });
    });
  });

  it("groups models by expertise and activates an installed alternative", async () => {
    backend.runtimeModels = [
      runtimeModelFixture("small-understanding", {
        role: "llm",
        state: "installed",
        downloadedBytes: 100,
      }),
      runtimeModelFixture("large-understanding", {
        role: "llm-alt",
        defaultOffer: false,
        advancedAvailable: true,
        state: "installed",
        downloadedBytes: 100,
      }),
      runtimeModelFixture("annotation-embedder", {
        role: "text-embedder",
        state: "installed",
        downloadedBytes: 100,
      }),
    ];

    render(SettingsApp);
    await openSettingsTab("Models");

    expect(await screen.findByText("Photo understanding")).toBeTruthy();
    expect(screen.getByText("Annotation search")).toBeTruthy();
    expect(screen.getAllByText("In use")).toHaveLength(2);
    await fireEvent.click(screen.getByRole("button", { name: "Use this model" }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("runtime_select_model", {
        modelId: "large-understanding",
      });
    });
  });
});
