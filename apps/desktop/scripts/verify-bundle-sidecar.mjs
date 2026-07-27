import { readdirSync, statSync } from "node:fs";
import { basename, join, resolve } from "node:path";

const root = resolve(process.argv[2] ?? "../../target/release/bundle");
const matches = [];

function visit(path) {
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.isDirectory()) visit(child);
    else if (
      entry.isFile() &&
      (entry.name === "pp-asr-server" || entry.name === "pp-asr-server.exe")
    ) {
      matches.push(child);
    }
  }
}

visit(root);
if (matches.length === 0) {
  throw new Error(`no packaged pp-asr-server found below ${root}`);
}
for (const path of matches) {
  const bytes = statSync(path).size;
  if (bytes < 1_000_000) {
    throw new Error(`packaged sidecar is implausibly small (${bytes} bytes): ${path}`);
  }
  console.log(`verified packaged ${basename(path)} (${bytes} bytes): ${path}`);
}
