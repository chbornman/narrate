#!/usr/bin/env bash
# Stage a LibriSpeech split as a voice-sweep corpus: per-CHAPTER continuous
# recordings (the chapter's utterances concatenated in reading order, with a
# short silence between each so the VAD has real endpoints to find) + a MANIFEST
# TSV that `pp-sweep voice --corpus-manifest` consumes.
#
# WHY a whole split (many readers), not one chapter: a single chapter is one
# voice, one mic, one reading speed; tuning the [voice] dials on it overfits to
# that reader. Scoring every config over a split and ranking on the TOKEN-
# WEIGHTED corpus WER (total edits / total reference tokens across all chapters)
# is far more robust. The audio + transcripts are gitignored; only this staging
# script and the manifest format are committed.
#
#   scripts/fetch-voice-corpus.sh dev-clean   # stage a split into ./voice-corpus
#   scripts/fetch-voice-corpus.sh dev-clean ~/data/voice-corpus
#
# Output (under <out>/<split>/):
#   <id>.wav          one PCM16 mono 16 kHz recording per chapter (ffmpeg-normalized)
#   <id>.txt          the chapter transcript, one utterance per line
#   <split>.tsv               the full manifest (every chapter)
#   <split>-sweep-subset.tsv  the 3 SHORTEST chapters, for fast iteration
#
# Manifest format (TSV; `#` lines are comments/header; paths are absolute):
#   # id<TAB>wav<TAB>transcript<TAB>utterances
#   <id><TAB><abs-wav-path><TAB><abs-transcript-path><TAB><utterance-count>
#
# The transcript is plain text (one utterance per line); the WER normalizer in
# pp-sweep folds case/punctuation, so the transcript stays human-readable.
set -euo pipefail
cd "$(dirname "$0")/.."

SPLIT="${1:-dev-clean}"
OUT="${2:-voice-corpus}"

# LibriSpeech mirror. The split name maps to the OpenSLR tarball.
BASE_URL="https://www.openslr.org/resources/12"
case "$SPLIT" in
  dev-clean)   TARBALL="dev-clean.tar.gz" ;;
  dev-other)   TARBALL="dev-other.tar.gz" ;;
  test-clean)  TARBALL="test-clean.tar.gz" ;;
  test-other)  TARBALL="test-other.tar.gz" ;;
  *) echo "unknown split: $SPLIT (want dev-clean|dev-other|test-clean|test-other)" >&2; exit 2 ;;
esac

for tool in curl tar ffmpeg; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 2; }
done

WORK="$OUT/_download"
DEST="$OUT/$SPLIT"
mkdir -p "$WORK" "$DEST"

# Download + extract once (idempotent: skip if the split tree is already there).
if [ ! -d "$WORK/LibriSpeech/$SPLIT" ]; then
  echo "downloading $TARBALL ..." >&2
  curl -fSL "$BASE_URL/$TARBALL" -o "$WORK/$TARBALL"
  echo "extracting ..." >&2
  tar -xzf "$WORK/$TARBALL" -C "$WORK"
fi
SRC="$WORK/LibriSpeech/$SPLIT"

MANIFEST="$DEST/$SPLIT.tsv"
{
  echo -e "# id\twav\ttranscript\tutterances"
} > "$MANIFEST"

# A chapter is reader/<reader>/<chapter>/. Its <reader>-<chapter>.trans.txt has
# one line per utterance: "<utt-id> THE TRANSCRIPT TEXT". We concatenate the
# chapter's flac utterances (in id order) with a 0.5 s silence gap, normalize to
# PCM16 mono 16 kHz, and write the transcript one utterance per line.
GAP_S=0.5
declare -a SUBSET_ROWS=()   # "tokens<TAB>manifest-row" for the shortest-N subset

while IFS= read -r trans; do
  dir="$(dirname "$trans")"
  reader="$(basename "$(dirname "$dir")")"
  chapter="$(basename "$dir")"
  id="${reader}-${chapter}"

  wav_abs="$(cd "$DEST" && pwd)/$id.wav"
  txt_abs="$(cd "$DEST" && pwd)/$id.txt"

  # Build the per-utterance flac list (in transcript order) + the transcript.
  : > "$txt_abs"
  concat_list="$WORK/$id.concat.txt"
  : > "$concat_list"
  silence="$WORK/_silence.wav"
  [ -f "$silence" ] || ffmpeg -nostdin -hide_banner -loglevel error -y \
    -f lavfi -i "anullsrc=r=16000:cl=mono" -t "$GAP_S" -acodec pcm_s16le "$silence"

  count=0
  tokens=0
  while IFS= read -r line; do
    utt_id="${line%% *}"
    text="${line#* }"
    flac="$dir/$utt_id.flac"
    [ -f "$flac" ] || continue
    # Normalize each utterance to the wire format, then concat with a gap.
    norm="$WORK/$id.$utt_id.wav"
    ffmpeg -nostdin -hide_banner -loglevel error -y \
      -i "$flac" -ar 16000 -ac 1 -sample_fmt s16 "$norm"
    echo "file '$norm'" >> "$concat_list"
    echo "file '$silence'" >> "$concat_list"
    echo "$text" >> "$txt_abs"
    count=$((count + 1))
    tokens=$((tokens + $(echo "$text" | wc -w)))
  done < "$trans"

  [ "$count" -gt 0 ] || continue
  ffmpeg -nostdin -hide_banner -loglevel error -y \
    -f concat -safe 0 -i "$concat_list" -ar 16000 -ac 1 -sample_fmt s16 "$wav_abs"
  rm -f "$concat_list"

  row="$(printf '%s\t%s\t%s\t%s' "$id" "$wav_abs" "$txt_abs" "$count")"
  echo "$row" >> "$MANIFEST"
  SUBSET_ROWS+=("$(printf '%s\t%s' "$tokens" "$row")")
done < <(find "$SRC" -name '*.trans.txt' | sort)

# The sweep-subset is the 3 SHORTEST chapters (fewest reference tokens) so an
# iteration loop on the dials runs in seconds before a full-split confirmation.
SUBSET="$DEST/$SPLIT-sweep-subset.tsv"
{
  echo -e "# id\twav\ttranscript\tutterances"
  printf '%s\n' "${SUBSET_ROWS[@]}" | sort -n | head -n 3 | cut -f2-
} > "$SUBSET"

echo "wrote manifest: $MANIFEST ($(($(wc -l < "$MANIFEST") - 1)) chapters)" >&2
echo "wrote sweep subset (3 shortest): $SUBSET" >&2
