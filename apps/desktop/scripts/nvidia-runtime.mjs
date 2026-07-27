// @ts-nocheck -- executable Node packaging code is covered by the runtime
// contract tests; TypeScript declarations for its public seam live in the
// importing test rather than turning the shipping script into generated JS.
import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { basename, dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const desktopDir = resolve(scriptDir, "..");
export const stagedRuntimeDir = resolve(
  desktopDir,
  "src-tauri",
  "nvidia-runtime",
);
const CONTRACT = "runtime-contract.json";
const MIN_REQUIRED_LIBRARY_BYTES = 1_000_000;

function fail(message) {
  throw new Error(`NVIDIA runtime contract: ${message}`);
}

export function assertNvidiaTarget(
  platform = process.platform,
  arch = process.arch,
) {
  if (platform !== "linux" || arch !== "x64") {
    fail(`nvidia-package supports linux/x64 only, not ${platform}/${arch}`);
  }
}

function regularFiles(dir, predicate) {
  if (!dir || !existsSync(dir) || !statSync(dir).isDirectory()) return [];
  return readdirSync(dir)
    .map((name) => join(dir, name))
    .filter((path) => {
      const metadata = lstatSync(path);
      return (metadata.isFile() || metadata.isSymbolicLink()) && predicate(basename(path));
    })
    .sort();
}

function requiredFile(paths, predicate, description, minimumBytes) {
  const path = paths.find((candidate) => predicate(basename(candidate)));
  if (!path) fail(`missing ${description}`);
  const bytes = statSync(path).size;
  if (bytes < minimumBytes) {
    fail(`${description} is implausibly small (${bytes} bytes): ${path}`);
  }
  return path;
}

function artifact(source, path) {
  const bytes = statSync(source).size;
  if (bytes < 1) fail(`empty runtime file: ${source}`);
  return { source, path, bytes };
}

export function inspectNvidiaSource({
  platform = process.platform,
  arch = process.arch,
  ortDylib = process.env.PHOTOPROOF_ORT_DYLIB,
  trtLibs = process.env.PHOTOPROOF_TRT_LIBS,
  requireTensorRt = process.env.PHOTOPROOF_REQUIRE_TENSORRT === "1",
  minimumRequiredLibraryBytes = MIN_REQUIRED_LIBRARY_BYTES,
} = {}) {
  assertNvidiaTarget(platform, arch);
  if (!ortDylib) {
    fail("PHOTOPROOF_ORT_DYLIB must point to libonnxruntime.so");
  }
  const resolvedDylib = realpathSync(resolve(ortDylib));
  if (!statSync(resolvedDylib).isFile()) {
    fail(`PHOTOPROOF_ORT_DYLIB is not a file: ${resolvedDylib}`);
  }
  const ortDir = dirname(resolve(ortDylib));
  const ortFiles = regularFiles(ortDir, (name) =>
    /^libonnxruntime(?:_providers_[a-z0-9_]+)?\.so(?:\..+)?$/i.test(name),
  );
  requiredFile(
    ortFiles,
    (name) => /^libonnxruntime\.so(?:\..+)?$/i.test(name),
    "ONNX Runtime shared library",
    minimumRequiredLibraryBytes,
  );
  requiredFile(
    ortFiles,
    (name) => /^libonnxruntime_providers_cuda\.so(?:\..+)?$/i.test(name),
    "ONNX Runtime CUDA provider",
    minimumRequiredLibraryBytes,
  );

  const artifacts = ortFiles.map((source) =>
    artifact(source, join("onnxruntime-cuda", "lib", basename(source))),
  );
  const providers = ["CUDA"];
  if (trtLibs) {
    const trtDir = realpathSync(resolve(trtLibs));
    const trtFiles = regularFiles(
      trtDir,
      (name) =>
        /^lib(?:nvinfer|nvonnxparser)[a-z0-9_]*\.so(?:\..+)?$/i.test(name),
    );
    requiredFile(
      trtFiles,
      (name) => /^libnvinfer\.so\.10(?:\..+)?$/i.test(name),
      "TensorRT 10 runtime",
      minimumRequiredLibraryBytes,
    );
    artifacts.push(
      ...trtFiles.map((source) =>
        artifact(source, join("tensorrt", "lib", basename(source))),
      ),
    );
    providers.push("TensorRT");
  } else if (requireTensorRt) {
    fail("PHOTOPROOF_REQUIRE_TENSORRT=1 requires PHOTOPROOF_TRT_LIBS");
  }
  return { artifacts, providers };
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

export function stageNvidiaRuntime(options = {}) {
  const inspected = inspectNvidiaSource(options);
  const expectedSuffix = `${sep}src-tauri${sep}nvidia-runtime`;
  if (!stagedRuntimeDir.endsWith(expectedSuffix)) {
    fail(`refusing to replace unexpected staging path ${stagedRuntimeDir}`);
  }
  rmSync(stagedRuntimeDir, { recursive: true, force: true });
  mkdirSync(stagedRuntimeDir, { recursive: true });
  const files = inspected.artifacts.map((entry) => {
    const destination = join(stagedRuntimeDir, entry.path);
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(entry.source, destination);
    return {
      path: entry.path.split(sep).join("/"),
      bytes: entry.bytes,
      sha256: sha256(destination),
    };
  });
  const contract = {
    schemaVersion: 1,
    flavor: "nvidia",
    platform: "linux",
    architecture: "x64",
    cargoFeatures: ["nvidia-package", "cuda-dynamic"],
    providers: inspected.providers,
    files,
  };
  writeFileSync(
    join(stagedRuntimeDir, CONTRACT),
    `${JSON.stringify(contract, null, 2)}\n`,
  );
  verifyRuntimeRoot(stagedRuntimeDir);
  return contract;
}

function safeContractPath(root, path) {
  if (
    typeof path !== "string" ||
    path.length === 0 ||
    path.startsWith("/") ||
    path.split(/[\\/]/).includes("..")
  ) {
    fail(`unsafe contract path ${JSON.stringify(path)}`);
  }
  const target = resolve(root, path);
  const prefix = `${resolve(root)}${sep}`;
  if (!target.startsWith(prefix)) fail(`contract path escapes runtime root: ${path}`);
  return target;
}

export function verifyRuntimeRoot(root, { requireTensorRt = false } = {}) {
  const contractPath = join(root, CONTRACT);
  if (!existsSync(contractPath)) fail(`missing ${contractPath}`);
  const contract = JSON.parse(readFileSync(contractPath, "utf8"));
  if (
    contract.schemaVersion !== 1 ||
    contract.flavor !== "nvidia" ||
    contract.platform !== "linux" ||
    contract.architecture !== "x64" ||
    !contract.cargoFeatures?.includes("nvidia-package") ||
    !contract.cargoFeatures?.includes("cuda-dynamic") ||
    !contract.providers?.includes("CUDA") ||
    !Array.isArray(contract.files)
  ) {
    fail(`invalid ${contractPath}`);
  }
  if (requireTensorRt && !contract.providers.includes("TensorRT")) {
    fail("bundle contract does not include TensorRT");
  }
  for (const entry of contract.files) {
    const path = safeContractPath(root, entry.path);
    if (!existsSync(path) || !statSync(path).isFile()) {
      fail(`contract file is missing: ${entry.path}`);
    }
    const bytes = statSync(path).size;
    if (bytes !== entry.bytes || sha256(path) !== entry.sha256) {
      fail(`contract file failed size/hash verification: ${entry.path}`);
    }
  }
  const paths = contract.files.map((entry) => entry.path);
  if (!paths.some((path) => /libonnxruntime\.so(?:\.|$)/.test(basename(path)))) {
    fail("contract omits libonnxruntime");
  }
  if (!paths.some((path) => /libonnxruntime_providers_cuda\.so/.test(basename(path)))) {
    fail("contract omits the CUDA provider");
  }
  return contract;
}

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

export function verifyBundleFlavor(tree, expectedFlavor) {
  const files = filesBelow(tree);
  const contracts = files.filter((path) => basename(path) === CONTRACT);
  const cudaLibraries = files.filter((path) =>
    basename(path).startsWith("libonnxruntime_providers_cuda.so"),
  );
  if (expectedFlavor === "default") {
    if (contracts.length > 0 || cudaLibraries.length > 0) {
      fail("default CPU/Apple package unexpectedly contains NVIDIA runtime files");
    }
    return null;
  }
  if (expectedFlavor !== "nvidia") fail(`unknown bundle flavor ${expectedFlavor}`);
  if (contracts.length !== 1) {
    fail(`expected one packaged runtime contract, found ${contracts.length}`);
  }
  return verifyRuntimeRoot(dirname(contracts[0]));
}

function usage() {
  return "usage: nvidia-runtime.mjs source|stage|staged|bundle <tree> <default|nvidia>";
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [command, first, second] = process.argv.slice(2);
  if (command === "source") {
    const inspected = inspectNvidiaSource();
    console.log(
      `verified NVIDIA source runtime: ${inspected.providers.join("+")}, ` +
        `${inspected.artifacts.length} libraries`,
    );
  } else if (command === "stage") {
    const contract = stageNvidiaRuntime();
    console.log(
      `staged NVIDIA runtime: ${contract.providers.join("+")}, ` +
        `${contract.files.length} libraries -> ${stagedRuntimeDir}`,
    );
  } else if (command === "staged") {
    const contract = verifyRuntimeRoot(stagedRuntimeDir);
    console.log(`verified staged NVIDIA runtime: ${contract.files.length} libraries`);
  } else if (command === "bundle" && first && second) {
    verifyBundleFlavor(resolve(first), second);
    console.log(`verified ${second} installed runtime flavor below ${resolve(first)}`);
  } else {
    throw new Error(usage());
  }
}
