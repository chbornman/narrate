import type { ApplicationHealth } from "../types/dto";

export type SettingsTab = "library" | "appearance" | "models" | "system";

export const SETTINGS_TABS: readonly {
  id: SettingsTab;
  label: string;
  description: string;
}[] = [
  {
    id: "library",
    label: "Library",
    description: "Folders, background processing, file behavior, and preview storage.",
  },
  {
    id: "appearance",
    label: "Appearance",
    description: "Interface theme, interface size, and the backdrop around photographs.",
  },
  {
    id: "models",
    label: "Models",
    description: "Local AI files, hardware acceleration, verification, and runtime status.",
  },
  {
    id: "system",
    label: "System",
    description: "Diagnostics, application updates, backups, recovery, and local attention data.",
  },
];

export function parseSettingsTab(value: string | null): SettingsTab {
  return SETTINGS_TABS.some((tab) => tab.id === value)
    ? (value as SettingsTab)
    : "library";
}

export type HealthIssue = ApplicationHealth["issues"][number];

/** Health belongs beside the control that can resolve it. This prevents a
 * duplicate generic health page from becoming the only place a model/folder
 * failure is understandable. */
export function settingsTabForHealthIssue(issue: HealthIssue): SettingsTab {
  if (
    issue.subsystem === "models" ||
    issue.subsystem === "runtime" ||
    issue.id === "disk:models"
  ) {
    return "models";
  }
  if (
    issue.subsystem === "volumes" ||
    issue.subsystem === "roots" ||
    issue.subsystem === "background-work" ||
    issue.subsystem === "cache"
  ) {
    return "library";
  }
  return "system";
}

/** Blocking issues lead, then preserve the backend's stable issue ordering. */
export function firstHealthSettingsTab(issues: HealthIssue[]): SettingsTab {
  const first = issues.find((issue) => issue.blocking) ?? issues[0];
  return first === undefined ? "system" : settingsTabForHealthIssue(first);
}
