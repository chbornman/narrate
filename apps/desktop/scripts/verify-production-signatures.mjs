import { readdirSync, statSync } from "node:fs";
import { basename, extname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(process.argv[2] ?? "../../target/release/bundle");
const files = [];
const directories = [];
function visit(path) {
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isDirectory()) {
      directories.push(child);
      visit(child);
    } else if (entry.isFile()) {
      files.push(child);
    }
  }
}
visit(root);

function run(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) {
    throw new Error(
      `${command} failed for ${args.at(-1)}: ${
        result.error?.message ?? result.stderr ?? result.stdout
      }`,
    );
  }
}

const signatures = files.filter(
  (path) => extname(path) === ".sig" && statSync(path).size > 32,
);
if (signatures.length === 0) {
  throw new Error("production bundle has no non-empty Tauri updater signature");
}

if (process.platform === "darwin") {
  const apps = directories.filter((path) => extname(path) === ".app");
  const dmgs = files.filter((path) => extname(path) === ".dmg");
  if (apps.length === 0 || dmgs.length === 0) {
    throw new Error("production macOS build needs both app and dmg artifacts");
  }
  for (const app of apps) run("codesign", ["--verify", "--deep", "--strict", app]);
  for (const dmg of dmgs) run("xcrun", ["stapler", "validate", dmg]);
}

if (process.platform === "win32") {
  const artifacts = files.filter((path) =>
    [".exe", ".msi"].includes(extname(path).toLowerCase()),
  );
  if (artifacts.length === 0) {
    throw new Error("production Windows build has no installer");
  }
  const command =
    "$s=Get-AuthenticodeSignature -LiteralPath $args[0];" +
    "if($s.Status -ne 'Valid'){throw \"invalid Authenticode status: $($s.Status)\"}";
  for (const artifact of artifacts) {
    run("powershell.exe", ["-NoProfile", "-Command", command, artifact]);
  }
}

console.log(
  `verified production signatures: ${signatures.map(basename).join(", ")}`,
);
