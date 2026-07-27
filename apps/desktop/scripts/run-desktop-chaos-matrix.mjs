#!/usr/bin/env node

/**
 * A25 executable desktop lifecycle chaos matrix.
 *
 * This runner deliberately invokes the real Rust/Vitest acceptance binaries.
 * It emits one JSON record per backlog scenario with phase, host platform,
 * invariant results, and the exact suites that supplied the evidence. Cases
 * that require a real hard mount, suspend cycle, accelerator, or installed
 * shell remain explicit platform drills instead of being mislabeled by a
 * cooperative in-process mock.
 *
 * Usage:
 *   node apps/desktop/scripts/run-desktop-chaos-matrix.mjs
 *   node apps/desktop/scripts/run-desktop-chaos-matrix.mjs --suite archived_roots
 *   node apps/desktop/scripts/run-desktop-chaos-matrix.mjs --list
 */

import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "../../..");

const suites = {
  desktop_lib: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-desktop",
      "--lib",
    ],
    cwd: repoRoot,
  },
  core_lib: {
    command: ["cargo", "test", "-p", "photoproof-core", "--lib"],
    cwd: repoRoot,
  },
  archived_roots: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "library_acceptance",
      "archive_root_",
    ],
    cwd: repoRoot,
  },
  library_acceptance: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "library_acceptance",
    ],
    cwd: repoRoot,
  },
  library_watcher: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "library_watcher",
    ],
    cwd: repoRoot,
  },
  library_orphans: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "library_orphan_retention",
    ],
    cwd: repoRoot,
  },
  runtime_download: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "runtime_download",
    ],
    cwd: repoRoot,
  },
  runtime_supervisor: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "runtime_supervisor",
    ],
    cwd: repoRoot,
  },
  runtime_process: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "runtime_process",
    ],
    cwd: repoRoot,
    platforms: ["linux", "darwin"],
  },
  sidecars: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "sidecars_acceptance",
    ],
    cwd: repoRoot,
  },
  collections: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "collections_acceptance",
    ],
    cwd: repoRoot,
  },
  kill9_recovery: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-core",
      "--test",
      "m1_e2e_kill9",
      "c3_kill9_process_harness",
    ],
    cwd: repoRoot,
    platforms: ["linux", "darwin"],
  },
  connectors: {
    command: [
      "cargo",
      "test",
      "-p",
      "photoproof-connectors",
      "--lib",
    ],
    cwd: repoRoot,
  },
  frontend_boot: {
    command: [
      "bun",
      "x",
      "vitest",
      "run",
      "tests/boot-state.test.ts",
      "tests/settings-boot.test.ts",
      "tests/settings-window-boot.test.ts",
    ],
    cwd: resolve(repoRoot, "apps/desktop"),
  },
  frontend_runtime: {
    command: [
      "bun",
      "x",
      "vitest",
      "run",
      "tests/runtime-status.test.ts",
      "tests/librarystatus.test.ts",
    ],
    cwd: resolve(repoRoot, "apps/desktop"),
  },
};

const invariants = {
  usable: "window-usable-independent-of-optional-work",
  authored: "authored-truth-preserved",
  derived: "derived-state-valid-or-repended",
  terminal: "work-terminal-or-paused",
  backend: "ui-projects-committed-backend-truth",
  owned: "no-unowned-child-or-writer-after-clean-shutdown",
};

