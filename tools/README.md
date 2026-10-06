# tools

Scripts that produce committed artefacts. Each one says which question it answers and how
to run it; none of them are needed to build or use Slice, only to regenerate or re-check
something that is already in the tree.

| script | question it answers |
|---|---|
| `gen-solver-vectors.py` | Does our sub-space solver agree with fontTools' on every case fontTools tests? |
| `browser-smoke.sh` | Does the application start in a real browser and read a font through it? |
| `browser-slice-test.py` | If someone fills in the editors and presses Slice, do they get the font they asked for? |
| `compare-with-fonttools.py` | Does slicing a font here give the same font the original Slice would have given? |
| `fontspector-compare.py` | Does slicing introduce problems fontspector can see? Compares before and against, and discounts anything fontTools' own instance also produces. |
| `diffenator3-compare.py` | Does a font sliced here *render* the same as one sliced by fontTools — glyphs and shaped words, pixel by pixel? |
| `corpus-sweep.py` | Pointed at hundreds of real variable fonts nobody designed a test around, does it crash, does it produce a readable font, and does that font agree with fontTools? |
| `compare-cff2-with-fonttools.py` | Does instancing a CFF2 font resolve the same blends into the same charstrings fontTools writes? |
| `kerning-compare.py` | Does a sliced font position glyphs — kerning, mark attachment — the way the variable font does at that location, and the way fontTools' instance does? Shapes every pair of characters with HarfBuzz. |
| `gdef-store-survey.py` | How many fonts in Google Fonts keep kerning or anchors in a `GDEF` variation store, and how many address it from inside an extension lookup? |
| `kerning-sample.sh` | Across real fonts, not just the two the bug was found on, do static instances kern the way fontTools' do? Ten fonts fixed by the survey, before and after in one run. |
| `overlap-check.py` | After overlap removal, does any glyph still overlap — checked with skia-pathops, not our engine — and did every glyph keep its shape? |
| `overlap-engine-eval/` | Would `linesweeper` remove overlaps correctly on the shapes `flo_curves` gets wrong, and can it be used from WebAssembly? (a cargo crate; see its own README) |
| `woff2-decoder-eval/` | Which pure-Rust WOFF2 decoder reconstructs an sfnt most faithfully, and which ones still build? (a cargo crate, not a script; see its own README) |

Two more live as cargo examples next to the code they debug, rather than here:

| example | question it answers |
|---|---|
| `crates/slice-core/examples/probe_glyph.rs` | Why did this glyph's points not land where a renderer says they should? |
| `crates/slice-core/examples/probe_partial.rs` | What happened to this font's variation data when it was partially instanced? |

Both print their reasoning rather than a verdict, and both earned their place by finding
a real bug: `probe_glyph` found IUP interpolating against already-moved coordinates and
normalized coordinates not being quantized to F2Dot14; `probe_partial` found gvar's entry
array being positional, so a glyph with no variations shifted every later glyph's deltas
onto the wrong glyph.

## `gen-solver-vectors.py`

`crates/slice-core/src/solver.rs` is a hand port of
`fontTools.varLib.instancer.solver`. The evidence that the port is faithful is
`crates/slice-core/src/solver_vectors.rs`: fontTools' own parametrised test table,
lifted verbatim and compiled into a Rust `const` that the solver's
`matches_fonttools_solver_test_vectors` test walks.

```sh
tools/gen-solver-vectors.py            # regenerate from the pinned fontTools tag
tools/gen-solver-vectors.py --check    # fail if the committed file is out of date
tools/gen-solver-vectors.py --from /path/to/solver_test.py   # use a local copy
```

The fontTools release is pinned in `FONTTOOLS_TAG` at the top of the script. To move to a
newer fontTools: bump the tag, re-run without `--check`, and commit the regenerated
vectors together with whatever solver change they force. The generated file records the
tag it came from in its header, so a checkout always says which upstream release its
vectors describe.

As of fontTools 4.62.1 the table holds 32 cases, and all 32 pass.

## `browser-smoke.sh`

`cargo test` runs the engine natively, which is where nearly all the logic lives and
where it should be tested. What that cannot reach is the part that only exists in a
browser: whether the WebAssembly module instantiates, whether Leptos mounts, and whether
a font read through the browser's file APIs actually reaches the three editors.

