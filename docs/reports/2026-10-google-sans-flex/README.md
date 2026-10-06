# One report, three fixes — October 2026

[`one-pager.pdf`](one-pager.pdf) summarises, on one A4 page, what followed from a user's
report that Google Sans Flex Condensed Black still had overlaps after "Remove overlapping
contours":

1. Overlap removal skipped glyphs whose crossings run along straight edges, and left
   stray zero-area paths behind (fixed in dc17993).
2. Static instances kept the default master's kerning and anchors (b230c24).
3. The walk that applies variable kerning skipped extension lookups, in partial instances
   too (b230c24).

**Question `build.py` answers:** what does each bug look like? Every outline and line of
text on the page is drawn from fonts the two builds actually produce; the variable font,
shaped by HarfBuzz at the same location, is the reference. The numbers on the page come
from the tools named in its footer, and their recipes and results are in
[`tools/README.md`](../../../tools/README.md).

## Rebuilding it

```sh
git worktree add --detach ../slice-before 73f9f96          # the build the report was made against
(cd ../slice-before && cargo build --release -p slice-cli)
cargo build --release -p slice-cli                         # the fixed build
docs/reports/2026-10-google-sans-flex/build.py \
    --gsf 'GoogleSansFlex[GRAD,ROND,opsz,slnt,wdth,wght].ttf' \
    --before ../slice-before/target/release/slice
```

The font is Google Sans Flex 4.005 from google/fonts, `ofl/googlesansflex/`, sha256
`c31a482fbecbf2e07e6890134d20078723aadf732c9b9c6c9a44f86f8265b6fe`. The script needs
chromium, sets up `.report-venv/` (fontTools 4.62.1, uharfbuzz 0.56.3, skia-pathops 0.9.2)
on its first run, and fails rather than writing a second page if the layout overflows.
The page is set in Google Sans Flex too, in instances sliced by the fixed build.
