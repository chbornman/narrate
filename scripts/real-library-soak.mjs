#!/usr/bin/env node
/**
 * Repeatable real-media soak harness.
 *
 * Source trees are opened read-only. Selected supported photographs are copied
 * into an explicit staging directory outside the source and outside /homenas;
 * every runner sees only that stage. Progress is written as versioned JSON,
 * append-only JSONL, and an upserted spreadsheet-friendly CSV row.
 */
import {
  constants as fsConstants,
  createReadStream,
  createWriteStream,
} from "node:fs";
import {
  access,
  appendFile,
  mkdir,
  readFile,
  readdir,
  rename,
  stat,
  statfs,
  writeFile,
} from "node:fs/promises";
import { createHash, randomBytes } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";

export const RECEIPT_SCHEMA = 1;
export const STAGE_SCHEMA = 1;
export const SUPPORTED_EXTENSIONS = new Set([
  "jpg", "jpeg", "png", "tif", "tiff", "webp", "heic", "heif",
  "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "raf", "orf",
  "ori", "rw2", "pef", "srw", "x3f", "3fr", "fff", "iiq", "mos",
  "mrw", "kdc", "dcr", "sr2", "srf", "erf",
]);
const RAW_EXTENSIONS = new Set([
  "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "raf", "orf",
  "ori", "rw2", "pef", "srw", "x3f", "3fr", "fff", "iiq", "mos",
  "mrw", "kdc", "dcr", "sr2", "srf", "erf",
]);
const EXCLUDED_DIR_NAMES = new Set([
  "@eadir", "__macosx", "$recycle.bin", "system volume information",
  "lost+found", "node_modules", "cache", "proxies", "thumbnails",
]);
const MAX_FILE_BYTES = 2 * 1024 * 1024 * 1024;
const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(SCRIPT_DIR, "..");

const TIERS = {
  dry: { limit: 10, inventoryCap: 100, loops: 0, reserveGiB: 0, maxSelectedGiB: 10 },
  small: { limit: 25, inventoryCap: 2_000, loops: 1, reserveGiB: 5, maxSelectedGiB: 50 },
  standard: {
    limit: 5_000,
    inventoryCap: 0,
    loops: 3,
    reserveGiB: 100,
    maxSelectedGiB: 300,
  },
  soak: {
    limit: 50_000,
    inventoryCap: 0,
    loops: 3,
    reserveGiB: 1_024,
    maxSelectedGiB: 2_200,
  },
};

function usage() {
  return `usage:
  node scripts/real-library-soak.mjs --source [NAME=]DIR --receipts DIR --tier dry
  node scripts/real-library-soak.mjs --source [NAME=]DIR --destination DIR \\
    --receipts DIR --tier small [--prepare-only]
  node scripts/real-library-soak.mjs --source DIR --destination DIR \\
    --receipts DIR --tier soak [--reuse-stage]

options:
  --tier dry|small|standard|soak
  --limit N                 override selected media count
  --inventory-cap N         stop after N supported files (0 = complete walk)
  --loops N                 override repeated runner loops
  --include-all-raw         select every inventoried RAW before rendered files
  --reserve-gib N           free-space reserve after selected bytes are copied
  --max-selected-gib N      fail before copy when the selection is larger
  --seed TEXT               deterministic selection seed
  --run-id TEXT             stable CSV upsert key
  --prepare-only            inventory/copy/validate, do not run a benchmark
  --reuse-stage             reuse a matching stage manifest
  --bench-bin FILE          use an existing pp_bench binary
  --runner-command-json JSON
      external installed-compatible argv array; placeholders:
      {stage}, {runnerReceipt}, {runId}, {loop}

The source and /homenas are never writable targets. JSON argv is executed
directly without a shell.`;
}

