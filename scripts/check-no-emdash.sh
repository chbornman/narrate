#!/usr/bin/env bash
#
# check-no-emdash.sh — creep gate for the "no em-dashes in user-visible UI copy"
# project rule (CLAUDE.md). A one-time sweep already de-dashed visible strings
# (landed ddb0e86); this gate stops new ones from landing in rendered text.
#
# WHY a heuristic and not a parser: a full Svelte/TS parse is overkill for a
# lint that only needs to separate "rendered text" from "code comments and one
# known sentinel". We scope narrowly to keep false positives at zero on the
# current tree while still catching dashes that reach the screen.
#
# Scope (apps/desktop/src):
#   .svelte — flag — / – in template markup and rendered attribute values,
#             AFTER stripping <script>…</script>, <style>…</style> and <!-- -->.
#   .ts/.js — flag — / – inside code lines, AFTER dropping // and /* */ comments
#             (where U+2014/U+2013 only live in string literals or sentinels in
#             this codebase). The menus.ts separator sentinel (the bare "—"
#             order-table entry that renders as <hr/>) is allowlisted by its
#             exact forms, not by exempting the whole file.
#
# Output on violation: `file:line: <offending line>` to stderr, exit 1.
# Clean tree: exit 0, silent.

set -euo pipefail

# Resolve repo root from this script's location so the gate runs from anywhere.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SRC="$ROOT/apps/desktop/src"

if [ ! -d "$SRC" ]; then
  echo "check-no-emdash: source dir not found: $SRC" >&2
  exit 2
fi

EMDASH=$'—' # —
ENDASH=$'–' # –

# One awk program, invoked once per file so the multi-line state flags
# (in_block / in_script / in_style / in_comment) reset cleanly between files.
SCAN_AWK='
function visible(s) { return (index(s, EM) > 0 || index(s, EN) > 0) }
function report(s)  { printf("%s:%d: %s\n", FILENAME, FNR, s) > "/dev/stderr"; found = 1 }

# ---- .svelte: strip <script>/<style>/<!-- --> then flag dashes in markup ----
ext == "svelte" {
  orig = $0; line = $0
  while (1) {
    if (in_script) {
      p = index(tolower(line), "</script>"); if (p == 0) next
      line = substr(line, p + 9); in_script = 0
    } else if (in_style) {
      p = index(tolower(line), "</style>"); if (p == 0) next
      line = substr(line, p + 8); in_style = 0
    } else if (in_comment) {
      p = index(line, "-->"); if (p == 0) next
      line = substr(line, p + 3); in_comment = 0
    } else {
      lc = tolower(line)
      ps = index(lc, "<script"); py = index(lc, "<style"); pc = index(line, "<!--")
      first = 0; which = ""
      if (ps > 0)               { first = ps; which = "script" }
      if (py > 0 && (first == 0 || py < first)) { first = py; which = "style" }
      if (pc > 0 && (first == 0 || pc < first)) { first = pc; which = "comment" }
      if (first == 0) break
      if (visible(substr(line, 1, first - 1))) { report(orig); break }
      rest = substr(line, first)
      if (which == "script")      { cp = index(tolower(rest), "</script>"); if (cp == 0) { in_script = 1; next } line = substr(rest, cp + 9) }
      else if (which == "style")  { cp = index(tolower(rest), "</style>");  if (cp == 0) { in_style = 1;  next } line = substr(rest, cp + 8) }
      else                        { cp = index(rest, "-->");                if (cp == 0) { in_comment = 1; next } line = substr(rest, cp + 3) }
    }
  }
  # Svelte templates embed JS inside { } expressions and on:event handlers;
  # those carry // and /* */ code comments that are NOT rendered. Strip them
  # the same way as on the .ts side. The "://" guard avoids eating URLs (e.g.
  # an http:// inside a rendered href), so only true comment slashes go.
  while ((b = index(line, "/*")) > 0) {
    c = index(substr(line, b + 2), "*/")
    if (c == 0) { line = substr(line, 1, b - 1); break }
    line = substr(line, 1, b - 1) substr(line, b + 2 + c + 1)
  }
  lp = index(line, "//")
  if (lp > 0 && !(lp > 1 && substr(line, lp - 1, 1) == ":")) line = substr(line, 1, lp - 1)

  if (visible(line)) report(orig)
  next
}

# ---- .ts / .js: drop comments, allowlist the menus.ts sentinel -------------
{
  orig = $0; line = $0
  if (in_block) { p = index(line, "*/"); if (p == 0) next; line = substr(line, p + 2); in_block = 0 }
  # Collapse any /* ... */ spans that open on this line.
  while ((b = index(line, "/*")) > 0) {
    c = index(substr(line, b + 2), "*/")
    if (c == 0) { line = substr(line, 1, b - 1); in_block = 1; break }
    line = substr(line, 1, b - 1) substr(line, b + 2 + c + 1)
  }
  lp = index(line, "//"); if (lp > 0) line = substr(line, 1, lp - 1)  # // line comment

  if (!visible(line)) next

  # Allowlist: the menus.ts separator sentinel, exact forms only.
  t = line; gsub(/^[ \t]+/, "", t); gsub(/[ \t]+$/, "", t)
  if (t == "\"" EM "\",")                  next   #   "—",  order-table separator
  if (t == "| \"" EM "\"")                 next   #   | "—" SeatEntry type union
  if (t == "if (entry === \"" EM "\") {")  next   #   sentinel comparison

  report(orig)
}

END { exit (found ? 1 : 0) }
'

status=0
while IFS= read -r f; do
  case "$f" in
    *.svelte) ext="svelte" ;;
    *)        ext="code" ;;
  esac
  if ! awk -v EM="$EMDASH" -v EN="$ENDASH" -v ext="$ext" "$SCAN_AWK" "$f"; then
    status=1
  fi
done < <(find "$SRC" -type f \( -name '*.svelte' -o -name '*.ts' -o -name '*.js' \) | sort)

if [ "$status" -ne 0 ]; then
  {
    echo ""
    echo "check-no-emdash: em-dash (—) / en-dash (–) found in user-visible UI copy."
    echo "Replace with ' - ' or rephrase. (Comments and the menus.ts separator sentinel are exempt.)"
  } >&2
fi

exit "$status"
