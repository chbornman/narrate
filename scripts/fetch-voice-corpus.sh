#!/usr/bin/env bash
# fetch-voice-corpus.sh - stage a LibriSpeech split as the voice WER corpus.
#
# WHY: pp-voice-bench / pp-sweep voice score gating-cost (gated WER - raw WER)
# against a reference transcript. Alice ch1 gave us ONE reader; LibriSpeech is
# the standard ASR benchmark built from public-domain audiobooks, with many
# readers + clean per-utterance references + held-out splits (dev-clean to TUNE
# on, test-clean to REPORT on - never tune on test).
#
# CRITICAL - the silence gaps: each chapter is its utterances concatenated in
# reading order WITH a 0.5 s silence between them. Without that silence the VAD
# gate never closes and the endpoint rules (rule2 trailing-silence, the
# snappiness dial) never fire - so a [voice] sweep over rule2/hysteresis would be
# a no-op. The gaps give the endpointer real boundaries to find, so the sweep
# measures realistic truncation.
#
# Output (gitignored under test-corpora/*; only this script is committed):
#   <split>/<id>.wav              one PCM16 mono 16 kHz recording per chapter
#   <split>/<id>.transcript.txt   the chapter transcript, one utterance per line
#   <split>-manifest.tsv          the full manifest (every chapter)
#   <split>-sweep-subset.tsv      the 3 shortest chapters (fewest tokens), fast iteration
# Manifest format (TSV; `#` lines are comments/header; paths absolute):
#   # id<TAB>wav<TAB>transcript<TAB>utterances
#
# Models are NOT needed to fetch/stage - only to later RUN the sweep (on a
# machine with the ASR models).
#
# Usage: scripts/fetch-voice-corpus.sh [dev-clean|test-clean|dev-other|test-other]
# Idempotent: skips an already-downloaded tarball; restages missing chapters.
set -euo pipefail

SPLIT="${1:-test-clean}"
case "$SPLIT" in
  dev-clean|test-clean|dev-other|test-other) ;;
  *) echo "unknown split: $SPLIT (use dev-clean|test-clean|dev-other|test-other)" >&2; exit 2 ;;
esac

for tool in wget tar ffmpeg; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 2; }
done

# Resolve repo root from this script's location so it runs from anywhere.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/test-corpora/voice-long/librispeech"
DL="$DEST/_download"
URL="https://www.openslr.org/resources/12/${SPLIT}.tar.gz"
TARBALL="$DL/${SPLIT}.tar.gz"
GAP_S=0.5

mkdir -p "$DL"

# Download + extract once (resumable; idempotent on the extracted tree).
if [ ! -d "$DL/LibriSpeech/$SPLIT" ]; then
  echo "fetch-voice-corpus: downloading $SPLIT (resumable)..." >&2
  wget -c -O "$TARBALL" "$URL"
  echo "fetch-voice-corpus: extracting..." >&2
  tar -xzf "$TARBALL" -C "$DL"
fi
SRC="$DL/LibriSpeech/$SPLIT"
[ -d "$SRC" ] || { echo "expected $SRC after extract" >&2; exit 1; }

OUT="$DEST/$SPLIT"
mkdir -p "$OUT"
OUT_ABS="$(cd "$OUT" && pwd)"
MANIFEST="$DEST/${SPLIT}-manifest.tsv"
printf '# id\twav\ttranscript\tutterances\n' > "$MANIFEST"

# One reusable 0.5 s silence clip to splice between utterances.
SILENCE="$DL/_silence.wav"
[ -f "$SILENCE" ] || ffmpeg -nostdin -hide_banner -loglevel error -y \
  -f lavfi -i "anullsrc=r=16000:cl=mono" -t "$GAP_S" -acodec pcm_s16le "$SILENCE"

echo "fetch-voice-corpus: concatenating chapters (utterances + ${GAP_S}s gaps)..." >&2
declare -a SUBSET_ROWS=()   # "tokens<TAB>manifest-row" for the shortest-N subset
chapters=0

# A chapter is <reader>/<chapter>/ with a <reader>-<chapter>.trans.txt whose
# lines are "<utt-id> THE TRANSCRIPT TEXT", one per utterance in id order.
while IFS= read -r trans; do
  dir="$(dirname "$trans")"
  reader="$(basename "$(dirname "$dir")")"
  chapter="$(basename "$dir")"
  id="${reader}-${chapter}"
  wav="$OUT_ABS/${id}.wav"
  ref="$OUT_ABS/${id}.transcript.txt"

  concat_list="$DL/${id}.concat.txt"
  : > "$concat_list"
  : > "$ref"
  count=0
  tokens=0
  while IFS= read -r line; do
    utt_id="${line%% *}"
    text="${line#* }"
    flac="$dir/$utt_id.flac"
    [ -f "$flac" ] || continue
    # Normalize each utterance to the wire format, then concat utterance + gap.
    norm="$DL/${id}.${utt_id}.wav"
    ffmpeg -nostdin -hide_banner -loglevel error -y \
      -i "$flac" -ar 16000 -ac 1 -sample_fmt s16 "$norm"
    printf "file '%s'\n" "$norm" >> "$concat_list"
    printf "file '%s'\n" "$SILENCE" >> "$concat_list"
    printf '%s\n' "$text" >> "$ref"
    count=$((count + 1))
    tokens=$((tokens + $(printf '%s' "$text" | wc -w)))
  done < "$trans"
  [ "$count" -gt 0 ] || { rm -f "$concat_list"; continue; }

  ffmpeg -nostdin -hide_banner -loglevel error -y \
    -f concat -safe 0 -i "$concat_list" -ar 16000 -ac 1 -sample_fmt s16 "$wav"
  rm -f "$concat_list" "$DL/${id}".*.wav

  row="$(printf '%s\t%s\t%s\t%s' "$id" "$wav" "$ref" "$count")"
  printf '%s\n' "$row" >> "$MANIFEST"
  SUBSET_ROWS+=("$(printf '%s\t%s' "$tokens" "$row")")
  chapters=$((chapters + 1))
done < <(find "$SRC" -name '*.trans.txt' | sort)

# Fast sweep subset: the 3 shortest chapters (fewest reference tokens) so a dial
# iteration runs in seconds before a full-split confirmation.
SUBSET="$DEST/${SPLIT}-sweep-subset.tsv"
{
  printf '# id\twav\ttranscript\tutterances\n'
  printf '%s\n' "${SUBSET_ROWS[@]}" | sort -n | head -n 3 | cut -f2-
} > "$SUBSET"

echo "" >&2
echo "fetch-voice-corpus: done. $chapters chapters staged under $OUT" >&2
echo "manifest:      $MANIFEST" >&2
echo "sweep subset:  $SUBSET (3 shortest chapters)" >&2
echo "" >&2
echo "Next (on a machine with the ASR models):" >&2
echo "  pp-sweep voice --corpus-manifest $SUBSET --grid 'rule2=0.8,1.0,1.2,1.5' --propose tuning.proposed.toml" >&2
echo "To reclaim space after staging: rm -rf $DL" >&2