const cases = [
  // Startup.
  {
    id: "startup-unavailable-hard-nas",
    phase: "startup",
    scenario: "unavailable or hard NAS",
    suites: ["desktop_lib", "library_acceptance"],
    invariants: ["usable", "authored", "terminal"],
    platformDrill:
      "A real kernel-blocked NAS mount cannot be faithfully produced by an in-process fixture; run the installed-shell hard-mount drill.",
  },
  {
    id: "startup-unplugged-external-drive",
    phase: "startup",
    scenario: "unplugged external drive",
    suites: ["library_acceptance", "desktop_lib"],
    invariants: ["usable", "authored", "backend"],
  },
  {
    id: "startup-corrupt-control-files",
    phase: "startup",
    scenario: "corrupt settings, config, and installed registry",
    suites: ["desktop_lib", "core_lib"],
    invariants: ["usable", "authored", "backend"],
  },
  {
    id: "startup-newer-db",
    phase: "startup",
    scenario: "database created by a newer application",
    suites: ["core_lib", "frontend_boot"],
    invariants: ["authored", "backend"],
  },
  {
    id: "startup-interrupted-migration",
    phase: "startup",
    scenario: "interrupted migration",
    suites: ["core_lib"],
    invariants: ["authored", "derived"],
  },
  {
    id: "startup-full-disk",
    phase: "startup",
    scenario: "full or critically low disk",
    suites: ["desktop_lib", "runtime_download"],
    invariants: ["usable", "authored", "terminal", "backend"],
  },
  {
    id: "startup-missing-child-or-runtime",
    phase: "startup",
    scenario: "missing child binaries or runtime libraries",
    suites: ["desktop_lib", "runtime_supervisor"],
    invariants: ["usable", "terminal", "backend", "owned"],
  },
  {
    id: "startup-slow-hung-hardware-probe",
    phase: "startup",
    scenario: "slow or hung hardware probe",
    suites: ["desktop_lib"],
    invariants: ["usable", "terminal", "backend", "owned"],
  },
  {
    id: "startup-corrupt-model",
    phase: "startup",
    scenario: "corrupt or same-size model file",
    suites: ["desktop_lib", "runtime_download"],
    invariants: ["derived", "terminal", "backend"],
  },

  // Live transitions.
  {
    id: "live-root-lifecycle",
    phase: "live",
    scenario: "add, archive, remove, and re-add roots",
    suites: ["archived_roots", "desktop_lib"],
    invariants: ["authored", "derived", "terminal", "backend"],
  },
  {
    id: "live-volume-cycle",
    phase: "live",
    scenario: "volume offline then online",
    suites: ["library_acceptance", "sidecars"],
    invariants: ["authored", "derived", "terminal", "backend"],
  },
  {
    id: "live-watcher-overflow",
    phase: "live",
    scenario: "watcher overflow and reconcile",
    suites: ["library_watcher"],
    invariants: ["authored", "derived", "terminal"],
  },
  {
    id: "live-sleep-resume",
    phase: "live",
    scenario: "sleep then resume",
    suites: ["desktop_lib", "library_acceptance"],
    invariants: ["derived", "terminal"],
    platformDrill:
      "The fake wall-gap and resume-reconcile path are automated; an actual OS suspend/resume remains an installed-shell platform drill.",
  },
  {
    id: "live-model-install-cycle",
    phase: "live",
    scenario: "model download, remove, and reinstall",
    suites: ["desktop_lib", "runtime_download"],
    invariants: ["derived", "terminal", "backend"],
  },
  {
    id: "live-tier-config-change",
    phase: "live",
    scenario: "tier and runtime config changes",
    suites: ["desktop_lib", "runtime_supervisor"],
    invariants: ["derived", "terminal", "backend", "owned"],
  },
  {
    id: "live-gpu-fallback",
    phase: "live",
    scenario: "requested accelerator falls back",
    suites: ["connectors", "desktop_lib", "frontend_runtime"],
    invariants: ["terminal", "backend"],
    platformDrill:
      "Forced provider fixtures are automated; actual CoreML/CUDA/TensorRT selection requires the matching founder hardware.",
  },
  {
    id: "live-runtime-crash-restart",
    phase: "live",
    scenario: "runtime exhausts crash budget then restarts",
    suites: ["runtime_supervisor", "runtime_process", "desktop_lib"],
    invariants: ["terminal", "backend", "owned"],
  },
  {
    id: "live-cache-deletion",
    phase: "live",
    scenario: "preview or vector cache deleted",
    suites: ["archived_roots", "library_acceptance", "library_orphans"],
    invariants: ["authored", "derived", "terminal"],
  },
  {
    id: "live-multi-window-mutation",
    phase: "live",
    scenario: "concurrent main and settings window mutations",
    suites: ["desktop_lib", "frontend_boot", "frontend_runtime"],
    invariants: ["authored", "backend"],
    platformDrill:
      "Mock-runtime broadcasts and stale-snapshot arbitration are automated; a two-native-webview smoke remains an installed-shell drill.",
  },

  // Shutdown and crash.
  ...[
    ["scan", ["desktop_lib", "library_acceptance", "kill9_recovery"]],
    ["queue", ["desktop_lib", "library_acceptance", "kill9_recovery"]],
    ["doctor", ["desktop_lib", "library_acceptance", "library_orphans"]],
    ["model-build", ["desktop_lib"]],
    ["download", ["desktop_lib", "runtime_download"]],
    ["sidecar-flush", ["sidecars", "kill9_recovery"]],
    ["collection-flush", ["collections"]],
    ["wal-checkpoint", ["core_lib", "desktop_lib", "kill9_recovery"]],
  ].map(([work, caseSuites]) => ({
    id: `shutdown-during-${work}`,
    phase: "shutdown-crash",
    scenario: `quit or crash during ${work}`,
    suites: caseSuites,
    invariants: ["authored", "derived", "terminal", "owned"],
    platformDrill:
      work === "model-build"
        ? "The deterministic embedder state-machine covers timeout, shutdown, and stale landing; a truly wedged native ORT constructor still requires the native-process isolation/platform drill."
        : undefined,
  })),

  // The cross-cutting archived-root contract is separate so regressions cannot
  // hide behind the generic live-root row.
  {
    id: "archived-root-contract",
    phase: "live",
    scenario:
      "archived roots across search, burden, watcher ownership, repair, stale inference, and ingest",
    suites: ["archived_roots", "desktop_lib"],
    invariants: ["authored", "derived", "terminal", "backend"],
  },
];

