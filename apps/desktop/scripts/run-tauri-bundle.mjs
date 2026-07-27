import { existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

const desktop = resolve(import.meta.dirname, "..");
const tauriCli = resolve(
  desktop,
  "node_modules/@tauri-apps/cli/tauri.js",
);

if (!existsSync(tauriCli)) {
  throw new Error(
    `Tauri CLI is not installed at ${tauriCli}; install locked frontend dependencies first`,
  );
}

const env = { ...process.env };
if (process.platform === "linux" && env.NO_STRIP === undefined) {
  // linuxdeploy ships its own binutils. On rolling distributions those tools
  // can lag the host's ELF format (for example SHT_RELR/.relr.dyn) and damage
  // or reject otherwise valid copied libraries. Distribution libraries and
  // Cargo release binaries are already stripped; leave them intact here.
  env.NO_STRIP = "1";
}

const result = spawnSync(
  process.execPath,
  [tauriCli, "build", ...process.argv.slice(2)],
  {
    cwd: desktop,
    env,
    stdio: "inherit",
    windowsHide: true,
  },
);

if (result.error) {
  throw result.error;
}
process.exit(result.status ?? 1);
