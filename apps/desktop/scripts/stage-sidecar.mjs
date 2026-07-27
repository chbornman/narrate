import { copyFileSync, mkdirSync, statSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const desktopDir = resolve(scriptDir, "..");
const workspaceDir = resolve(desktopDir, "../..");
const rustc = spawnSync("rustc", ["-vV"], {
  cwd: workspaceDir,
  encoding: "utf8",
});
const verboseVersion = rustc.stdout ?? "";
const host =
  process.env.TAURI_ENV_TARGET_TRIPLE?.trim() ||
  verboseVersion
    .split(/\r?\n/)
    .find((line) => line.startsWith("host: "))
    ?.slice("host: ".length)
    .trim();

if (!host) {
  throw new Error(
    `could not determine Rust target: ${rustc.error?.message ?? rustc.stderr}`,
  );
}

const executableSuffix = host.includes("windows") ? ".exe" : "";
const profile = process.env.PROFILE === "debug" ? "debug" : "release";
const source = join(
  workspaceDir,
  "target",
  profile,
  `pp-asr-server${executableSuffix}`,
);
const destinationDir = join(desktopDir, "src-tauri", "binaries");
const destination = join(
  destinationDir,
  `pp-asr-server-${host}${executableSuffix}`,
);

statSync(source);
mkdirSync(destinationDir, { recursive: true });
copyFileSync(source, destination);
console.log(`staged ${source} -> ${destination}`);