function hostPlatform() {
  if (process.platform === "win32") return "windows";
  if (process.platform === "darwin") return "macos";
  return "linux";
}

function usageError(message) {
  process.stderr.write(`${message}\n`);
  process.exit(2);
}

const args = process.argv.slice(2);
const listOnly = args.includes("--list");
let selectedSuite = null;
const suiteArg = args.indexOf("--suite");
if (suiteArg !== -1) {
  selectedSuite = args[suiteArg + 1];
  if (!selectedSuite || !suites[selectedSuite]) {
    usageError(`unknown or missing suite: ${selectedSuite ?? ""}`);
  }
}

if (listOnly) {
  process.stdout.write(
    `${JSON.stringify(
      {
        schema: 1,
        platform: hostPlatform(),
        suites: Object.keys(suites),
        cases: cases.map(({ id, phase, scenario, suites: evidenceSuites }) => ({
          id,
          phase,
          scenario,
          suites: evidenceSuites,
        })),
      },
      null,
      2,
    )}\n`,
  );
  process.exit(0);
}

const suiteNames = selectedSuite
  ? [selectedSuite]
  : [...new Set(cases.flatMap((testCase) => testCase.suites))];
const suiteResults = {};

for (const name of suiteNames) {
  const suite = suites[name];
  const supported =
    !suite.platforms || suite.platforms.includes(process.platform);
  if (!supported) {
    suiteResults[name] = {
      status: "platform-not-applicable",
      command: suite.command,
      elapsedMs: 0,
    };
    continue;
  }
  const started = Date.now();
  process.stderr.write(
    `[A25] ${name}: ${suite.command.map((part) => JSON.stringify(part)).join(" ")}\n`,
  );
  const result = spawnSync(suite.command[0], suite.command.slice(1), {
    cwd: suite.cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    env: process.env,
  });
  suiteResults[name] = {
    status: result.status === 0 ? "passed" : "failed",
    command: suite.command,
    elapsedMs: Date.now() - started,
    exitCode: result.status,
    signal: result.signal,
    stdoutTail: (result.stdout ?? "").split("\n").slice(-12).join("\n"),
    stderrTail: (result.stderr ?? "").split("\n").slice(-12).join("\n"),
  };
}

const caseResults = cases.map((testCase) => {
  const evidence = testCase.suites.map((name) => ({
    suite: name,
    status: suiteResults[name]?.status ?? "not-run",
  }));
  const failed = evidence.some(({ status }) => status === "failed");
  const allPassed = evidence.every(({ status }) => status === "passed");
  const somePassed = evidence.some(({ status }) => status === "passed");
  let status = failed
    ? "failed"
    : allPassed
      ? "passed"
      : somePassed
        ? "partial"
        : "not-run";
  if (testCase.platformDrill && status === "passed") {
    status = "fixture-passed-platform-drill-pending";
  }
  return {
    id: testCase.id,
    phase: testCase.phase,
    platform: hostPlatform(),
    scenario: testCase.scenario,
    status,
    invariants: testCase.invariants.map((key) => ({
      id: invariants[key],
      status,
    })),
    evidence,
    platformDrill: testCase.platformDrill ?? null,
  };
});

const report = {
  schema: 1,
  generatedAt: new Date().toISOString(),
  platform: hostPlatform(),
  selectedSuite,
  suites: suiteResults,
  cases: caseResults,
};
process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
process.exit(
  Object.values(suiteResults).some(({ status }) => status === "failed") ? 1 : 0,
);
