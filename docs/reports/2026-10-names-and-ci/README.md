# Two more fixes, and a move — October 2026

[`one-pager.pdf`](one-pager.pdf) is the follow-up to
[the first one-pager](../2026-10-google-sans-flex/), on one A4 page:

4. **Names nobody edited were saved empty** (7fe0c72, with d48bba1). From a9dceae
   (2026-08-24) the web app gave the slice job only the Name Editor rows and bit fields
   that differed from the font, so every untouched name came out as an empty string, the
   typographic names were deleted, and fsSelection and macStyle were zeroed. A user's
   condensed Regular failed Font Book because of it. Reopening the app's own link was a
   second path to the same blank subfamily.
5. **CI never finished** (4e2c99c, 4445257). Every CI run from 2026-08-24 to 2026-10-06 —
   eight of them — was killed at GitHub's six-hour limit in the conformance job, where
   `run.py` re-executed itself forever into a venv without PyQt5; CI also never had the
   original Slice to compare against.
6. **Moved to googlefonts** (a0f1b31).

**Question `build.py` answers:** what does each look like, before and after? Section 4 is
measured when the page is built, by driving two builds of the app in headless Chromium:
the user's Regular (Google Sans Flex, four names typed, subfamily left as "Regular",
sliced once) and the bundled Recursive sliced with nothing edited, each saved font read
back byte by byte. Section 5's bars come from `ci-runs.json`, a snapshot of the GitHub
API taken 2026-10-08 (`--refresh-ci` takes a new one). The 141-restarts figure is
`tools/ci-replay.sh`'s; see `tools/README.md`.

Measured on 2026-10-08:

| | e7d44f7 | 7fe0c72 |
|---|---|---|
| Regular, typed once: saved name ID 2 | `""` | `"Regular"` |
| Regular, typed once: saved fsSelection | `0000000000000000` (font `0000000011000000`) | `0000000011000000` |
| Regular's link reopened: Subfamily cell | `""` | `"Regular"` |
| Recursive, nothing edited: names 1, 2, 3, 4, 6 | all `""` | as the font |
| Recursive, nothing edited: names 16, 17 | deleted | as the font |
| Recursive, nothing edited: fsSelection | `0000000000000000` (font `0000000011000000`) | as the font |

## Rebuilding it

```sh
git worktree add --detach ../slice-before e7d44f7    # the last build before the name fixes
(cd ../slice-before && ./build.sh)
./build.sh && cargo build --release -p slice-cli     # this checkout's app and CLI
docs/reports/2026-10-names-and-ci/build.py \
    --gsf 'GoogleSansFlex[GRAD,ROND,opsz,slnt,wdth,wght].ttf' \
    --before-dist ../slice-before/dist
```

The font is Google Sans Flex 4.005 from google/fonts (sha256 under
`tools/README.md`, `overlap-check.py`). The script needs chromium and `pdfinfo`, uses
only the standard library, prints every measurement it puts on the page, and refuses
to write a second page.