```sh
tools/browser-smoke.sh              # build, then check
tools/browser-smoke.sh --no-build   # check whatever is already in dist/
PORT=9000 tools/browser-smoke.sh    # if 8931 is taken
```

It serves `dist/`, opens it with `?sample` (which loads the bundled Recursive test font
on start), dumps the rendered DOM and asserts what should be in it. It needs `chromium`
or `chrome` on PATH and skips itself, with exit status 0, when neither is present.

The signals it checks are chosen to fail loudly rather than subtly:

- the loading message has removed itself, which only happens after the module
  instantiates;
- the status bar reports `loaded (5 axes)`, so `fvar` was read;
- `wght` reads `300.0 : 1000.0 [300.0]`, so the extents are right and in the right order;
- the Name Editor fields carry the font's actual names. This one earns its place: the
  rows are keyed by nameID and are never rebuilt, so an early version left every field
  blank on screen while the model behind it was correct. A screenshot showed it; the
  DOM check now catches it.
- `OS/2.fsSelection` reads `0000000011000000`, so the bits came from the font rather
  than starting at zero — including bit 7, which the editor does not expose and must
  preserve.

Note that input values are asserted through the `value` attribute. Leptos drives inputs
through the DOM *property*, which a serialised DOM does not show, so the components set
both; the attribute exists to make the state inspectable from outside.

## `browser-slice-test.py`

`browser-smoke.sh` proves the application starts and reads a font. `cargo test` proves
the engine is right. Neither covers the path between them: the click handler, the job
the interface builds out of the three editors, and the Blob handed back as a download.

```sh
tools/browser-slice-test.py              # build, then run
tools/browser-slice-test.py --no-build   # use whatever is in dist/
tools/browser-slice-test.py --keep       # leave the produced fonts in the repo root
```

It drives Chromium over the DevTools protocol: opens the page with the sample font,
types into the Axis Editor and the Name Editor through the native value setter (so
Leptos sees the `input` events), ticks overlap removal, and presses Slice. Rather than
intercept a real download it wraps `URL.createObjectURL`, so the exact bytes the page
produced come back to the script.

Those bytes are then read by `slice-cli`, which is what makes this worth having: the
browser made the font and the *native* engine has to agree it is one, with the family
name the interface set, no `fvar` left, and the glyph count intact. It then slices a
second time with `wght` restricted to `300:700` instead, checking the partial path leaves
a variable font with only that axis, at its new extent.

The DevTools client is about a hundred lines of socket code rather than a dependency.
That is a deliberate trade: this repository needs nothing but a Rust toolchain and a
browser, and keeping it that way is worth more than the lines saved. Like the smoke test,
it exits 0 with a message when no browser is on PATH.

## `compare-with-fonttools.py`

The original Slice is a thin interface over `fontTools.varLib.instancer`. So the sharpest
parity test available is not to describe the original's behaviour and check against the
description — it is to run the actual library, on the same input, with the same request,
and diff the results.

```sh
tools/compare-with-fonttools.py            # build, set up the venv, compare
tools/compare-with-fonttools.py --verbose  # print every field, not just differences
```

It installs the exact fontTools release the sub-space solver was ported from into
`.fonttools-venv/`, calls `instantiateVariableFont` the way `InstanceWorker` does
(`inplace=True, optimize=True`, default overlap mode), runs `slice cut` with the same
settings, and compares both fonts field by field: every glyph's outline as recorded pen
output, every advance, `maxp`, the `head` bounding box and flags, `hhea`'s extremes,
`OS/2`'s weight class, width class and average width, `post.italicAngle`, the set of
name IDs, the `fvar` axes and instance count, `STAT`'s axes and values, and which lookups
each `GSUB`/`GPOS` feature runs.

Eight cases, from pinning everything to keeping two axes with one restricted. All eight
match.

The eighth is the `avar` 2 case, and it runs on a different fixture: Roboto Delta, whose
39 axes move each other through an item variation store rather than through independent
segment maps. It has to pin **every** axis, because fontTools refuses to partially
instance an avar 2 font and so do we — leaving one out turns the request into a partial.
It is worth its length: reading only the version 1 segment maps and ignoring the store put
the outlines **173 font units** from fontTools at `opsz=70 wght=600 wdth=120`, a visibly
different letter, produced with no error and no warning. This case is what measures that,
and it now reports no difference at all.

