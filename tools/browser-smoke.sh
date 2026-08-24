#!/usr/bin/env bash
# Does the application actually start in a browser and read a font?
#
# The engine is covered by `cargo test`, which runs natively. What that cannot reach is
# the part that only exists in a browser: whether the WebAssembly module instantiates,
# whether Leptos mounts, and whether a font read through the browser's file APIs reaches
# the editors. This checks exactly that, and nothing more.
#
#   tools/browser-smoke.sh                # build, then check
#   tools/browser-smoke.sh --no-build     # check whatever is already in dist/
#
# It loads the page with `?sample`, which opens the bundled Recursive test font on start,
# then dumps the rendered DOM and looks for what should be there. Exits non-zero on the
# first thing that is missing, naming it.
#
# Requires chromium (or chrome) and python3 on PATH.

set -euo pipefail
cd "$(dirname "$0")/.."

PORT=${PORT:-8931}
BUILD=1
[[ "${1:-}" == "--no-build" ]] && BUILD=0

BROWSER=""
for candidate in chromium chromium-browser google-chrome chrome; do
  if command -v "$candidate" >/dev/null 2>&1; then
    BROWSER=$candidate
    break
  fi
done
if [[ -z "$BROWSER" ]]; then
  echo "no chromium/chrome on PATH; skipping the browser smoke test" >&2
  exit 0
fi

if [[ $BUILD == 1 ]]; then
  ./build.sh >/dev/null
fi

if [[ ! -f dist/pkg/slice_web_bg.wasm ]]; then
  echo "dist/ has not been built; run ./build.sh" >&2
  exit 1
fi

workdir=$(mktemp -d)
cleanup() {
  [[ -n "${server_pid:-}" ]] && kill "$server_pid" 2>/dev/null || true
  rm -rf "$workdir"
}
trap cleanup EXIT

python3 -m http.server --directory dist "$PORT" >"$workdir/server.log" 2>&1 &
server_pid=$!

# Wait for the server rather than sleeping a fixed amount.
for _ in $(seq 1 50); do
  if curl -fsS -o /dev/null "http://127.0.0.1:$PORT/index.html" 2>/dev/null; then
    break
  fi
  sleep 0.1
done

"$BROWSER" --headless --disable-gpu --no-sandbox \
  --virtual-time-budget=15000 \
  --dump-dom "http://127.0.0.1:$PORT/index.html?sample" \
  >"$workdir/dom.html" 2>"$workdir/browser.log"

fail() {
  echo "FAIL: $1" >&2
  echo >&2
  echo "browser log:" >&2
  tail -20 "$workdir/browser.log" >&2
  exit 1
}

check() {
  local description=$1 needle=$2
  if grep -qF -- "$needle" "$workdir/dom.html"; then
    echo "  ok   $description"
  else
    fail "$description (looked for: $needle)"
  fi
}

echo "checking the rendered page"

# The engine started and Leptos mounted.
check "the application mounted"            '<div class="app">'

# The loading message removes itself once the module has instantiated, so its absence
# is the signal that the engine really started rather than the page merely rendering.
if grep -qF 'id="boot"' "$workdir/dom.html"; then
  fail "the loading message is still on the page, so the engine did not start"
fi
echo "  ok   the engine started"

absent() {
  local description=$1 needle=$2
  if grep -qF -- "$needle" "$workdir/dom.html"; then
    fail "$description"
  fi
  echo "  ok   $description"
}

# The sample the application actually ships, read out of the source rather than repeated
# here: this file said `Recursive-VF.subset.ttf` for a while after the sample changed, and
# a test that hardcodes what it is checking will do that again.
sample=$(sed -n 's/^pub const SAMPLE_NAME: &str = "\(.*\)";$/\1/p' \
  crates/slice-web/src/files.rs)
if [[ -z "$sample" ]]; then
  echo "could not read SAMPLE_NAME from crates/slice-web/src/files.rs" >&2
  exit 1
fi

# The sample font was read, and every editor was filled from it.
check "the font was opened"                "$sample"
check "the status bar reports the font"    'loaded (5 axes)'
check "glyph count is shown"               '1304 glyphs'

# The Axis Editor read fvar, in order, with the right extents.
for axis in MONO CASL wght slnt CRSV; do
  check "axis $axis is listed"             ">$axis<"
done
check "wght extent is right"               '300.0 : 1000.0 [300.0]'
check "CRSV default is right"              '0.0 : 1.0 [0.5]'

# The Name Editor was filled from the name table, not left blank. This is the check
# that caught the rows being keyed such that they never re-rendered.
check "name records reached the fields"    'value="Recursive Sans Linear Light"'
check "the postscript name is filled in"   'value="Recursive-SansLinearLight"'

# The Bit Flag Editor read the real OS/2 value rather than starting at zero.
check "fsSelection was read from the font" '0000000011000000'

# The controls that only this version has.
check "overlap removal is offered"         'Remove overlapping contours'
check "the output name is suggested"       "${sample%.*}"

echo
echo "browser smoke test passed"
