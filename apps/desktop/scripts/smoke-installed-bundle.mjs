import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, extname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { verifyBundleFlavor } from "./nvidia-runtime.mjs";

const bundleRoot = resolve(process.argv[2] ?? "../../target/release/bundle");
const runtimeArgument = process.argv.indexOf("--expect-runtime");
const expectedRuntime =
  runtimeArgument >= 0 ? process.argv[runtimeArgument + 1] : "default";
const formatArgument = process.argv.indexOf("--format");
const expectedFormat =
  formatArgument >= 0 ? process.argv[formatArgument + 1] : "native";
if (!["default", "nvidia"].includes(expectedRuntime)) {
  throw new Error(
    `--expect-runtime must be default or nvidia, got ${expectedRuntime ?? "<missing>"}`,
  );
}
if (!["native", "appimage"].includes(expectedFormat)) {
  throw new Error(
    `--format must be native or appimage, got ${expectedFormat ?? "<missing>"}`,
  );
}
if (expectedFormat === "appimage" && process.platform !== "linux") {
  throw new Error("--format appimage is only supported on Linux");
}
const work = mkdtempSync(join(tmpdir(), "photoproof-installed-smoke-"));

function filesBelow(root) {
  const files = [];
  function visit(path) {
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      const child = join(path, entry.name);
      if (entry.isDirectory()) visit(child);
      else if (entry.isFile()) files.push(child);
    }
  }
  visit(root);
  return files;
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    timeout: 120_000,
    windowsHide: true,
    ...options,
  });
  if (result.error || result.status !== 0) {
    throw new Error(
      `${command} failed (${result.status ?? "spawn"}): ${
        result.error?.message ?? result.stderr ?? result.stdout
      }`,
    );
  }
  return result;
}

function commandAvailable(command, args = ["--version"]) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    timeout: 10_000,
    windowsHide: true,
  });
  return !result.error && result.status === 0;
}

function one(paths, description) {
  if (paths.length !== 1) {
    throw new Error(
      `expected exactly one ${description}, found ${paths.length}: ${paths.join(", ")}`,
    );
  }
  return paths[0];
}

function installedTree() {
  const bundleFiles = filesBelow(bundleRoot);
  if (process.platform === "linux") {
    if (expectedFormat === "appimage") {
      const appImage = one(
        bundleFiles.filter((path) => extname(path).toLowerCase() === ".appimage"),
        "AppImage package",
      );
      const extracted = join(work, "appimage");
      mkdirSync(extracted, { recursive: true });
      run(appImage, ["--appimage-extract"], { cwd: extracted });
      return {
        tree: join(extracted, "squashfs-root"),
        launcher: appImage,
      };
    }
    const deb = one(
      bundleFiles.filter((path) => extname(path) === ".deb"),
      "Debian package",
    );
    const extracted = join(work, "deb");
    mkdirSync(extracted, { recursive: true });
    if (commandAvailable("dpkg-deb")) {
      run("dpkg-deb", ["-x", deb, extracted]);
    } else {
      // Minimal build hosts may have no dpkg installation. A .deb is an ar
      // archive whose data.tar.* member is the installed filesystem tree, so
      // use the ubiquitous binutils + tar pair without weakening inspection.
      const archive = join(work, "deb-archive");
      mkdirSync(archive, { recursive: true });
      run("ar", ["x", deb], { cwd: archive });
      const dataArchive = one(
        filesBelow(archive).filter((path) =>
          basename(path).startsWith("data.tar."),
        ),
        "Debian data archive",
      );
      run("tar", ["-xf", dataArchive, "-C", extracted]);
    }
    return { tree: extracted };
  }
  if (process.platform === "darwin") {
    const appExecutables = bundleFiles.filter((path) =>
      path.includes(".app/Contents/MacOS/"),
    );
    if (appExecutables.length === 0) {
      throw new Error(`no macOS application bundle found below ${bundleRoot}`);
    }
    return { tree: bundleRoot };
  }
  if (process.platform === "win32") {
    const msi = one(
      bundleFiles.filter((path) => extname(path).toLowerCase() === ".msi"),
      "MSI package",
    );
    const extracted = join(work, "msi");
    run("msiexec.exe", ["/a", msi, "/qn", `TARGETDIR=${extracted}`]);
    return { tree: extracted };
  }
  throw new Error(`unsupported smoke host ${process.platform}`);
}

function executablePair(root) {
  const files = filesBelow(root);
  const childName = process.platform === "win32" ? "pp-asr-server.exe" : "pp-asr-server";
  const child = one(
    files.filter((path) => basename(path) === childName),
    `installed ${childName}`,
  );
  const siblingFiles = files.filter(
    (path) =>
      dirname(path) === dirname(child) &&
      path !== child &&
      basename(path).toLowerCase().includes("photoproof") &&
      (process.platform !== "win32" || extname(path).toLowerCase() === ".exe"),
  );
  const app = one(siblingFiles, "Photoproof executable beside its ASR child");
  const childBytes = statSync(child).size;
  if (childBytes < 1_000_000) {
    throw new Error(`installed child is implausibly small (${childBytes} bytes): ${child}`);
  }
  return { app, child };
}

if (!existsSync(bundleRoot)) {
  throw new Error(`bundle root does not exist: ${bundleRoot}`);
}
const { tree, launcher } = installedTree();
verifyBundleFlavor(tree, expectedRuntime);
const { app, child } = executablePair(tree);
const appData = join(work, "app-data");
if (launcher) {
  run(launcher, ["--appimage-extract-and-run", "--installed-smoke", appData]);
} else {
  run(app, ["--installed-smoke", appData]);
}
const receiptPath = join(appData, "installed-smoke.json");
const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
if (
  receipt.phaseBeforeShutdown !== "Usable" ||
  receipt.phaseAfterShutdown !== "Stopping" ||
  !Number.isInteger(receipt.databaseUserVersion) ||
  receipt.databaseUserVersion < 1 ||
  !Number.isFinite(receipt.initToUsableMs) ||
  receipt.initToUsableMs > 5_000 ||
  !Number.isFinite(receipt.shutdownMs) ||
  receipt.shutdownMs > 5_000 ||
  !Array.isArray(receipt.subsystemHealth) ||
  receipt.sidecarBytes < 1_000_000 ||
  receipt.asrReady !== false ||
  receipt.llmReady !== false ||
  !Number.isInteger(receipt.backupHelperFiles) ||
  receipt.backupHelperFiles < 1 ||
  receipt.restoreRollbackRetained !== true
) {
  throw new Error(`installed smoke receipt failed its contract: ${JSON.stringify(receipt)}`);
}
console.log(
  `installed smoke passed (${expectedRuntime} runtime, ${expectedFormat} format): ` +
    `${launcher ?? app}; child ${child}; ` +
    `schema ${receipt.databaseUserVersion}; ` +
    `usable ${receipt.initToUsableMs} ms; shutdown ${receipt.shutdownMs} ms`,
    `backup helper ${receipt.backupHelperFiles} files; rollback retained`,
);