`--verbose` prints each field's value rather than only the disagreements, which is how to
read the numbers back out:

```
  ok   maxPoints                102
  ok   headBBox                 [18, -10, 598, 562]
  ok   usWeightClass            1000
  ok   nameIDs                  [0, 1, 2, 3, 4, 5, 6, 269, 270, 271, 272, 273, 402, 412, 413]
```

Those are the same fields the parity review quoted — before the fixes they read 392,
`[-275, -330, 2380, 1125]`, 300, and 153 records. Running this against an older commit
reproduces the failures rather than leaving the reader to take the numbers on trust.

This is what found the parity defects the review turned up, and it is worth keeping
pointed at the code: it is the only check here that can see a whole class of mistake —
"we never did that step at all" — which no amount of internal consistency testing
reaches. Two differences are allowlisted in `ACCEPTED` and explained in the script's
docstring; both are size rather than behaviour.

It needs network access the first time. Like the other browser and environment scripts,
it exits 0 with a message when it cannot prepare its environment.

## `compare-cff2-with-fonttools.py`

`compare-with-fonttools.py` compares *drawn outlines*, which is the right check for
`glyf` and not enough for CFF2. A CFF2 instance can draw correctly today and still be
wrong: a `blend` left behind in a font whose variation store has been deleted, a
`vsindex` pointing at a subtable that is no longer there, a Private DICT that lost its
alignment zones, a region list that no longer matches the deltas in the charstrings. None
of those move a point until something else reads the font.

So this one reaches inside the table. It disassembles every charstring on both sides and
prints them next to each other, along with the variation store's regions, each
subtable's region indices, the Private DICTs, `hmtx` and `fvar`.

```sh
tools/compare-cff2-with-fonttools.py             # build, compare, print differences
tools/compare-cff2-with-fonttools.py --verbose   # print the programs even when equal
```

Six cases on `tests/suite/fixtures/out/cff2-vf.otf`: pinned at 400, 500, 700 and 900, and
restricted to 400:700 and 400:900. **Every charstring program is byte-identical to
fontTools 4.62.1's**, in all six. That is a stronger statement than "the outlines agree",
and it holds because both sides round the same way — fontTools adds `round(defaultDelta)`
to the base value in `instantiateCFF2`, and `instancer/cff2/charstring.rs` copies that
down to Python's ties-to-even.

Three differences are allowlisted and explained in the script's docstring: table size
(fontTools re-specializes the operators, this does not), `STAT` and name records (`slice
cut` runs the whole pipeline and `instantiateVariableFont` does not), and an `HVAR` whose
store has no regions left, which fontTools keeps as a shell and this drops.

## `commit-stamp-check.sh`

**Does the commit hash shown in the interface actually follow HEAD?**

The status bar links the running build to a commit on GitHub. That link is worth nothing
unless it is right, and the way it goes wrong is not a crash: cargo caches build script
output, so `crates/slice-web/build.rs` can capture a hash once and keep reporting it
through every later commit, naming a stale commit with complete confidence.

The unit tests in `crates/slice-web/src/ui/dialogs.rs` cover how a stamp is *rendered* —
short hash, `(modified)`, `unknown build`, and when a link is withheld. They cannot cover
this, because this is a question about cargo's behaviour across two builds rather than
about any function. So the script clones the repository, stamps it, moves HEAD, stamps
again in the same target directory, and requires the second stamp to have moved with it;
then it edits a tracked file and requires the `+` that marks a modified tree.

```sh
tools/commit-stamp-check.sh
```

Takes a few minutes and needs a few hundred megabytes: it builds the dependency tree once
in its own target directory. Sharing the repository's was tried and produced a **vacuous
pass** — cargo reused the compiled crate wholesale, no build script ran at all, and the
probe read the repository's own stamp believing it had read the clone's. The script now
asserts that the build script output it reads names the clone's `.git`, so that failure
reports itself rather than reporting success.

## `overlap-check.py`

**After "Remove overlapping contours", does any glyph still overlap, and did the merge
keep every glyph's shape?**

The engine's own tests ask only the second half: whether merging changed which points are
inside a glyph. A merge that does nothing at all passes that perfectly, and that is how
this got past them. A user outlined Google Sans Flex Condensed Black in Illustrator and
found `e`, `n`, `m`, `r`, `u` still overlapping, with the box ticked.