function parseArgs(argv) {
  const out = {
    tier: "dry",
    seed: "photoproof-real-soak-v1",
    prepareOnly: false,
    reuseStage: false,
    includeAllRaw: false,
    sourceInputs: [],
  };
  for (let index = 0; index < argv.length; index++) {
    const flag = argv[index];
    const value = () => {
      const next = argv[++index];
      if (next === undefined) throw new Error(`${flag} requires a value`);
      return next;
    };
    switch (flag) {
      case "--source": out.sourceInputs.push(value()); break;
      case "--destination": out.destination = value(); break;
      case "--receipts": out.receipts = value(); break;
      case "--tier": out.tier = value(); break;
      case "--limit": out.limit = positiveInteger(value(), flag); break;
      case "--inventory-cap": out.inventoryCap = nonnegativeInteger(value(), flag); break;
      case "--loops": out.loops = nonnegativeInteger(value(), flag); break;
      case "--reserve-gib": out.reserveGiB = nonnegativeNumber(value(), flag); break;
      case "--max-selected-gib": {
        out.maxSelectedGiB = nonnegativeNumber(value(), flag);
        break;
      }
      case "--seed": out.seed = value(); break;
      case "--run-id": out.runId = value(); break;
      case "--bench-bin": out.benchBin = value(); break;
      case "--runner-command-json": out.runnerCommandJson = value(); break;
      case "--prepare-only": out.prepareOnly = true; break;
      case "--reuse-stage": out.reuseStage = true; break;
      case "--include-all-raw": out.includeAllRaw = true; break;
      case "-h":
      case "--help":
        console.log(usage());
        process.exit(0);
      default: throw new Error(`unknown flag ${flag}\n${usage()}`);
    }
  }
  if (!TIERS[out.tier]) throw new Error(`unknown tier ${out.tier}`);
  const tier = TIERS[out.tier];
  out.limit ??= tier.limit;
  out.inventoryCap ??= tier.inventoryCap;
  out.loops ??= tier.loops;
  out.reserveGiB ??= tier.reserveGiB;
  out.maxSelectedGiB ??= tier.maxSelectedGiB;
  out.reserveBytes = Math.round(out.reserveGiB * 1024 ** 3);
  out.maxSelectedBytes = Math.round(out.maxSelectedGiB * 1024 ** 3);
  if (out.sourceInputs.length === 0 || !out.receipts) {
    throw new Error(`at least one --source and --receipts are required`);
  }
  if (out.tier !== "dry" && !out.destination) {
    throw new Error(`--destination is required outside the dry tier`);
  }
  if (out.runnerCommandJson) {
    const parsed = JSON.parse(out.runnerCommandJson);
    if (!Array.isArray(parsed) || parsed.length === 0 || parsed.some((v) => typeof v !== "string")) {
      throw new Error("--runner-command-json must be a non-empty JSON string array");
    }
    out.runnerCommand = parsed;
  }
  out.runId ??= `${new Date().toISOString().replaceAll(/[:.]/g, "-")}-${randomBytes(3).toString("hex")}`;
  if (!/^[A-Za-z0-9._-]+$/.test(out.runId)) {
    throw new Error("--run-id may contain only letters, digits, dot, underscore, and dash");
  }
  return out;
}

function positiveInteger(value, flag) {
  const n = Number(value);
  if (!Number.isSafeInteger(n) || n <= 0) throw new Error(`${flag} must be a positive integer`);
  return n;
}

function nonnegativeInteger(value, flag) {
  const n = Number(value);
  if (!Number.isSafeInteger(n) || n < 0) throw new Error(`${flag} must be a nonnegative integer`);
  return n;
}

function nonnegativeNumber(value, flag) {
  const n = Number(value);
  if (!Number.isFinite(n) || n < 0) throw new Error(`${flag} must be a nonnegative number`);
  return n;
}

function parseSourceInputs(inputs) {
  const used = new Set();
  return inputs.map((input) => {
    const equals = input.indexOf("=");
    const explicit = equals > 0 ? input.slice(0, equals) : null;
    const path = resolve(equals > 0 ? input.slice(equals + 1) : input);
    let name = explicit ?? basename(path);
    name = name.replaceAll(/[^A-Za-z0-9._-]/g, "_");
    if (!name || name === "." || name === "..") throw new Error(`invalid source namespace: ${name}`);
    if (used.has(name)) throw new Error(`duplicate source namespace ${name}; use NAME=DIR`);
    used.add(name);
    return { name, path };
  });
}

function within(parent, child) {
  const rel = relative(parent, child);
  return rel === "" || (!rel.startsWith(`..${sep}`) && rel !== ".." && !isAbsolute(rel));
}

