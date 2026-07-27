#!/usr/bin/env bash
set -euo pipefail

signing_dir="${RUNNER_TEMP}/photoproof-apple-signing"
certificate_path="${signing_dir}/certificate.p12"
keychain_path="${signing_dir}/build.keychain-db"
api_key_path="${signing_dir}/AuthKey_${APPLE_API_KEY}.p8"
mkdir -p "${signing_dir}"
printf '%s' "${APPLE_CERTIFICATE}" | base64 --decode > "${certificate_path}"
printf '%s' "${APPLE_API_PRIVATE_KEY}" | base64 --decode > "${api_key_path}"
security create-keychain -p "${APPLE_KEYCHAIN_PASSWORD}" "${keychain_path}"
security set-keychain-settings -lut 21600 "${keychain_path}"
security unlock-keychain -p "${APPLE_KEYCHAIN_PASSWORD}" "${keychain_path}"
security import "${certificate_path}" \
  -P "${APPLE_CERTIFICATE_PASSWORD}" \
  -A \
  -T /usr/bin/codesign \
  -k "${keychain_path}"
security set-key-partition-list \
  -S apple-tool:,apple:,codesign: \
  -s \
  -k "${APPLE_KEYCHAIN_PASSWORD}" \
  "${keychain_path}"
security default-keychain -d user -s "${keychain_path}"
identity="$(
  security find-identity -v -p codesigning "${keychain_path}" \
    | sed -n '/Developer ID Application/s/.*"\(.*\)"/\1/p' \
    | head -n 1
)"
if [[ -z "${identity}" ]]; then
  echo "no Developer ID Application signing identity was imported" >&2
  exit 1
fi
{
  printf 'APPLE_SIGNING_IDENTITY=%s\n' "${identity}"
  printf 'APPLE_API_KEY_PATH=%s\n' "${api_key_path}"
} >> "${GITHUB_ENV}"
