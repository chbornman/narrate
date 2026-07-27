import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import {
  assertSafePaths,
  inventorySource,
  selectInventory,
  summarizeRunnerErrors,
} from "./real-library-soak.mjs";

test("queue errors make a measured run non-clean without leaking detail", () => {
  const summary = summarizeRunnerErrors([
    {
      loop: 1,
      runnerReceipt: {
        queue_errors: 2,
        error_groups: [
          { pass: "preview", category: "decode", format: "raw", count: 2 },
        ],
      },
    },
    { loop: 2, runnerReceipt: { queue_errors: 0, error_groups: [] } },
  ]);
  assert.equal(summary.total, 2);
  assert.equal(summary.loopsWithErrors, 1);
  assert.deepEqual(summary.byLoop[0].groups, [
    { pass: "preview", category: "decode", format: "raw", count: 2 },
  ]);
});

test("inventory mirrors the supported-media and hidden/sidecar exclusions", async () => {
  const root = await mkdtemp(join(tmpdir(), "pp-soak-inventory-"));
  try {
    await mkdir(join(root, "nested"));
    await mkdir(join(root, ".hidden"));
    await writeFile(join(root, "a.JPG"), "jpeg");
    await writeFile(join(root, "nested", "b.CR3"), "raw");
    await writeFile(join(root, "nested", "b.CR3.photoproof.json"), "{}");
    await writeFile(join(root, ".hidden", "c.nef"), "hidden");
    await writeFile(join(root, "video.mov"), "unsupported");

    const inventory = await inventorySource(root);
    assert.equal(inventory.complete, true);
    assert.deepEqual(
      inventory.files.map((file) => [file.relPath, file.extension, file.kind]),
      [["a.JPG", "jpg", "rendered"], [join("nested", "b.CR3"), "cr3", "raw"]],
    );
    assert.deepEqual(inventory.countsByExtension, { jpg: 1, cr3: 1 });

    const first = selectInventory(inventory.files, 1, "fixed");
    const second = selectInventory(inventory.files, 1, "fixed");
    assert.deepEqual(first, second);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("write targets can never point into immutable /homenas or the source", () => {
  assert.throws(
    () => assertSafePaths("/homenas/iris_images/RAW", "/homenas/stage", "/tmp/receipts"),
    /immutable \/homenas/,
  );
  assert.throws(
    () => assertSafePaths("/tmp/source", "/tmp/source/stage", "/tmp/receipts"),
    /inside the source tree/,
  );
});

test("all-RAW selection fills with rendered files deterministically", () => {
  const files = [
    { sourceName: "photos", relPath: "b.jpg", kind: "rendered" },
    { sourceName: "raw", relPath: "a.arw", kind: "raw" },
    { sourceName: "photos", relPath: "c.jpg", kind: "rendered" },
    { sourceName: "raw", relPath: "d.dng", kind: "raw" },
  ];
  const selected = selectInventory(files, 3, "fixed", true);
  assert.equal(selected.filter((file) => file.kind === "raw").length, 2);
  assert.equal(selected.filter((file) => file.kind === "rendered").length, 1);
  assert.deepEqual(selected, selectInventory(files, 3, "fixed", true));
});

test("dry-run emits versioned receipts and upserts one spreadsheet row", async () => {
  const root = await mkdtemp(join(tmpdir(), "pp-soak-dry-"));
  const source = join(root, "source");
  const receipts = join(root, "receipts");
  await mkdir(source);
  await writeFile(join(source, "one.arw"), "not-decoded-in-dry-tier");
  const script = new URL("./real-library-soak.mjs", import.meta.url);
  const args = [
    script.pathname,
    "--source", source,
    "--receipts", receipts,
    "--tier", "dry",
    "--run-id", "same-row",
  ];
  try {
    for (let attempt = 0; attempt < 2; attempt++) {
      const result = spawnSync(process.execPath, args, { encoding: "utf8" });
      assert.equal(result.status, 0, result.stderr);
    }
    const receipt = JSON.parse(await readFile(join(receipts, "same-row.json"), "utf8"));
    assert.equal(receipt.schema, 1);
    assert.equal(receipt.result, "passed");
    assert.equal(receipt.sourceValidation.unchanged, true);
    const csv = (await readFile(join(receipts, "soak-progress.v2.csv"), "utf8"))
      .trim()
      .split(/\r?\n/);
    assert.equal(csv.length, 2, "header plus one upserted run row");
    assert.match(csv[1], /^1,same-row,/);
    const finalRows = (await readFile(join(receipts, "soak-runs.v1.jsonl"), "utf8"))
      .trim()
      .split(/\r?\n/);
    assert.equal(finalRows.length, 2, "final JSONL remains append-only");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("free-space preflight fails before copying selected media", async () => {
  const root = await mkdtemp(join(tmpdir(), "pp-soak-preflight-"));
  const source = join(root, "source");
  const destination = join(root, "stage");
  const receipts = join(root, "receipts");
  await mkdir(source);
  await writeFile(join(source, "one.jpg"), "fixture");
  const script = new URL("./real-library-soak.mjs", import.meta.url);
  try {
    const result = spawnSync(process.execPath, [
      script.pathname,
      "--source", `smoke=${source}`,
      "--destination", destination,
      "--receipts", receipts,
      "--tier", "small",
      "--limit", "1",
      "--inventory-cap", "0",
      "--prepare-only",
      "--reserve-gib", "1000000",
      "--run-id", "preflight-failure",
    ], { encoding: "utf8" });
    assert.equal(result.status, 1);
    await assert.rejects(readFile(join(destination, "smoke", "one.jpg")));
    const receipt = JSON.parse(
      await readFile(join(receipts, "preflight-failure.json"), "utf8"),
    );
    assert.equal(receipt.result, "failed");
    assert.equal(receipt.phase, "failed");
    assert.match(receipt.error, /stage preflight needs/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
