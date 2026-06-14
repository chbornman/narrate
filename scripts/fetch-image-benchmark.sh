#!/usr/bin/env bash
# fetch-image-benchmark.sh - stage the MS-COCO 5K image-text retrieval benchmark
# as a folder of real photos + a normalized captions.json, for tuning SEARCH
# ranking against vetted ground truth (the search analog of the LibriSpeech voice
# corpus).
#
# WHY a real benchmark, not fake notes: a fake note planted on a photo just tests
# whether search finds the text you injected (a tautology), and skips the visual
# signal entirely. COCO is real photos + human-written captions: each caption is a
# QUERY whose right answer is the photo it describes - that honestly tests the
# CLIP visual + semantic retrieval the app actually does. The benchmark photos go
# into a SEPARATE eval library (pp-eval-ingest), so the real journal stays clean.
#
# Output (gitignored under test-corpora/retrieval/*; only this script is committed):
#   coco5k/images/<file>.jpg     N real photos
#   coco5k/captions.json         {"<file>.jpg": ["caption 1", ... 5 captions], ...}
# Then: pp-eval-ingest --images coco5k/images --lib /tmp/coco-eval-lib \
#         --captions coco5k/captions.json --out-golden test-corpora/retrieval/coco-golden.json
#
# Usage: scripts/fetch-image-benchmark.sh [N|all]   (default 1000; "all" = 5000)
# 1000 photos = 5000 caption-queries, plenty for a sweep and faster to embed.
set -euo pipefail

N="${1:-1000}"
for tool in wget unzip python3; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 2; }
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/test-corpora/retrieval/coco5k"
DL="$DEST/_download"
BASE="https://huggingface.co/datasets/nlphuji/mscoco_2014_5k_test_image_text_retrieval/resolve/main"
IMAGES_OUT="$DEST/images"
CAPTIONS_OUT="$DEST/captions.json"

mkdir -p "$DL" "$IMAGES_OUT"

echo "fetch-image-benchmark: downloading COCO 5K (818MB images + captions, resumable)..." >&2
wget -c -O "$DL/images.zip" "$BASE/images_mscoco_2014_5k_test.zip"
wget -c -O "$DL/captions.csv" "$BASE/test_5k_mscoco_2014.csv"

echo "fetch-image-benchmark: extracting images..." >&2
if [ ! -d "$DL/extracted" ]; then
  unzip -q -o "$DL/images.zip" -d "$DL/extracted"
fi

echo "fetch-image-benchmark: staging $N images + normalized captions..." >&2
N="$N" EXTRACTED="$DL/extracted" CSV="$DL/captions.csv" \
IMAGES_OUT="$IMAGES_OUT" CAPTIONS_OUT="$CAPTIONS_OUT" python3 - <<'PY'
import csv, ast, json, os, shutil, sys

csv.field_size_limit(10**7)
n_arg = os.environ["N"]
extracted = os.environ["EXTRACTED"]
csv_path = os.environ["CSV"]
images_out = os.environ["IMAGES_OUT"]
captions_out = os.environ["CAPTIONS_OUT"]

# Index every extracted image by basename so we can find it wherever it sits.
found = {}
for dirpath, _dirs, files in os.walk(extracted):
    for fn in files:
        if fn.lower().endswith((".jpg", ".jpeg", ".png")):
            found[fn] = os.path.join(dirpath, fn)

rows = []
with open(csv_path, newline="") as f:
    for row in csv.DictReader(f):
        rows.append((row["filename"], row["raw"]))
# Deterministic order (sorted by filename) so a subset is reproducible.
rows.sort(key=lambda r: r[0])
limit = len(rows) if n_arg == "all" else min(int(n_arg), len(rows))

captions = {}
staged = 0
missing = 0
for filename, raw in rows[:limit]:
    src = found.get(filename)
    if src is None:
        missing += 1
        continue
    caps = ast.literal_eval(raw)              # list of 5 caption strings
    caps = [c.strip() for c in caps if c and c.strip()]
    if not caps:
        continue
    shutil.copyfile(src, os.path.join(images_out, filename))
    captions[filename] = caps
    staged += 1

with open(captions_out, "w") as f:
    json.dump(captions, f, indent=0)

total_caps = sum(len(v) for v in captions.values())
print(f"  staged {staged} images, {total_caps} caption-queries"
      + (f" ({missing} requested files not in the zip)" if missing else ""), file=sys.stderr)
PY

echo "" >&2
echo "fetch-image-benchmark: done." >&2
echo "  images:   $IMAGES_OUT" >&2
echo "  captions: $CAPTIONS_OUT" >&2
echo "" >&2
echo "Next (needs the CLIP model; on your machine):" >&2
echo "  pp-eval-ingest --images $IMAGES_OUT --lib /tmp/coco-eval-lib \\" >&2
echo "    --captions $CAPTIONS_OUT --out-golden $ROOT/test-corpora/retrieval/coco-golden.json" >&2
echo "  pp-sweep search --db /tmp/coco-eval-lib/photoproof.db \\" >&2
echo "    --queries $ROOT/test-corpora/retrieval/coco-golden.json --grid 's4=0.5,1,1.5' --propose p.toml" >&2
echo "" >&2
echo "Reclaim space after staging: rm -rf $DL" >&2