So this asks the first half too, with skia-pathops: the Skia path ops fontTools'
`removeOverlaps` is built on, and nothing `linesweeper` shares code with. Per glyph,
composites resolved, it reports contours that cross each other (`cross`), contours that
cross themselves (`self`), area filled more than once (`double`: the non-zero and
even-odd fills differ), and contours enclosing under one square unit (`empty`: stray
paths). With `--against` it also measures the mean distance each glyph's filled edge
moved.

```sh
tools/overlap-check.py FONT [FONT ...]          # any overlaps left?
tools/overlap-check.py --against PLAIN MERGED   # ...and did the shapes survive?
```

The case it was written for, Google Sans Flex 4.005 from google/fonts
(`ofl/googlesansflex/GoogleSansFlex[GRAD,ROND,opsz,slnt,wdth,wght].ttf`, sha256
`c31a482fbecbf2e07e6890134d20078723aadf732c9b9c6c9a44f86f8265b6fe`), at the reporter's
location:

```sh
GSF='GoogleSansFlex[GRAD,ROND,opsz,slnt,wdth,wght].ttf'
AXES='--axis opsz=18 --axis wdth=80 --axis wght=900 --axis GRAD=0 --axis ROND=0 --axis slnt=0'
target/release/slice cut "$GSF" plain.ttf  $AXES
target/release/slice cut "$GSF" merged.ttf $AXES --remove-overlaps
tools/overlap-check.py plain.ttf merged.ttf
tools/overlap-check.py --against plain.ttf merged.ttf
```

| | glyphs with problems, of 682 |
|---|---|
| `plain.ttf`, no overlap removal | 305 (189 cross, 146 self, 305 double) |
| `merged.ttf` at 73f9f96 | **45**: 37 `self` — `e n m r u P ə Ə` and their accented forms — and 8 `empty` |
| `merged.ttf` with the fix | **0** |

The 37 were glyphs the engine never tried to merge. A bounding-box screen ran first and
compared segments' boxes by the area they shared, which for a horizontal or vertical line
is always zero, so a stem edge crossing a horizontal one was invisible to it. The 8 were
made by the merge: a hairline spike in the source (a curve running half a unit out and
straight back) came out of the sweep as a contour of its own with no area. Both are fixed
in `crates/slice-core/src/overlaps.rs`; the regression tests use the real `n` and
`acutecomb.viet` outlines from this instance.

With the fix, `slice cut` reports `305 glyphs simplified, 377 left as they were` — the
same 305, by name — and the worst mean edge movement against `plain.ttf` is **0.113
units** (`eth`): the integer rounding and quadratic refit, and nothing more. 279 of the
305 change shape at all; the other 26 are straight-sided and come back exact.

The threshold that decides "the merge changed nothing" sits in a wide gap, measured by
the ignored test `probe_area_change` in `overlaps.rs` on `plain.ttf`: the 364 glyphs
without overlaps change area by at most **6.3e-8** square units when merged, and the 305
with them by at least **60**:

```sh
SLICE_PROBE_FONT=plain.ttf cargo test --release -p slice-core --lib \
    probe_area_change -- --ignored --nocapture
```

This is one font. It is the first real one overlap removal has been checked on by an
engine other than its own; see `docs/evidence.md`.

## `kerning-compare.py`

**Does a font sliced here position its glyphs the way the variable font does at the same
location, and the way fontTools' instance does?**

No outline check can see positioning. Variable kerning and mark anchors live in a `GDEF`
item variation store that `GPOS` reaches into, and an instance that mishandles it draws
every glyph perfectly and sets every line at the wrong width. So this sets text: it
shapes every ordered pair of characters with HarfBuzz in three fonts — the variable font
at the location (HarfBuzz evaluates the store itself), fontTools' instance from
`instantiateVariableFont`, and ours from `slice cut` — and compares every glyph's advance
and offsets. A pair is as often a base and a combining mark as two letters, so anchors
are covered along with kerning. A request that leaves axes variable is compared at each
surviving axis's minimum, middle and maximum.

