#!/usr/bin/env bash
#
# Does the commit stamp shown in the interface actually follow HEAD?
#
# The interface links its own build to a commit on GitHub. That link is worth nothing
# unless it is *right*, and the way it goes wrong is not a crash but silence: cargo
# caches build script output, so `build.rs` can capture a hash once and then keep
# reporting it for every later commit. The page would go on naming a commit from weeks
# ago with complete confidence.
#
# The unit tests in `ui/dialogs.rs` cover how a stamp is rendered. They cannot cover
# this, because this is a question about cargo's behaviour across two builds. So:
#
#   1. Build, and check the stamp matches HEAD.
#   2. Move HEAD.
#   3. Build again *into the same target directory*, and check the stamp moved with it.
#
# Step 3 is the whole point. A fresh target directory would pass trivially.
#
# Run it:  tools/commit-stamp-check.sh
#
# It works in a throwaway clone under a temp directory, so it never touches the
# repository you are sitting in and the commit it makes in step 2 is discarded with it.

set -euo pipefail

repo="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"

# The clone goes beside the repository rather than under /tmp, which on this kind of
# machine is a tmpfs too small to hold a Rust build.
work="$(mktemp -d "${TMPDIR:-$(dirname "$repo")}/slice-stamp-probe.XXXXXX")"
trap 'rm -rf "$work"' EXIT

echo "Cloning into $work"
git clone --quiet --no-hardlinks "$repo" "$work/slice-web"
cd "$work/slice-web"

# `cargo check` rather than `cargo build`: build scripts run either way, and this is a
# question about build scripts. Checking skips code generation, which is most of the time
# and nearly all of the disk -- on the order of a minute and a few hundred megabytes
# instead of several minutes and a couple of gigabytes.
#
# The target directory belongs to the probe alone. Sharing the repository's was tried and
# is *wrong*: cargo reused the already-compiled crate wholesale, no build script ran at
# all, and the probe read the repository's stamp while believing it had read the clone's.
# A check that passes without running the thing it checks is worse than no check.
export CARGO_PROFILE_DEV_DEBUG=0

# Read what build.rs emitted, rather than what git says -- the point is to check the
# build script, so asking it is the only answer that counts.
stamp() {
  cargo check --quiet -p slice-web
  local output
  output="$(ls -t target/debug/build/slice-web-*/output 2>/dev/null | head -1)"
  [ -n "$output" ] || fail "no build script output at all -- nothing was checked"
  # Guard against the vacuous pass described above: the stamp must have been produced by
  # a build script that was looking at *this* clone.
  grep -q "rerun-if-changed=$PWD/.git/HEAD" "$output" \
    || fail "the build script output in $output was not produced by this clone"
  sed -n 's/^cargo:rustc-env=SLICE_COMMIT=//p' "$output" | tail -1
}

fail() { echo "FAIL: $*" >&2; exit 1; }

before_head="$(git rev-parse HEAD)"
before="$(stamp)"
echo "HEAD  $before_head"
echo "stamp $before"
[ "$before" = "$before_head" ] || fail "a clean checkout stamped '$before', expected '$before_head'"

git -c user.email=probe@example.invalid -c user.name=probe \
    commit --quiet --allow-empty -m "probe: move HEAD"
after_head="$(git rev-parse HEAD)"
after="$(stamp)"
echo "HEAD  $after_head"
echo "stamp $after"

[ "$after" != "$before" ] || fail "the stamp did not move with HEAD -- cargo served a cached build script, which is exactly the failure this checks for"
[ "$after" = "$after_head" ] || fail "after moving HEAD the stamp was '$after', expected '$after_head'"

# And the other half of the contract: a modified tree must be marked, so the interface
# knows not to link it to a commit that does not describe the running code.
echo "// probe" >> crates/slice-web/src/lib.rs
dirty="$(stamp)"
echo "stamp $dirty (with an edited working tree)"
[ "$dirty" = "${after_head}+" ] || fail "a dirty tree stamped '$dirty', expected '${after_head}+'"

echo
echo "PASS: the stamp tracks HEAD across builds and marks a modified tree"
