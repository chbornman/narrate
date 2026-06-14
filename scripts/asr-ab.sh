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

# Authoritative peak RSS via the kernel high-water mark (ru_maxrss), NOT `ps`
# sampling: `ps rss` does not count mmap'd external-data pages (the parakeet
# FP32 encoder.onnx.data is mmap'd), so it under-reports by >10x. `/usr/bin/time`
# reports ru_maxrss even when the child is signalled. Portable across macOS
# (`time -l`, bytes) and GNU/Linux (`time -v`, kbytes). Loads the model then
# TERMs it; model residency dominates peak (streaming adds only small buffers).
peak_rss_mb() { # server args...  -> echoes MB
  local bin="$1"; shift
  local slog; slog=$(mktemp)
  local i
  if [[ "$(uname)" == "Darwin" ]]; then
    # macOS: /usr/bin/time -l reports ru_maxrss (bytes) for the signalled child.
    local tlog; tlog=$(mktemp)
    /usr/bin/time -l "$bin" "$@" >"$slog" 2>"$tlog" &
    local tpid=$!
    for i in $(seq 1 80); do grep -q READY "$slog" 2>/dev/null && break; sleep 0.5; done
    sleep 2  # settle past load
    local spid; spid=$(pgrep -P "$tpid" | head -1 || true)
    [[ -n "$spid" ]] && kill -TERM "$spid" 2>/dev/null || true
    wait "$tpid" 2>/dev/null || true
    grep 'maximum resident set size' "$tlog" | awk '{printf "%.0f", $1/1048576}'
    rm -f "$tlog"
  else
    # Linux: /proc/<pid>/status VmHWM is the kernel peak-RSS high-water mark
    # (== ru_maxrss), counts mmap'd resident pages, needs no GNU time package.
    "$bin" "$@" >"$slog" 2>&1 &
    local spid=$!
    for i in $(seq 1 80); do grep -q READY "$slog" 2>/dev/null && break; sleep 0.5; done
    sleep 2  # settle past load
    awk '/^VmHWM:/{printf "%.0f", $2/1024}' "/proc/$spid/status" 2>/dev/null
    kill -TERM "$spid" 2>/dev/null || true
    wait "$spid" 2>/dev/null || true
  fi
  rm -f "$slog"
}

run_engine() {
  local name="$1" server="$2" model_dir="$3"
  if [[ ! -d "$model_dir" ]]; then
    echo "## $name: SKIP (model dir absent: $model_dir)"; echo; return
  fi
  # Engine-specific server args: sherpa reads the four int8 paths; parakeet
  # reads --model-dir (the other flags are inert for it). One spawner, two shapes.
  local srv_args
  if [[ "$name" == "parakeet" ]]; then
    srv_args=(--port 0 --encoder x --decoder x --joiner x --tokens x --model-dir "$model_dir" --lang en)
  else
    srv_args=(--port 0 --encoder "$model_dir/encoder.int8.onnx" --decoder "$model_dir/decoder.int8.onnx" \
              --joiner "$model_dir/joiner.int8.onnx" --tokens "$model_dir/tokens.txt" --model-dir "$model_dir")
  fi

  pkill -f pp-asr-server >/dev/null 2>&1 || true
  sleep 1
  # 1) RTF: ONE clean streamed pass (no --expect; score_run would triple-stream).
  #    WER is validated separately (PLAN-NEMOTRON-35-SIDECAR §10).
  local t0 t1 wall
  t0=$(date +%s.%N)
  "$BENCH" --wav "$WAV" --model-dir "$model_dir" --server "$server" >/dev/null 2>&1 || true
  t1=$(date +%s.%N)
  pkill -f pp-asr-server >/dev/null 2>&1 || true
  local wall; wall=$(awk "BEGIN{printf \"%.1f\", $t1-$t0}")
  local rtf; rtf=$(awk "BEGIN{printf \"%.2f\", $AUDIO_S/$wall}")
  local chunk_ms; chunk_ms=$(awk "BEGIN{printf \"%.0f\", 560/($AUDIO_S/$wall)}")

  # 2) peak RSS: authoritative ru_maxrss (ps cannot see mmap'd weights).
  sleep 1
  local peak_mb; peak_mb=$(peak_rss_mb "$server" "${srv_args[@]}")
  pkill -f pp-asr-server >/dev/null 2>&1 || true

  echo "## $name"
  echo "  wall:        ${wall}s   (audio ${AUDIO_S}s, single pass)"
  echo "  RTF:         ${rtf}x    (>1 = faster than real-time)"
  echo "  decode/chunk:~${chunk_ms}ms per 560ms chunk  (per-final tail proxy)"
  echo "  peak RSS:    ${peak_mb} MB   (ru_maxrss, counts mmap'd weights)"
  echo
}

echo "=== A/B: streaming $WAV through each engine ==="
echo
run_engine sherpa   target/release/pp-asr-server-sherpa   "$SHERPA_DIR"
run_engine parakeet target/release/pp-asr-server-parakeet "$PARAKEET_DIR"
echo "=== done ($(uname -m) $(uname -s)) ==="
