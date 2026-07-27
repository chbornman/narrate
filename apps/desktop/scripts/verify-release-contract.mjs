import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const workspace = resolve(import.meta.dirname, "../../..");
const readJson = (path) => JSON.parse(readFileSync(resolve(workspace, path), "utf8"));
const rootCargo = readFileSync(resolve(workspace, "Cargo.toml"), "utf8");
const schemaSource = readFileSync(
  resolve(workspace, "crates/photoproof-core/src/store/schema.rs"),
  "utf8",
);
const packageJson = readJson("apps/desktop/package.json");
const tauriConfig = readJson("apps/desktop/src-tauri/tauri.conf.json");
const contract = readJson("docs/releases/release-contract.json");

const cargoVersion = rootCargo.match(
  /^\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
)?.[1];
const schemaVersion = Number(
  schemaSource.match(/CURRENT_VERSION:\s*i64\s*=\s*(\d+)/)?.[1],
);
const versions = new Set([
  cargoVersion,
  packageJson.version,
  tauriConfig.version,
  contract.version,
]);
if (versions.size !== 1 || versions.has(undefined)) {
  throw new Error(`release versions disagree: ${[...versions].join(", ")}`);
}
if (!Number.isInteger(schemaVersion) || contract.database.schema !== schemaVersion) {
  throw new Error(
    `release contract schema ${contract.database.schema} does not match code ${schemaVersion}`,
  );
}
if (contract.database.migrationsVerified !== true) {
  throw new Error("release contract must explicitly attest migration verification");
}
if (contract.database.downgradeSupported !== false) {
  throw new Error("release contract must not claim an unsupported database downgrade");
}
if (!contract.rollback?.trim() || !contract.database.compatibility?.trim()) {
  throw new Error("release contract needs explicit compatibility and rollback text");
}

const notesPath = resolve(workspace, `docs/releases/${contract.version}.md`);
const notes = readFileSync(notesPath, "utf8");
for (const heading of [
  "# Photoproof",
  "## Changes",
  "## Database compatibility",
  "## Rollback",
]) {
  if (!notes.includes(heading)) {
    throw new Error(`${notesPath} is missing ${heading}`);
  }
}
const tag = process.env.GITHUB_REF_NAME;
if (tag && tag !== `desktop-v${contract.version}`) {
  throw new Error(
    `release tag ${tag} does not match desktop-v${contract.version}`,
  );
}
console.log(
  `release contract verified: v${contract.version}, database schema ${schemaVersion}`,
);
