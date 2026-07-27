import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const desktop = resolve(import.meta.dirname, "..");
const workspace = resolve(desktop, "../..");
const read = (path) => readFileSync(resolve(workspace, path), "utf8");
const json = (path) => JSON.parse(read(path));
const packageJson = json("apps/desktop/package.json");
const baseConfig = json("apps/desktop/src-tauri/tauri.conf.json");
const bundleConfig = json("apps/desktop/src-tauri/tauri.bundle.conf.json");
const nvidiaConfig = json("apps/desktop/src-tauri/tauri.nvidia.conf.json");
const desktopCargo = read("apps/desktop/src-tauri/Cargo.toml");
const connectorCargo = read("crates/photoproof-connectors/Cargo.toml");
const main = read("apps/desktop/src-tauri/src/main.rs");
const runtime = read("apps/desktop/src-tauri/src/ort_runtime.rs");

function requireTruth(condition, message) {
  if (!condition) throw new Error(`NVIDIA wiring contract: ${message}`);
}

requireTruth(
  desktopCargo.includes('nvidia-package = ["cuda-dynamic"]'),
  "desktop nvidia-package must include cuda-dynamic",
);
requireTruth(
  connectorCargo.includes('cuda-dynamic = ["tensorrt", "ort/load-dynamic"]'),
  "connector cuda-dynamic must compile TensorRT/CUDA and dynamic ORT",
);
for (const script of ["dev:nvidia", "bundle:nvidia"]) {
  requireTruth(
    packageJson.scripts?.[script]?.includes("--features nvidia-package"),
    `${script} must explicitly enable nvidia-package`,
  );
  requireTruth(
    packageJson.scripts[script].includes("tauri.nvidia.conf.json"),
    `${script} must use the NVIDIA Tauri config`,
  );
}
requireTruth(
  nvidiaConfig.build?.beforeBuildCommand === "npm run build:bundle:nvidia",
  "NVIDIA build must stage and verify its runtime before bundling",
);
requireTruth(
  nvidiaConfig.bundle?.resources?.["nvidia-runtime/"] === "runtime/",
  "NVIDIA runtime resources must land below $RESOURCE/runtime",
);
requireTruth(
  !baseConfig.bundle?.resources && !bundleConfig.bundle?.resources,
  "default CPU/Apple bundles must not contain NVIDIA runtime resources",
);
requireTruth(
  !packageJson.scripts.bundle.includes("nvidia-package") &&
    !packageJson.scripts.tauri.includes("nvidia-package"),
  "default bundle/dev commands must remain CPU/Apple-safe",
);
requireTruth(
  /fn main\(\) \{(?:\s|\/\/[^\n]*\n)*photoproof_desktop::ort_runtime::resolve\(\);/.test(
    main,
  ),
  "ORT resolution must remain the first statement of main",
);
requireTruth(
  runtime.includes('cfg(feature = "nvidia-package")'),
  "NVIDIA package must have a fail-closed runtime branch",
);

console.log("NVIDIA dev/package wiring contract verified");
