#!/usr/bin/env bash
# ASR engine A/B: sherpa-int8 (default) vs parakeet-rs Nemotron 3.5.
#
# WHY: PLAN-NEMOTRON-35-SIDECAR §7.4 gates a tier flip on per-final latency
# + peak RSS vs the live 560 ms int8 pipeline. pp-voice-bench gives WER +
# segments but not throughput/RSS, so this wraps it: it builds both server
# binaries, streams the same wav through each, samples the child server's
# peak RSS, and times wall-clock to derive RTF (audio_s / wall_s) and a
# per-chunk decode-latency proxy (560 ms / RTF). Portable bash so the M1 and
# margo run the byte-identical harness.
#
# Usage: scripts/asr-ab.sh [WAV] [EXPECT_TXT]
#   defaults to the Alice ch1 16k corpus (§7 corpus).
set -euo pipefail
cd "$(dirname "$0")/.."

WAV="${1:-test-corpora/voice-long/alice-ch1-16k.wav}"
EXPECT="${2:-test-corpora/voice-long/alice-ch1-transcript.txt}"

# Model dirs: sherpa = four-file int8 transducer; parakeet = its own FP32 dir.
# WHY env-overridable: the app-data models live outside the repo and differ
# per machine (macOS Application Support vs Linux XDG).
if [[ "$(uname)" == "Darwin" ]]; then
  MODELS="${PP_MODELS_DIR:-$HOME/Library/Application Support/com.photoproof.desktop/models}"
else
  MODELS="${PP_MODELS_DIR:-$HOME/.local/share/com.photoproof.desktop/models}"
fi
SHERPA_DIR="${PP_SHERPA_DIR:-$MODELS/nemotron-speech-streaming-en-0.6b-560ms-int8}"
PARAKEET_DIR="${PP_PARAKEET_DIR:-$MODELS/nemotron-3.5-asr-streaming-0.6b-parakeet}"

# The parakeet HF export nests the files one subdir deep (the repo subdir is
# preserved on download). from_pretrained wants the dir that holds config.json,
# so descend if it is not directly present.
if [[ -d "$PARAKEET_DIR" && ! -f "$PARAKEET_DIR/config.json" ]]; then
  inner=$(find "$PARAKEET_DIR" -maxdepth 2 -name config.json -print 2>/dev/null | head -1)
  [[ -n "$inner" ]] && PARAKEET_DIR="$(dirname "$inner")"
fi

# Audio duration in seconds (header math: bytes / (sr*ch*bytes_per_sample)).
# Falls back to a python-free `wc`-of-data estimate; here we read the WAV
# fmt/ data sizes via `od`. Simpler: trust 16k mono s16 and size on disk.
bytes=$(wc -c < "$WAV")
AUDIO_S=$(awk "BEGIN{printf \"%.1f\", ($bytes-44)/(16000*1*2)}")
echo "WAV=$WAV  (~${AUDIO_S}s audio)  EXPECT=$EXPECT"
echo

BENCH=target/release/pp_voice_bench
echo "=== building bench + both server binaries (release) ==="
cargo build --release -p photoproof-core --bin pp_voice_bench >/dev/null 2>&1
cargo build --release -p pp-asr-server >/dev/null 2>&1
cp target/release/pp-asr-server target/release/pp-asr-server-sherpa
cargo build --release -p pp-asr-server --features engine-parakeet >/dev/null 2>&1
cp target/release/pp-asr-server target/release/pp-asr-server-parakeet
echo "built."
echo

run_engine() {
  local name="$1" server="$2" model_dir="$3"
  if [[ ! -d "$model_dir" ]]; then
    echo "## $name: SKIP (model dir absent: $model_dir)"; echo; return
  fi
  pkill -f pp-asr-server >/dev/null 2>&1 || true
  sleep 1
  # Background peak-RSS sampler: find the child server PID, record max RSS(KB).
  # WHY 200ms: cheap enough not to steal CPU from a multi-minute decode run.
  local peakfile; peakfile=$(mktemp)
  echo 0 > "$peakfile"
  (
    while true; do
      pid=$(pgrep -f "pp-asr-server-${name}" | head -1 || true)
      if [[ -n "${pid:-}" ]]; then
        rss=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || true)
        if [[ -n "${rss:-}" ]]; then
          cur=$(cat "$peakfile")
          [[ "$rss" -gt "$cur" ]] && echo "$rss" > "$peakfile"
        fi
      fi
      sleep 0.1
    done
  ) &
  local sampler=$!

  # SINGLE clean pass: no --expect (which triple-streams via score_run's
  # gated+raw passes) so wall-clock is one honest decode of the audio. WER is
  # validated separately (PLAN-NEMOTRON-35-SIDECAR §10); this pass is the
  # §7.4 latency + peak-RSS gate.
  local t0 t1 wall segs
  t0=$(date +%s.%N)
  segs=$("$BENCH" --wav "$WAV" --model-dir "$model_dir" --server "$server" \
        2>/dev/null | grep -c 'entry\|onset' || true)
  t1=$(date +%s.%N)
  kill "$sampler" >/dev/null 2>&1 || true
  pkill -f pp-asr-server >/dev/null 2>&1 || true

  wall=$(awk "BEGIN{printf \"%.1f\", $t1-$t0}")
  local peak_kb; peak_kb=$(cat "$peakfile"); rm -f "$peakfile"
  local peak_mb; peak_mb=$(awk "BEGIN{printf \"%.0f\", $peak_kb/1024}")
  local rtf; rtf=$(awk "BEGIN{printf \"%.2f\", $AUDIO_S/$wall}")
  local chunk_ms; chunk_ms=$(awk "BEGIN{printf \"%.0f\", 560/($AUDIO_S/$wall)}")

  echo "## $name"
  echo "  wall:        ${wall}s   (audio ${AUDIO_S}s, single pass)"
  echo "  RTF:         ${rtf}x    (>1 = faster than real-time)"
  echo "  decode/chunk:~${chunk_ms}ms per 560ms chunk  (per-final tail proxy)"
  echo "  peak RSS:    ${peak_mb} MB"
  echo "  entries:     ${segs}"
  echo
}

echo "=== A/B: streaming $WAV through each engine ==="
echo
run_engine sherpa   target/release/pp-asr-server-sherpa   "$SHERPA_DIR"
run_engine parakeet target/release/pp-asr-server-parakeet "$PARAKEET_DIR"
echo "=== done ($(uname -m) $(uname -s)) ==="