export function assertSafePaths(sourceValue, destinationValue, receiptsValue) {
  const sourceInputs = Array.isArray(sourceValue) ? sourceValue : [sourceValue];
  const sources = parseSourceInputs(sourceInputs);
  const receipts = resolve(receiptsValue);
  const destination = destinationValue ? resolve(destinationValue) : null;
  const homenas = resolve("/homenas");
  for (const source of sources) {
    if (source.path === resolve("/") || source.path === homenas) {
      throw new Error("refusing an overly broad source root");
    }
  }
  for (const [name, target] of [["receipts", receipts], ["destination", destination]]) {
    if (!target) continue;
    if (within(homenas, target)) throw new Error(`${name} must not be inside immutable /homenas`);
    for (const source of sources) {
      if (within(source.path, target)) throw new Error(`${name} must not be inside the source tree`);
      if (within(target, source.path)) throw new Error(`${name} must not contain the source tree`);
    }
    if (target === resolve("/") || target === REPO_ROOT) {
      throw new Error(`refusing broad ${name} target ${target}`);
    }
  }
  if (destination && within(destination, receipts)) {
    throw new Error("receipts must not be inside the disposable media stage");
  }
  return { sources, destination, receipts };
}

function excludedDirectory(name) {
  const lower = name.toLowerCase();
  return name.startsWith(".") ||
    EXCLUDED_DIR_NAMES.has(lower) ||
    lower.endsWith(".lrdata") ||
    lower.endsWith(".cocatalogdb") ||
    lower.startsWith("lightroom catalog");
}

function supportedFile(name) {
  if (name.startsWith(".") || name.toLowerCase().endsWith(".photoproof.json")) return null;
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return null;
  const extension = name.slice(dot + 1).toLowerCase();
  return SUPPORTED_EXTENSIONS.has(extension) ? extension : null;
}

export async function inventorySource(source, inventoryCap = 0) {
  const files = [];
  const countsByExtension = {};
  let bytes = 0;
  let complete = true;
  async function visit(directory) {
    let entries = await readdir(directory, { withFileTypes: true });
    entries = entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      if (inventoryCap > 0 && files.length >= inventoryCap) {
        complete = false;
        return false;
      }
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) {
        if (!excludedDirectory(entry.name) && !await visit(path)) return false;
        continue;
      }
      if (!entry.isFile()) continue;
      const extension = supportedFile(entry.name);
      if (!extension) continue;
      const info = await stat(path, { bigint: true });
      if (info.size > BigInt(MAX_FILE_BYTES)) continue;
      const relPath = relative(source, path);
      const row = {
        relPath,
        extension,
        kind: RAW_EXTENSIONS.has(extension) ? "raw" : "rendered",
        size: Number(info.size),
        mtimeNs: info.mtimeNs.toString(),
      };
      files.push(row);
      bytes += row.size;
      countsByExtension[extension] = (countsByExtension[extension] ?? 0) + 1;
    }
    return true;
  }
  await visit(source);
  return { files, bytes, countsByExtension, complete };
}

export function selectInventory(files, limit, seed, includeAllRaw = false) {
  const ranked = files
    .map((file) => ({
      ...file,
      selectionKey: createHash("sha256")
        .update(`${seed}\0${file.sourceName ?? ""}\0${file.relPath}`)
        .digest("hex"),
    }))
    .sort((a, b) =>
      a.selectionKey.localeCompare(b.selectionKey) ||
      (a.sourceName ?? "").localeCompare(b.sourceName ?? "") ||
      a.relPath.localeCompare(b.relPath));
  const selected = includeAllRaw
    ? [
        ...ranked.filter((file) => file.kind === "raw"),
        ...ranked.filter((file) => file.kind !== "raw"),
      ].slice(0, limit)
    : ranked.slice(0, limit);
  return selected.map(({ selectionKey: _selectionKey, ...file }) => file);
}

function metadataFingerprint(files) {
  const hash = createHash("sha256");
  for (const file of [...files].sort((a, b) =>
    (a.sourceName ?? "").localeCompare(b.sourceName ?? "") ||
    a.relPath.localeCompare(b.relPath))) {
    hash.update(
      `${file.sourceName ?? ""}\0${file.relPath}\0${file.size}\0${file.mtimeNs}\n`,
    );
  }
  return hash.digest("hex");
}

