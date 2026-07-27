import { writeFileSync } from "node:fs";
import { resolve } from "node:path";

const output = process.argv[2];
if (!output) {
  throw new Error("usage: write-production-config.mjs <output.json>");
}

function required(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`production release is missing ${name}`);
  return value;
}

const endpointValue = required("PHOTOPROOF_UPDATE_ENDPOINT");
const endpoint = new URL(endpointValue);
if (endpoint.protocol !== "https:") {
  throw new Error("PHOTOPROOF_UPDATE_ENDPOINT must use HTTPS");
}
const pubkey = required("PHOTOPROOF_UPDATER_PUBLIC_KEY");
const platform = process.env.RUNNER_OS ?? process.platform;
const config = {
  bundle: {
    createUpdaterArtifacts: true,
    externalBin: ["binaries/pp-asr-server"],
  },
  plugins: {
    updater: {
      pubkey,
      // Preserve Tauri's literal {{target}}/{{arch}}/{{current_version}}
      // placeholders. URL.toString() percent-encodes the braces and silently
      // disables server-side target/version substitution.
      endpoints: [endpointValue],
      windows: {
        installMode: "passive",
      },
    },
  },
};

if (platform === "macOS" || platform === "darwin") {
  config.bundle.macOS = {
    signingIdentity: required("APPLE_SIGNING_IDENTITY"),
    minimumSystemVersion: "12.0",
  };
}
if (platform === "Windows" || platform === "win32") {
  const timestamp = new URL(required("WINDOWS_TIMESTAMP_URL"));
  if (!["https:", "http:"].includes(timestamp.protocol)) {
    throw new Error("WINDOWS_TIMESTAMP_URL must use HTTP or HTTPS");
  }
  config.bundle.windows = {
    certificateThumbprint: required("WINDOWS_CERTIFICATE_THUMBPRINT"),
    digestAlgorithm: "sha256",
    timestampUrl: timestamp.toString(),
  };
}

writeFileSync(resolve(output), `${JSON.stringify(config, null, 2)}\n`, {
  mode: 0o600,
});
console.log(`wrote production Tauri config for ${platform}`);
