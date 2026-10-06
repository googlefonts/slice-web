#!/usr/bin/env bash
# Across real fonts, not just the two it was found on, do static instances kern the way
# fontTools' instances do?
#
#   tools/kerning-sample.sh SURVEY.tsv OUT.tsv [OTHER_SLICE_BINARY]
#
# SURVEY.tsv is what tools/gdef-store-survey.py writes. The sample is every 25th of the
# fonts that address their GDEF store from inside an extension lookup -- ten of Google
# Fonts' 250 -- so it is fixed by the survey rather than chosen. Each is instanced
# statically with every axis at its maximum and compared by tools/kerning-compare.py,
# using this checkout's target/release/slice (build it first) and, if given, another
# build's binary, so a before and an after can be read off the same run.
#
# OUT.tsv gets one row per font and build: label, font, pairs shaped, pairs where that
# build and fontTools' instance disagree. A minute or so per font and build.
set -euo pipefail

survey=${1:?usage: tools/kerning-sample.sh SURVEY.tsv OUT.tsv [OTHER_SLICE_BINARY]}
out=${2:?usage: tools/kerning-sample.sh SURVEY.tsv OUT.tsv [OTHER_SLICE_BINARY]}
other=${3:-}

repo=$(cd "$(dirname "$0")/.." && pwd)
compare="$repo/tools/kerning-compare.py"
python="$repo/.shaping-venv/bin/python"
[ -x "$python" ] || { echo "run tools/kerning-compare.py once to set up .shaping-venv" >&2; exit 1; }

builds=("this=$repo/target/release/slice")
[ -n "$other" ] && builds+=("other=$other")

: > "$out"
awk -F'\t' '$5 > 0 { print $1 }' "$survey" | awk 'NR % 25 == 1' |
while read -r font; do
  axes=$("$python" -c '
import sys
from fontTools.ttLib import TTFont
print(" ".join(f"--axis={a.axisTag}={a.maxValue:g}" for a in TTFont(sys.argv[1])["fvar"].axes))
' "$font")
  for build in "${builds[@]}"; do
    label=${build%%=*}
    binary=${build#*=}
    # shellcheck disable=SC2086 # $axes is a list of separate arguments
    report=$("$compare" "$font" --no-build --slice "$binary" $axes 2>/dev/null || true)
    pairs=$(sed -n 's/.* characters, \([0-9]*\) pairs.*/\1/p' <<<"$report")
    disagree=$(sed -n 's/.*ours and fontTools disagree: \([0-9]*\).*/\1/p' <<<"$report" | paste -sd+ | bc)
    printf '%s\t%s\t%s\t%s\n' "$label" "$(basename "$font")" "$pairs" "$disagree" | tee -a "$out"
  done
done
