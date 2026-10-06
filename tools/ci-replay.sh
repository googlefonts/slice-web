#!/usr/bin/env bash
# Does the Conformance corpus job pass when it is run the way CI runs it?
#
#   tools/ci-replay.sh [COMMIT] [LIMIT_SECONDS]     # defaults: HEAD, 600
#
# CI starts from nothing: a fresh checkout, a .suite-venv that the fixture step creates
# with fontTools alone, a Python with no PyQt5, and the original Slice checked out at the
# commit ci.yml pins. A workstation has none of that -- its .suite-venv has had PyQt5
# since the first run -- which is how the job hung on every CI run from 7948b04 to
# 8d1b9c7 while passing here. This rebuilds CI's starting point in a throwaway worktree
# under $TMPDIR, runs the job's last steps, and removes the worktree afterwards.
#
# It counts how often run.py starts, through a shim put in place of the venv's python
# that logs each start before handing over. One start inside the venv is the bootstrap
# working; more is the loop. run.py is cut off after LIMIT_SECONDS.
#
# Prints: the commit, how many times run.py started inside the venv, its exit status and
# wall time, both runners' summary lines, and gen-docs.py --check's verdict. Needs
# network access (pip, and a clone of source-foundry/Slice).
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
commit=$(git -C "$repo" rev-parse --short "${1:-HEAD}")
limit=${2:-600}
original_ref=$(sed -n 's/^ *ref: *\([0-9a-f]\{40\}\) *$/\1/p' "$repo/.github/workflows/ci.yml" | head -1)
[ -n "$original_ref" ] || { echo "no pinned original Slice ref in ci.yml" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-/tmp}/ci-replay-$commit-XXXXXX")
cleanup() {
  git -C "$repo" worktree remove --force "${work:?}/tree" 2>/dev/null || true
  rm -rf -- "${work:?}"
}
trap cleanup EXIT

git -C "$repo" worktree add --detach "$work/tree" "$commit" >/dev/null 2>&1
cd "$work/tree"

# The fixture step, as ci.yml writes it: fontTools and brotli, nothing else.
python3 -m venv .suite-venv
.suite-venv/bin/pip install -q "fonttools[woff]==4.62.1" brotli

# The shim. A venv recognises itself from pyvenv.cfg one level above the executable,
# so the real interpreter has to be started from inside bin/ for the venv to hold.
mv .suite-venv/bin/python .suite-venv/bin/python-real
starts="$work/starts.log"
cat > .suite-venv/bin/python <<EOF
#!/bin/sh
echo "\$*" >> "$starts"
exec "\$(dirname "\$0")/python-real" "\$@"
EOF
chmod +x .suite-venv/bin/python

git clone -q https://github.com/source-foundry/Slice "$work/original-slice"
git -C "$work/original-slice" checkout -q "$original_ref"

cargo build -q -p slice-cli

if python3 -c "import PyQt5" 2>/dev/null; then
  echo "warning: this python3 can import PyQt5, so it cannot reproduce CI's start" >&2
fi

started=$(date +%s)
set +e
SLICE_ORIGINAL_SRC="$work/original-slice/src" timeout "$limit" \
  python3 tests/suite/run.py --verbose > "$work/run.out" 2> "$work/run.err"
status=$?
set -e
elapsed=$(( $(date +%s) - started ))

runs=$(grep -c "tests/suite/run.py" "$starts" 2>/dev/null || true)
echo "commit:            $commit"
echo "run.py starts:     ${runs:-0} inside the venv"
if [ "$status" -eq 124 ]; then
  echo "run.py:            cut off after ${limit}s -- it never finished"
else
  echo "run.py:            exit $status after ${elapsed}s"
fi
grep -E "^=== |passed" "$work/run.out" | sed 's/^/    /' || true
if [ "$status" -ne 124 ]; then
  if python3 tests/suite/gen-docs.py --check > /dev/null 2>&1; then
    echo "gen-docs --check:  up to date"
  else
    echo "gen-docs --check:  FAILED"
  fi
fi
[ "$status" -eq 0 ]
