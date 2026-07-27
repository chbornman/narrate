import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { describe, expect, it } from "vitest";

import {
  assertNvidiaTarget,
  inspectNvidiaSource,
  verifyBundleFlavor,
  verifyRuntimeRoot,
} from "../scripts/nvidia-runtime.mjs";

function sourceFixture() {
  const root = mkdtempSync(join(tmpdir(), "photoproof-nvidia-source-"));
  const ort = join(root, "ort");
  const trt = join(root, "trt");
  mkdirSync(ort);
  mkdirSync(trt);
  for (const [dir, name] of [
    [ort, "libonnxruntime.so.1.26.0"],
    [ort, "libonnxruntime_providers_shared.so"],
    [ort, "libonnxruntime_providers_cuda.so"],
    [ort, "libonnxruntime_providers_tensorrt.so"],
    [trt, "libnvinfer.so.10"],
  ]) {
    writeFileSync(join(dir, name), `fixture:${name}`);
  }
  return { root, ort, trt, dylib: join(ort, "libonnxruntime.so.1.26.0") };
}

function packagedFixture() {
  const source = sourceFixture();
  const runtime = join(source.root, "bundle", "usr", "lib", "Photoproof", "runtime");
  const inspected = inspectNvidiaSource({
    platform: "linux",
    arch: "x64",
    ortDylib: source.dylib,
    trtLibs: source.trt,
    requireTensorRt: true,
    minimumRequiredLibraryBytes: 1,
  });
  const files = inspected.artifacts.map(
    (entry: { source: string; path: string }) => {
    const target = join(runtime, entry.path);
    mkdirSync(dirname(target), { recursive: true });
    const bytes = readFileSync(entry.source);
    writeFileSync(target, bytes);
    return {
      path: entry.path.replaceAll("\\", "/"),
      bytes: bytes.length,
      sha256: createHash("sha256").update(bytes).digest("hex"),
    };
    },
  );
  mkdirSync(runtime, { recursive: true });
  writeFileSync(
    join(runtime, "runtime-contract.json"),
    JSON.stringify({
      schemaVersion: 1,
      flavor: "nvidia",
      platform: "linux",
      architecture: "x64",
      cargoFeatures: ["nvidia-package", "cuda-dynamic"],
      providers: ["CUDA", "TensorRT"],
      files,
    }),
  );
  return { tree: join(source.root, "bundle"), runtime };
}

describe("NVIDIA runtime/package contract", () => {
  it("rejects unsupported CPU/Apple package targets before build", () => {
    expect(() => assertNvidiaTarget("darwin", "arm64")).toThrow(/linux\/x64 only/);
    expect(() => assertNvidiaTarget("win32", "x64")).toThrow(/linux\/x64 only/);
  });

  it("requires CUDA runtime truth and optional TensorRT truth from source", () => {
    const source = sourceFixture();
    const inspected = inspectNvidiaSource({
      platform: "linux",
      arch: "x64",
      ortDylib: source.dylib,
      trtLibs: source.trt,
      requireTensorRt: true,
      minimumRequiredLibraryBytes: 1,
    });
    expect(inspected.providers).toEqual(["CUDA", "TensorRT"]);
    writeFileSync(join(source.ort, "libonnxruntime_providers_cuda.so"), "");
    expect(() =>
      inspectNvidiaSource({
        platform: "linux",
        arch: "x64",
        ortDylib: source.dylib,
        minimumRequiredLibraryBytes: 1,
      }),
    ).toThrow(/CUDA provider is implausibly small/);
  });

  it("accepts one complete NVIDIA bundle and rejects tampering", () => {
    const fixture = packagedFixture();
    expect(verifyBundleFlavor(fixture.tree, "nvidia")?.providers).toContain("CUDA");
    const contract = verifyRuntimeRoot(fixture.runtime);
    const first = join(fixture.runtime, contract.files[0].path);
    writeFileSync(first, "tampered");
    expect(() => verifyRuntimeRoot(fixture.runtime)).toThrow(/size\/hash verification/);
  });

  it("keeps the default CPU/Apple package free of NVIDIA runtime files", () => {
    const root = mkdtempSync(join(tmpdir(), "photoproof-default-bundle-"));
    mkdirSync(join(root, "usr", "bin"), { recursive: true });
    writeFileSync(join(root, "usr", "bin", "photoproof"), "fixture");
    expect(verifyBundleFlavor(root, "default")).toBeNull();

    const nvidia = packagedFixture();
    expect(() => verifyBundleFlavor(nvidia.tree, "default")).toThrow(
      /unexpectedly contains NVIDIA runtime/,
    );
  });
});
