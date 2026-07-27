function required(name) {
  if (!process.env[name]?.trim()) {
    throw new Error(`production release is missing ${name}`);
  }
}

for (const name of [
  "PHOTOPROOF_UPDATE_ENDPOINT",
  "PHOTOPROOF_UPDATER_PUBLIC_KEY",
  "TAURI_SIGNING_PRIVATE_KEY",
  "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
]) {
  required(name);
}

const platform = process.env.RUNNER_OS ?? process.platform;
if (platform === "macOS" || platform === "darwin") {
  for (const name of [
    "APPLE_CERTIFICATE",
    "APPLE_CERTIFICATE_PASSWORD",
    "APPLE_KEYCHAIN_PASSWORD",
    "APPLE_API_ISSUER",
    "APPLE_API_KEY",
    "APPLE_API_PRIVATE_KEY",
  ]) {
    required(name);
  }
}
if (platform === "Windows" || platform === "win32") {
  for (const name of [
    "WINDOWS_CERTIFICATE",
    "WINDOWS_CERTIFICATE_PASSWORD",
    "WINDOWS_TIMESTAMP_URL",
  ]) {
    required(name);
  }
}
console.log(`production release credentials are present for ${platform}`);
