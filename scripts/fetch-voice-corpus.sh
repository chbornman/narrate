#!/usr/bin/env bash
# fetch-voice-corpus.sh - stage a LibriSpeech split as the voice WER corpus.
#
# WHY: pp-voice-bench/pp-sweep voice score gating-cost (gated WER - raw WER)
# against a reference transcript. Alice ch1 gave us ONE reader; LibriSpeech is
# the standard ASR benchmark built from public-domain audiobooks, with many
# readers + clean per-utterance references + held-out splits (dev-clean to TUNE
# on, test-clean to REPORT on - never tune on test). This stages a split as
# per-chapter continuous recordings (one <spk>-<chap>.wav + .transcript.txt
# each, mirroring the Alice shape the bench already consumes) plus a manifest.
#
# The audio is large + gitignored (test-corpora/*); only this script is
# committed. Models are NOT needed to fetch/stage - only to later RUN the sweep
# (that happens on your machine where the ASR models live).
#
# Usage:
#   scripts/fetch-voice-corpus.sh [dev-clean|test-clean]   (default: test-clean)
# Idempotent: re-running skips an already-downloaded tarball and re-staged wavs.
set -euo pipefail

SPLIT="${1:-test-clean}"
case "$SPLIT" in
  dev-clean|test-clean|dev-other|test-other) ;;
  *) echo "unknown split: $SPLIT (use dev-clean|test-clean|dev-other|test-other)" >&2; exit 2 ;;
esac

# Resolve repo root from this script's location so it runs from anywhere.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/test-corpora/voice-long/librispeech"
DL="$DEST/_download"
URL="https://www.openslr.org/resources/12/${SPLIT}.tar.gz"
TARBALL="$DL/${SPLIT}.tar.gz"

command -v ffmpeg >/dev/null || { echo "ffmpeg required (brew install ffmpeg)" >&2; exit 2; }

mkdir -p "$DL"

echo "fetch-voice-corpus: downloading $SPLIT (resumable)..."
# -c resumes a partial download; the server supports byte ranges.
wget -c -O "$TARBALL" "$URL"

echo "fetch-voice-corpus: extracting..."
tar -xzf "$TARBALL" -C "$DL"
SRC="$DL/LibriSpeech/$SPLIT"
[ -d "$SRC" ] || { echo "expected $SRC after extract" >&2; exit 1; }

OUT="$DEST/$SPLIT"
mkdir -p "$OUT"
MANIFEST="$DEST/${SPLIT}-manifest.tsv"
: > "$MANIFEST"
printf '# id\twav\ttranscript\tutterances\n' >> "$MANIFEST"

echo "fetch-voice-corpus: concatenating chapters into continuous wav + transcript..."
chapters=0
# LibriSpeech layout: <split>/<speaker>/<chapter>/{<spk>-<chap>-NNNN.flac, <spk>-<chap>.trans.txt}
for chapdir in "$SRC"/*/*/; do
  [ -d "$chapdir" ] || continue
  spk="$(basename "$(dirname "$chapdir")")"
  chap="$(basename "$chapdir")"
  id="${spk}-${chap}"
  trans="$chapdir/${id}.trans.txt"
  [ -f "$trans" ] || { echo "  skip $id (no trans.txt)"; continue; }

  wav="$OUT/${id}.wav"
  ref="$OUT/${id}.transcript.txt"

  # Skip if already staged (idempotent re-runs).
  if [ -f "$wav" ] && [ -f "$ref" ]; then
    chapters=$((chapters+1)); continue
  fi

  # Concatenate the chapter's utterances IN ORDER (NNNN sort) into one 16 kHz
  # mono PCM16 wav. ffmpeg's concat demuxer decodes each flac and re-encodes.
  list="$(mktemp)"
  count=0
  while IFS= read -r f; do
    # concat demuxer needs absolute paths, single-quoted.
    printf "file '%s'\n" "$f" >> "$list"
    count=$((count+1))
  done < <(find "$chapdir" -name '*.flac' | sort)
  [ "$count" -gt 0 ] || { rm -f "$list"; continue; }

  ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i "$list" \
    -ar 16000 -ac 1 -sample_fmt s16 "$wav"
  rm -f "$list"

  # The reference: each trans.txt line is "<utt-id> WORDS..."; the line order
  # matches the sorted flac order. Drop the id, one utterance per line. The WER
  # normalizer folds case/punctuation, so plain text is enough.
  awk '{ $1=""; sub(/^[ \t]+/, ""); print }' "$trans" > "$ref"

  printf '%s\t%s\t%s\t%d\n' "$id" "$wav" "$ref" "$count" >> "$MANIFEST"
  chapters=$((chapters+1))
  echo "  staged $id ($count utterances)"
done

# A small fast SWEEP subset (the 3 shortest chapters by utterance count) so a
# config sweep iterates quickly; the full manifest is for final validation.
SUBSET="$DEST/${SPLIT}-sweep-subset.tsv"
grep -v '^#' "$MANIFEST" | sort -t$'\t' -k4 -n | head -3 > "$SUBSET" || true

total_audio=$(find "$OUT" -name '*.wav' | wc -l | tr -d ' ')
echo ""
echo "fetch-voice-corpus: done. $chapters chapters, $total_audio wavs staged under:"
echo "  $OUT"
echo "manifest:      $MANIFEST"
echo "sweep subset:  $SUBSET (3 shortest chapters, for fast config sweeps)"
echo ""
echo "Next (on a machine with the ASR models):"
echo "  pp-sweep voice --corpus-manifest $SUBSET --grid 'rule2=0.8,1.0,1.2,1.5' --propose tuning.proposed.toml"
echo "  (single-file form still works: pp-sweep voice --corpus <one.wav> --expect <one.txt> ...)"
echo ""
echo "To free space after staging, the raw tarball + extract can be removed:"
echo "  rm -rf $DL"
