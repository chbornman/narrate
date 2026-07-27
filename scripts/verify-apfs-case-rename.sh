#!/usr/bin/env bash
#
# Native S6 acceptance receipt. Run on a Mac whose temporary directory lives
# on default (case-insensitive) APFS:
#
#   ./scripts/verify-apfs-case-rename.sh \
#     docs/benchmarks/apfs-case-rename-$(date -u +%Y%m%dT%H%M%SZ).txt
#
# The optional argument records the complete proof output. Without it, the
# receipt is printed only to stdout.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "S6 receipt requires macOS" >&2
  exit 2
fi

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
receipt_path="${1:-}"
if [[ -n "$receipt_path" ]]; then
  case "$receipt_path" in
    /*) ;;
    *) receipt_path="$repo_dir/$receipt_path" ;;
  esac
  mkdir -p "$(dirname "$receipt_path")"
  exec > >(tee "$receipt_path") 2>&1
fi

probe_base="${TMPDIR:-/tmp}"
probe_dir="$(mktemp -d "$probe_base/photoproof-apfs-s6.XXXXXX")"
cleanup() {
  rm -r -- "$probe_dir"
}
trap cleanup EXIT

lower="$probe_dir/case_probe"
upper="$probe_dir/CASE_PROBE"
printf 'photoproof-s6\n' > "$lower"
if [[ ! -e "$upper" ]]; then
  echo "S6 receipt requires a case-insensitive volume; uppercase alias was absent" >&2
  exit 3
fi
actual_name="$(find "$probe_dir" -mindepth 1 -maxdepth 1 -print | sed 's#^.*/##')"
if [[ "$actual_name" != "case_probe" ]]; then
  echo "unexpected probe directory entry: $actual_name" >&2
  exit 4
fi

volume_device="$(df -P "$probe_dir" | awk 'END { print $1 }')"
disk_info="$(diskutil info "$volume_device")"
fs_personality="$(printf '%s\n' "$disk_info" | awk -F': *' '/File System Personality/ { print $2; exit }')"
if [[ "$fs_personality" != APFS* ]]; then
  echo "S6 receipt requires APFS; diskutil reported '$fs_personality'" >&2
  exit 5
fi

echo "Photoproof S6 default-APFS receipt"
echo "utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "host=$(scutil --get ComputerName)"
echo "macos=$(sw_vers -productVersion)"
echo "arch=$(uname -m)"
echo "volume_device=$volume_device"
echo "filesystem=$fs_personality"
echo "case_sensitive=false (probed by distinct-spelling lookup + byte-exact directory enumeration)"
echo "commit=$(git -C "$repo_dir" rev-parse HEAD)"
echo "worktree=$(git -C "$repo_dir" status --short | wc -l | tr -d ' ') changed paths"

cd "$repo_dir"
export TMPDIR="$probe_dir"

cargo test -p photoproof-core --test sidecars_acceptance \
  s02_2_case_only_rename_relinks_sidecar -- --exact
cargo test -p photoproof-core --test library_watcher \
  w12_macos_case_insensitive_scan_uses_platform_semantics -- --exact
cargo test -p photoproof-core --test library_watcher

echo "result=PASS"
