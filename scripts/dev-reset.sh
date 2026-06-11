#!/usr/bin/env bash
# Nuke the DEV library state: the app-data dir holding photoproof.db,
# the preview cache, and window state. Your photos are untouched — and
# so are the .pp.json sidecars and .photoproof-volume markers that live
# BESIDE them (sidecars ARE the journal data; the database is just the
# rebuildable index, SCOPE §storage). Pass --sidecars <folder> to sweep
# those from a folder too, when you really mean "forget everything".
set -euo pipefail

case "$(uname -s)" in
  Darwin) APP_DATA="$HOME/Library/Application Support/com.photoproof.desktop" ;;
  Linux) APP_DATA="${XDG_DATA_HOME:-$HOME/.local/share}/com.photoproof.desktop" ;;
  *) echo "unsupported platform" >&2; exit 1 ;;
esac

if pgrep -f photoproof-desktop >/dev/null 2>&1; then
  echo "stopping running app…"
  pkill -f photoproof-desktop || true
  sleep 1
fi

if [ -d "$APP_DATA" ]; then
  echo "removing $APP_DATA"
  rm -rf "$APP_DATA"
else
  echo "nothing at $APP_DATA"
fi

if [ "${1:-}" = "--sidecars" ] && [ -n "${2:-}" ]; then
  echo "sweeping sidecars + volume markers under $2"
  find "$2" -name '*.pp.json' -delete
  find "$2" -maxdepth 1 -name '.photoproof-volume' -delete
fi

echo "done — next launch starts from an empty library."