```sh
GSF='GoogleSansFlex[GRAD,ROND,opsz,slnt,wdth,wght].ttf'
AXES='--axis opsz=18 --axis wdth=80 --axis GRAD=0 --axis ROND=0 --axis slnt=0'
tools/kerning-compare.py "$GSF" $AXES --axis wght=900        # static
tools/kerning-compare.py "$GSF" $AXES --axis wght=400:900    # partial
```

It passes when ours and fontTools' instance agree on every pair. Agreeing with the variable
font is the stronger statement and holds exactly for static instances; a partial one
re-rounds its re-tented deltas to integers, and both programs land the same 2 or 3 units
from the variable font at interior locations.

Google Sans Flex 4.005 (the file and sha256 under `overlap-check.py`), 332 characters,
110,224 pairs, and the real Recursive from `web/fonts/` (converted from WOFF2), 409
characters, 167,281 pairs — pairs placed differently from fontTools' instance:

| request | at dc17993 | fixed |
|---|---|---|
| GSF static, opsz 18 wdth 80 wght 900 | **20,251**, up to 394 units | 0 |
| GSF partial, wdth 80, wght 400:900, at 400 / 650 / 900 | **14,298** at each | 0 |
| Recursive static, CASL 1 wght 700 slnt −15 CRSV 1 | **1,108**, up to 110 units | 0 |
| Recursive partial, CASL 1, wght 300:700, at 300 / 500 / 700 | 0 | 0 |

Two bugs. A static instance copied `GDEF` and `GPOS` through untouched, so with no `fvar`
left the store could not be evaluated and every value stayed at the default master's —
Recursive's marks sat 110 units off their anchors, and Google Sans Flex's 394. And the
walk that writes a residual back into `GPOS` skipped extension lookups (type 9), believing
the subtable they wrap would be reached on its own; it is held inline, and compilers move
the biggest lookup — usually the kerning — into one. Google Sans Flex's kerning is in an
extension and Recursive's is not, which is why the partial check on Recursive passed all
along and the same job on Google Sans Flex was wrong by a constant 14,298 pairs.

Takes about a minute per location for a font this size.

## `gdef-store-survey.py`

**How many real fonts can a variable-positioning bug reach?**

```sh
.shaping-venv/bin/python tools/gdef-store-survey.py ~/google/fonts survey.tsv
```

On google/fonts at `c36d6f24ed9d8448fd7a4ee14667fddf8dfe701b`: **783** variable fonts,
**758** of them with a `GDEF` item variation store, and **250** that address it from inside
an extension lookup. The first number is the fonts whose static instances could come out
with the default master's kerning and anchors, anywhere off the default location where the
store varies; the second, the fonts whose partial instances could be off by a constant
whenever an axis was pinned away from its default. Google Sans Flex is in both.

## `kerning-sample.sh`

**Across real fonts, not just the two the bug was found on, do static instances kern the
way fontTools' instances do?**

```sh
tools/kerning-sample.sh survey.tsv sample.tsv [OTHER_SLICE_BINARY]
```

The sample is every 25th of the 250 fonts `gdef-store-survey.py` finds addressing their
store from inside an extension lookup, so the survey fixes it rather than anyone choosing
it. Each is instanced statically with every axis at its maximum and compared by
`kerning-compare.py`, once with this checkout's build and once with another binary — here
dc17993's, built in a worktree.

| font | pairs | at dc17993 | fixed |
|---|---|---|---|
| AlanSans[wght] | 99,856 | **62,731** | 0 |
| Bitter[wght] | 177,241 | **58,994** | 0 |
| FinlandicaHeadline[wght] | 177,241 | **76,971** | 0 |
| Literata-Italic[opsz,wght] | 173,889 | **60,992** | 0 |
| NotoSerif-Italic[wdth,wght] | 405,769 | **53,800** | 0 |
| PlaypenSans[wght] | 209,764 | **157,980** | 0 |
| PlaywriteDKLoopet[wght] | 124,609 | 0 | 0 |
| PlaywritePE[wght] | 124,609 | 0 | 0 |
| SchibstedGrotesk[wght] | 127,449 | **60,903** | 0 |
| SUSE[wght] | 103,684 | **35,173** | 0 |

Eight of the ten were wrong by tens of thousands of pairs, and none is now. The two
Playwrite fonts were never wrong because their positioning does not vary: shaped at the
default weight and at the maximum, the variable font adjusts every one of these pairs
identically, so there was nothing for the old build to miss.

