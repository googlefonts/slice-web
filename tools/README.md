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

Seven cases, from pinning everything to keeping two axes with one restricted. All seven
match.

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