async function hashFile(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

async function copyAndHash(sourcePath, destinationPath) {
  await mkdir(dirname(destinationPath), { recursive: true });
  const hash = createHash("sha256");
  const input = createReadStream(sourcePath);
  input.on("data", (chunk) => hash.update(chunk));
  await pipeline(input, createWriteStream(destinationPath, { flags: "wx", mode: 0o600 }));
  return hash.digest("hex");
}

async function snapshotSelected(selected) {
  const rows = [];
  for (const file of selected) {
    const info = await stat(join(file.sourceRoot, file.relPath), { bigint: true });
    rows.push({
      sourceName: file.sourceName,
      relPath: file.relPath,
      size: Number(info.size),
      mtimeNs: info.mtimeNs.toString(),
    });
  }
  return rows;
}

async function prepareStage(sources, destination, selected, run, reuse, reserveBytes) {
  const markerPath = join(destination, ".photoproof-soak-stage.v1.json");
  const selectionFingerprint = metadataFingerprint(selected);
  const sourceDescriptor = sources.map(({ name, path }) => ({ name, path }));
  if (reuse) {
    const marker = JSON.parse(await readFile(markerPath, "utf8"));
    if (
      marker.schema !== STAGE_SCHEMA ||
      JSON.stringify(marker.sources) !== JSON.stringify(sourceDescriptor) ||
      marker.selectionFingerprint !== selectionFingerprint
    ) {
      throw new Error("existing stage manifest does not match this source/selection");
    }
    return { ...marker, reused: true };
  }
  try {
    const entries = await readdir(destination);
    if (entries.length > 0) throw new Error("destination exists and is not empty; use --reuse-stage");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    await mkdir(destination, { recursive: true });
  }
  const filesystem = await statfs(destination, { bigint: true });
  const availableBytes = Number(filesystem.bavail * filesystem.bsize);
  const selectedBytes = selected.reduce((sum, file) => sum + file.size, 0);
  const requiredBytes = selectedBytes + reserveBytes;
  if (availableBytes < requiredBytes) {
    throw new Error(
      `stage preflight needs ${requiredBytes} bytes (${selectedBytes} selected + ` +
      `${reserveBytes} reserve), only ${availableBytes} available`,
    );
  }
  const copied = [];
  for (let index = 0; index < selected.length; index++) {
    const file = selected[index];
    const digest = await copyAndHash(
      join(file.sourceRoot, file.relPath),
      join(destination, file.stageRelPath),
    );
    copied.push({ ...file, sha256: digest });
    if ((index + 1) % 25 === 0 || index + 1 === selected.length) {
      await run.progress("copying", { copiedFiles: index + 1 });
    }
  }
  // Destination verification is deliberately separate from the copy stream.
  const verifyCount = Math.min(copied.length, 32);
  for (const file of copied.slice(0, verifyCount)) {
    const digest = await hashFile(join(destination, file.stageRelPath));
    if (digest !== file.sha256) throw new Error(`stage checksum mismatch: ${file.relPath}`);
  }
  const marker = {
    schema: STAGE_SCHEMA,
    sources: sourceDescriptor,
    selectionFingerprint,
    selectedFiles: copied.length,
    selectedBytes: copied.reduce((sum, file) => sum + file.size, 0),
    verifiedDestinationHashes: verifyCount,
    freeBytesBeforeCopy: availableBytes,
    reserveBytes,
    projectedFreeBytesAfterCopy: availableBytes - selectedBytes,
    createdAt: new Date().toISOString(),
  };
  await writeAtomic(markerPath, marker);
  return { ...marker, reused: false };
}

function queryNvidia() {
  const result = spawnSync("nvidia-smi", [
    "--query-gpu=name,uuid,memory.total,memory.used,driver_version",
    "--format=csv,noheader,nounits",
  ], { encoding: "utf8", timeout: 10_000 });
  if (result.error || result.status !== 0 || !result.stdout.trim()) {
    return {
      status: "unavailable",
      reason: (
        result.error?.message ||
        result.stderr ||
        result.stdout ||
        "nvidia-smi returned no accessible NVIDIA GPU"
      ).trim(),
    };
  }
  const [name, uuid, total, used, driver] = result.stdout.trim().split("\n")[0].split(",").map((v) => v.trim());
  return {
    status: "available",
    name,
    uuid,
    totalMb: Number(total),
    usedMb: Number(used),
    driver,
    measurementScope: "device total; may include other processes",
  };
}

async function sourceMountTruth(path) {
  if (process.platform !== "linux") {
    return { status: "unavailable", reason: "mount-option inspection is Linux-only" };
  }
  try {
    const lines = (await readFile("/proc/self/mountinfo", "utf8")).trim().split("\n");
    let best = null;
    for (const line of lines) {
      const fields = line.split(" ");
      const separator = fields.indexOf("-");
      if (separator < 0) continue;
      const mountPoint = fields[4]
        .replaceAll("\\040", " ")
        .replaceAll("\\011", "\t")
        .replaceAll("\\134", "\\");
      if (!within(mountPoint, path)) continue;
      if (!best || mountPoint.length > best.mountPoint.length) {
        const options = fields[5].split(",");
        const superOptions = fields[separator + 3].split(",");
        best = {
          status: "reported",
          mountPoint,
          filesystem: fields[separator + 1],
          source: fields[separator + 2],
          options,
          superOptions,
          readOnly: options.includes("ro") || superOptions.includes("ro"),
        };
      }
    }
    return best ?? { status: "unavailable", reason: "no containing mount found" };
  } catch (error) {
    return { status: "unavailable", reason: error.message };
  }
}

async function processRssKb(pid) {
  if (process.platform === "linux") {
    try {
      const status = await readFile(`/proc/${pid}/status`, "utf8");
      const rss = Number(status.match(/^VmRSS:\s+(\d+)/m)?.[1] ?? 0);
      const hwm = Number(status.match(/^VmHWM:\s+(\d+)/m)?.[1] ?? 0);
      return Math.max(rss, hwm);
    } catch {
      return 0;
    }
  }
  const result = spawnSync("ps", ["-o", "rss=", "-p", String(pid)], {
    encoding: "utf8",
    timeout: 2_000,
  });
  return result.status === 0 ? Number(result.stdout.trim() || 0) : 0;
}

async function runMonitored(command, args, options = {}) {
  const started = performance.now();
  const child = spawn(command, args, {
    cwd: options.cwd ?? REPO_ROOT,
    env: { ...process.env, ...options.env },
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  let peakRssKb = 0;
  const initialGpu = queryNvidia();
  let peakGpuUsedMb = initialGpu.usedMb ?? null;
  const sample = async () => {
    peakRssKb = Math.max(peakRssKb, await processRssKb(child.pid));
    if (initialGpu.status === "available") {
      const gpu = queryNvidia();
      if (gpu.status === "available") {
        peakGpuUsedMb = Math.max(peakGpuUsedMb ?? 0, gpu.usedMb);
      }
    }
  };
  await sample();
  const timer = setInterval(() => void sample(), 250);
  const { code, signal } = await new Promise((resolvePromise, reject) => {
    child.once("error", reject);
    child.once("close", (exitCode, exitSignal) =>
      resolvePromise({ code: exitCode, signal: exitSignal }));
  });
  clearInterval(timer);
  await sample();
  const result = {
    command,
    args,
    code,
    signal,
    durationMs: performance.now() - started,
    peakRssMb: peakRssKb > 0 ? peakRssKb / 1024 : null,
    peakGpuUsedMb,
    stdout,
    stderr,
  };
  if (code !== 0) {
    throw Object.assign(new Error(`${command} exited ${code ?? signal}: ${stderr || stdout}`), {
      monitoredResult: result,
    });
  }
  return result;
}

async function ensureBench(binaryOption) {
  if (binaryOption) {
    await access(resolve(binaryOption), fsConstants.X_OK);
    return resolve(binaryOption);
  }
  const build = spawnSync("cargo", [
    "build", "--release", "-q", "-p", "photoproof-core", "--bin", "pp_bench",
  ], { cwd: REPO_ROOT, encoding: "utf8", timeout: 30 * 60_000 });
  if (build.error || build.status !== 0) {
    throw new Error(`building pp_bench failed: ${build.error?.message ?? build.stderr}`);
  }
  return join(REPO_ROOT, "target", "release", "pp_bench");
}

function substituteArg(value, replacements) {
  let out = value;
  for (const [name, replacement] of Object.entries(replacements)) {
    out = out.replaceAll(`{${name}}`, replacement);
  }
  return out;
}

export function summarizeRunnerErrors(loops) {
  const byLoop = loops.map((loop) => ({
    loop: loop.loop,
    count: loop.runnerReceipt?.queue_errors ?? 0,
    groups: loop.runnerReceipt?.error_groups ?? [],
  }));
  return {
    total: byLoop.reduce((sum, loop) => sum + loop.count, 0),
    loopsWithErrors: byLoop.filter((loop) => loop.count > 0).length,
    byLoop,
  };
}

async function runLoops(args, paths, run) {
  const loops = [];
  const runner = args.runnerCommand ? "external-installed-compatible" : "pp_bench";
  const bench = args.runnerCommand ? null : await ensureBench(args.benchBin);
  for (let index = 0; index < args.loops; index++) {
    await run.progress("running", { loop: index + 1 });
    const loopReceipt = join(paths.receipts, `${args.runId}.loop-${index + 1}.runner.json`);
    let command;
    let commandArgs;
    let benchOutput = null;
    if (args.runnerCommand) {
      const replacements = {
        stage: paths.destination,
        runnerReceipt: loopReceipt,
        runId: args.runId,
        loop: String(index + 1),
      };
      [command, ...commandArgs] = args.runnerCommand.map((value) =>
        substituteArg(value, replacements));
    } else {
      const output = join(paths.receipts, `${args.runId}.loop-${index + 1}.bench.jsonl`);
      command = bench;
      commandArgs = [
        "ingest", "--source", paths.destination,
        "--label", `${args.runId}-loop-${index + 1}`,
        "--out", output,
      ];
    }
    const monitored = await runMonitored(command, commandArgs);
    if (args.runnerCommand) {
      try {
        benchOutput = JSON.parse(await readFile(loopReceipt, "utf8"));
      } catch (error) {
        benchOutput = { receiptStatus: "unavailable", reason: error.message };
      }
    } else {
      const line = monitored.stdout.trim().split("\n").filter(Boolean).at(-1);
      benchOutput = line ? JSON.parse(line) : null;
    }
    loops.push({
      loop: index + 1,
      durationMs: monitored.durationMs,
      peakRssMb: monitored.peakRssMb,
      peakGpuUsedMb: monitored.peakGpuUsedMb,
      runnerReceipt: benchOutput,
    });
  }
  return { runner, loops };
}

function csvEscape(value) {
  if (value === null || value === undefined) return "";
  const text = String(value);
  return /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

const CSV_COLUMNS = [
  "schema", "run_id", "updated_at", "phase", "result", "tier", "runner",
  "source", "destination", "inventory_complete", "inventory_files",
  "selected_files", "selected_bytes", "raw_files", "rendered_files", "loops",
  "mean_total_ms", "mean_files_per_s", "peak_rss_mb", "nvidia_status",
  "gpu_name", "peak_gpu_used_mb", "provider_status", "provider",
  "queue_errors", "source_unchanged", "error",
];

function csvRow(receipt) {
  const loopReceipts = receipt.loops ?? [];
  const totals = loopReceipts.map((loop) => loop.runnerReceipt?.total_ms).filter(Number.isFinite);
  const rates = loopReceipts.map((loop) => loop.runnerReceipt?.files_per_s).filter(Number.isFinite);
  const max = (values) => values.length ? Math.max(...values) : null;
  const mean = (values) => values.length ? values.reduce((a, b) => a + b, 0) / values.length : null;
  const values = {
    schema: receipt.schema,
    run_id: receipt.runId,
    updated_at: receipt.updatedAt,
    phase: receipt.phase,
    result: receipt.result,
    tier: receipt.tier,
    runner: receipt.runner,
    source: receipt.sources?.map((source) => `${source.name}=${source.path}`).join(";"),
    destination: receipt.destination,
    inventory_complete: receipt.inventory?.complete,
    inventory_files: receipt.inventory?.files,
    selected_files: receipt.selection?.files,
    selected_bytes: receipt.selection?.bytes,
    raw_files: receipt.selection?.rawFiles,
    rendered_files: receipt.selection?.renderedFiles,
    loops: loopReceipts.length,
    mean_total_ms: mean(totals),
    mean_files_per_s: mean(rates),
    peak_rss_mb: max(loopReceipts.map((loop) => loop.peakRssMb).filter(Number.isFinite)),
    nvidia_status: receipt.nvidia?.status,
    gpu_name: receipt.nvidia?.name,
    peak_gpu_used_mb: max(loopReceipts.map((loop) => loop.peakGpuUsedMb).filter(Number.isFinite)),
    provider_status: receipt.provider?.status,
    provider: receipt.provider?.name,
    queue_errors: receipt.runnerErrors?.total ?? 0,
    source_unchanged: receipt.sourceValidation?.unchanged,
    error: receipt.error,
  };
  return CSV_COLUMNS.map((column) => csvEscape(values[column])).join(",");
}

async function upsertCsv(path, receipt) {
  let rows = [];
  try {
    rows = (await readFile(path, "utf8")).trimEnd().split(/\r?\n/);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const header = CSV_COLUMNS.join(",");
  if (rows.length === 0) rows = [header];
  if (rows[0] !== header) throw new Error(`CSV schema mismatch at ${path}`);
  const key = csvEscape(receipt.runId);
  const next = csvRow(receipt);
  // run_id is deliberately restricted to a CSV-safe token, so the second
  // comma-delimited field can be compared without a full CSV parser.
  const index = rows.findIndex(
    (row, rowIndex) => rowIndex > 0 && row.split(",")[1] === key,
  );
  if (index >= 0) rows[index] = next;
  else rows.push(next);
  await writeTextAtomic(path, `${rows.join("\n")}\n`);
}

async function writeAtomic(path, value) {
  await writeTextAtomic(path, `${JSON.stringify(value, null, 2)}\n`);
}

async function writeTextAtomic(path, value) {
  const temp = `${path}.tmp-${process.pid}`;
  await writeFile(temp, value);
  await rename(temp, path);
}

function createProgressWriter(args, paths, receipt) {
  return {
    async progress(phase, patch = {}) {
      Object.assign(receipt, patch, {
        phase,
        updatedAt: new Date().toISOString(),
      });
      const event = {
        schema: RECEIPT_SCHEMA,
        runId: receipt.runId,
        at: receipt.updatedAt,
        phase,
        ...patch,
      };
      await appendFile(join(paths.receipts, "soak-progress.v1.jsonl"), `${JSON.stringify(event)}\n`);
      await writeAtomic(join(paths.receipts, `${args.runId}.json`), receipt);
      await upsertCsv(join(paths.receipts, "soak-progress.v2.csv"), receipt);
    },
  };
}

async function main(argv) {
  const args = parseArgs(argv);
  const paths = assertSafePaths(args.sourceInputs, args.destination, args.receipts);
  for (const source of paths.sources) {
    await access(source.path, fsConstants.R_OK);
    source.mount = await sourceMountTruth(source.path);
  }
  await mkdir(paths.receipts, { recursive: true });
  const receipt = {
    schema: RECEIPT_SCHEMA,
    runId: args.runId,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    phase: "starting",
    result: "running",
    tier: args.tier,
    sources: paths.sources,
    destination: paths.destination,
    immutableSourceContract: true,
    host: {
      platform: process.platform,
      arch: process.arch,
      node: process.version,
    },
    nvidia: queryNvidia(),
  };
  const run = createProgressWriter(args, paths, receipt);
  try {
    await run.progress("inventory");
    const inventory = {
      files: [],
      bytes: 0,
      countsByExtension: {},
      complete: true,
      sources: [],
    };
    for (const source of paths.sources) {
      const remainingCap =
        args.inventoryCap === 0 ? 0 : Math.max(0, args.inventoryCap - inventory.files.length);
      if (args.inventoryCap > 0 && remainingCap === 0) {
        inventory.complete = false;
        inventory.sources.push({ name: source.name, path: source.path, scanned: false });
        continue;
      }
      const sourceInventory = await inventorySource(source.path, remainingCap);
      inventory.files.push(
        ...sourceInventory.files.map((file) => ({
          ...file,
          sourceName: source.name,
          sourceRoot: source.path,
          stageRelPath: join(source.name, file.relPath),
        })),
      );
      inventory.bytes += sourceInventory.bytes;
      inventory.complete &&= sourceInventory.complete;
      for (const [extension, count] of Object.entries(sourceInventory.countsByExtension)) {
        inventory.countsByExtension[extension] =
          (inventory.countsByExtension[extension] ?? 0) + count;
      }
      inventory.sources.push({
        name: source.name,
        path: source.path,
        scanned: true,
        complete: sourceInventory.complete,
        files: sourceInventory.files.length,
        bytes: sourceInventory.bytes,
      });
    }
    const selected = selectInventory(
      inventory.files,
      args.limit,
      args.seed,
      args.includeAllRaw,
    );
    if (selected.length === 0) throw new Error("inventory found no supported media");
    const before = await snapshotSelected(selected);
    receipt.inventory = {
      complete: inventory.complete,
      cap: args.inventoryCap,
      files: inventory.files.length,
      bytes: inventory.bytes,
      countsByExtension: inventory.countsByExtension,
      sources: inventory.sources,
    };
    receipt.selection = {
      seed: args.seed,
      includeAllRaw: args.includeAllRaw,
      requestedFiles: args.limit,
      files: selected.length,
      bytes: selected.reduce((sum, file) => sum + file.size, 0),
      rawFiles: selected.filter((file) => file.kind === "raw").length,
      renderedFiles: selected.filter((file) => file.kind === "rendered").length,
      metadataFingerprint: metadataFingerprint(before),
      relativePaths: selected.map((file) => file.stageRelPath),
      maxSelectedBytes: args.maxSelectedBytes,
    };
    await run.progress("inventoried");
    if (receipt.selection.bytes > args.maxSelectedBytes) {
      throw new Error(
        `selection is ${receipt.selection.bytes} bytes, above the ` +
        `${args.maxSelectedBytes}-byte --max-selected-gib ceiling`,
      );
    }

    if (args.tier === "dry") {
      receipt.runner = "none";
      receipt.provider = { status: "unavailable", reason: "dry inventory tier does not start PhotoProof" };
    } else {
      await run.progress("copying", { copiedFiles: 0 });
      receipt.stage = await prepareStage(
        paths.sources,
        paths.destination,
        selected,
        run,
        args.reuseStage,
        args.reserveBytes,
      );
      if (!args.prepareOnly && args.loops > 0) {
        const ran = await runLoops(args, paths, run);
        receipt.runner = ran.runner;
        receipt.loops = ran.loops;
        if (ran.runner === "pp_bench") {
          receipt.runnerErrors = summarizeRunnerErrors(ran.loops);
          receipt.provider = {
            status: "unavailable",
            reason: "headless pp_bench does not initialize the ML runtime",
          };
        } else {
          const reported = ran.loops.map((loop) => loop.runnerReceipt?.provider).find(Boolean);
          receipt.provider = reported
            ? {
                status: "reported",
                name:
                  typeof reported === "string"
                    ? reported
                    : reported.name ?? reported.id ?? JSON.stringify(reported),
              }
            : { status: "unavailable", reason: "external runner receipt did not report a provider" };
        }
      } else {
        receipt.runner = "none";
        receipt.provider = {
          status: "unavailable",
          reason: "prepare-only run does not initialize the ML runtime",
        };
      }
    }

    await run.progress("validating-source");
    const after = await snapshotSelected(selected);
    const beforeFingerprint = metadataFingerprint(before);
    const afterFingerprint = metadataFingerprint(after);
    const hashSample = selected.slice(0, Math.min(8, selected.length));
    const hashSampleResults = [];
    if (receipt.stage) {
      for (const file of hashSample) {
        const sourceHash = await hashFile(join(file.sourceRoot, file.relPath));
        const stageHash = await hashFile(join(paths.destination, file.stageRelPath));
        hashSampleResults.push({
          relPath: file.stageRelPath,
          equal: sourceHash === stageHash,
        });
      }
    }
    receipt.sourceValidation = {
      beforeFingerprint,
      afterFingerprint,
      metadataUnchanged: beforeFingerprint === afterFingerprint,
      hashSampleFiles: hashSampleResults.length,
      hashSampleEqual: hashSampleResults.every((row) => row.equal),
      unchanged:
        beforeFingerprint === afterFingerprint &&
        hashSampleResults.every((row) => row.equal),
    };
    if (!receipt.sourceValidation.unchanged) {
      throw new Error("source metadata or sampled bytes changed during the run");
    }
    receipt.result =
      (receipt.runnerErrors?.total ?? 0) === 0 ? "passed" : "failed";
    if (receipt.result === "failed") {
      receipt.error =
        `${receipt.runnerErrors.total} queue errors across ` +
        `${receipt.runnerErrors.loopsWithErrors} measured loops`;
    }
    await run.progress("completed");
    await appendFile(
      join(paths.receipts, "soak-runs.v1.jsonl"),
      `${JSON.stringify(receipt)}\n`,
    );
    console.log(JSON.stringify(receipt, null, 2));
    return receipt;
  } catch (error) {
    receipt.result = "failed";
    receipt.error = error.message;
    if (error.monitoredResult) receipt.failedProcess = error.monitoredResult;
    await run.progress("failed");
    await appendFile(
      join(paths.receipts, "soak-runs.v1.jsonl"),
      `${JSON.stringify(receipt)}\n`,
    );
    throw error;
  }
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(`real-library-soak: ${error.stack ?? error.message}`);
    process.exitCode = 1;
  });
}
