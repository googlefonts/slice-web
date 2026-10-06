# Slice

**Build custom design sub-spaces from variable fonts, in the browser.**

### → [Try it](https://felipesanches.github.io/slice-web/app/)

The current build of `main`, deployed automatically on every push.

A reimplementation of [Slice](https://github.com/source-foundry/Slice) by Source Foundry:
same interface, Rust engine, runs as a web page, and it removes overlapping contours —
the thing the original never did, and the reason this exists.

Fonts never leave your machine. The file is read by the browser, sliced by WebAssembly,
and handed straight back as a download. There is no server, and nothing to upload.

```
                Axis Editor          Name Editor        Bit Flag Editor
    variable  ┌─────────────┐      ┌────────────┐      ┌──────────────┐
      font ──▶│ wght 200:700│─────▶│ 01 Family… │─────▶│ ☑ bit 6 REG  │──▶ sliced font
              │ wdth  100   │      │ 02 Sub…    │      │ ☐ bit 5 BOLD │
              └─────────────┘      └────────────┘      └──────────────┘
                                                        + remove overlaps
```

## Documentation

Everything is at **[felipesanches.github.io/slice-web](https://felipesanches.github.io/slice-web/)**.

| | |
|---|---|
| **[The application](https://felipesanches.github.io/slice-web/app/)** | Slice itself, running. Built from `main` by CI, not committed |
| **[User's manual](https://felipesanches.github.io/slice-web/manual/)** | The whole tool: the three editors, the axis syntax, overlap removal, the command line, and what every error message means. Also a [PDF](docs/manual/slice-manual.pdf) |
| [Install](https://felipesanches.github.io/slice-web/install/) | Nothing for the browser; one cargo build for the command line |
| [Developer](https://felipesanches.github.io/slice-web/developer/) | Building, testing, the code layout, and how to contribute |
| [Licences](https://felipesanches.github.io/slice-web/license/) | The application, the documentation and the third-party work |
| [Evidence](https://felipesanches.github.io/slice-web/evidence/) | What is tested, how it was measured, and what is still unknown |

The four evidence documents, also readable in the repository:

| | |
|---|---|
| [Test suite in plain English](docs/test-suite.md) | All 301 conformance cases and the reasoning behind each |
| [Adjudication](docs/adjudication.md) | Every case the original Slice fails, and the measurement behind each verdict |
| [Real-world sweep](docs/real-world-sweep.md) | What happens on 775 real variable fonts from Google Fonts |
| [Behaviour map](docs/original-behaviour.md) | The numbered map of the original that the suite is written against |
| [How the corpus works](tests/suite/README.md) | Writing a case, the check kinds, the fixtures |
| [Probes and harnesses](tools/README.md) | The scripts behind every measured number in these documents |

The manual is generated from a single source: edit `docs/manual/manual.md` and run
`docs/manual/build.py`, which writes the LaTeX, the PDF and the website copy together so
they cannot drift apart. `docs/manual/build.py --check` says whether the committed outputs
are current; it is a local step and CI does not run it, so that building the manual never
becomes a reason for the build machine to carry a TeX installation.

## Why

Slice takes a variable font and gives you a smaller one: a single static instance, or a
variable font with fewer axes and narrower ranges. The original is a PyQt5 desktop
application built on fontTools. It works, and this keeps its interface deliberately
close, because the interface is good and people know it.

Two things are different.

**It removes overlaps.** Ten years on, support for overlapping contours in design
applications is still poor. A sliced instance whose stems overlap shows seams when
outlined, misbehaves under boolean operations, and exports badly. fontTools does this
through Skia's path ops, which has no WebAssembly build, so the union here is done with
`linesweeper` — see [Overlap removal](#overlap-removal), which is the most interesting
part of this repository.

**It runs in a browser.** No install, no Python environment, no platform builds to code
sign and notarise.

## Using it

Open the page, drop in a variable font, and fill in the Axis Editor:

| what you want | what you type | example |
|---|---|---|
| keep the whole axis | nothing | |
| pin the axis to one location | a number | `400` |
| restrict the axis to a smaller range | `min:max` | `200:700` |

Pin every axis and you get a static font. Leave any axis with a range and the result is
still variable, with a smaller design space.

A restricted range has to contain the axis's original default value. This is the Level 3
sub-spacing rule: the compiler cannot move a default axis location, so the range has to
keep it. Typing `400:700` on an axis that defaults to 300 is rejected, with the reason.

### Removing overlaps

Tick **Remove overlapping contours**. It needs every axis pinned — merging contours
renumbers a glyph's points, and `gvar` deltas are indexed by point number, so the two
cannot both be true. (For a CFF2 font the reason is the same shape: the deltas live in
the charstrings' `blend` operators, which a redrawn outline no longer has.) It also drops
hinting, which no longer describes the outlines.

### From the command line

```sh
cargo run -p slice-cli -- info Recursive-VF.ttf
cargo run -p slice-cli -- cut Recursive-VF.ttf Bold.ttf \
    --axis wght=800 --axis CASL=1 --remove-overlaps
```

`info` prints exactly what the three editors would show. `cut` takes the same axis
syntax the interface accepts.

## Building it

Needs a Rust toolchain with the `wasm32-unknown-unknown` target, and `wasm-pack`.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

./build.sh                  # release build into dist/
./build.sh --dev            # faster, much larger .wasm, panics keep their messages
python3 -m http.server --directory dist 8080
```

`dist/` is a directory of static files. Copy it anywhere that serves files, as long as
`.wasm` is served as `application/wasm`.

```sh
cargo test --workspace              # the engine
tools/browser-smoke.sh              # that it starts in a real browser and reads a font
tools/browser-slice-test.py         # that pressing Slice produces the right font
tools/compare-with-fonttools.py     # that the result matches what fontTools produces
```

## How it is put together

```
crates/slice-core   the engine: no browser-specific dependencies, so all of it
                    can be tested from a native test binary
crates/slice-cli    a command-line front end, for tests and build scripts
crates/slice-web    the Leptos interface, compiled to WebAssembly
tools/              scripts that produce or re-check committed artefacts
testdata/           fixtures, copied from the original repository
```

The engine is built on [fontations](https://github.com/googlefonts/fontations) —
`read-fonts` for reading, `write-fonts` for writing, and `skrifa` both as a dependency
and, more importantly, as the thing the tests are checked against.

### Verification

There is no Rust equivalent of fontTools' instancer, so the variation maths here is a
port, and a port is worth only as much as the evidence that it matches. Four independent
checks:

**The sub-space solver is checked against fontTools' own test vectors.**
`crates/slice-core/src/solver.rs` is a hand port of
`fontTools.varLib.instancer.solver`. `tools/gen-solver-vectors.py` lifts upstream's
parametrised test table verbatim from a pinned release and compiles it into a Rust
`const`. All 32 cases pass.

**Instancing is checked against skrifa.** skrifa is a separate implementation of the
same specification, and it is what browsers and Android use to render these fonts. So:

> drawing the *variable* font at location L must produce the same outlines and advances
> as drawing our *instance at L* at its own default.

For static instances, over 15 locations and every glyph, the observed deviation is **0
font units** — outlines and advances both, and the test asserts exactly that rather than
a tolerance. For partial instances the same property is checked at locations sampled
across each restricted range; there the largest deviation is **1 font unit**, which is
the integer rounding of `gvar` deltas.

This oracle earned its keep. It found IUP interpolating a tuple's missing deltas against
coordinates that earlier tuples had already moved, and normalized coordinates being
computed in `f64` when a font stores them as F2Dot14 — neither of which a single-axis
test would have caught.

**CFF2 is checked twice over.** Its charstrings are rewritten rather than redrawn — the
`blend` operators are resolved or re-tented in place, so the hints and the subroutine
structure survive — and there are two independent checks that the rewriting is right.
`tools/compare-cff2-with-fonttools.py` disassembles the charstrings on both sides and
compares them program by program: over six requests on the CFF2 fixture, every program is
**identical** to fontTools 4.62.1's. `cff2_instance_matches_skrifa` then draws them, where
the largest deviation is 1 unit and is skrifa's own — it quantizes the location to F2Dot14
and truncates each coordinate, so a value both tools store as 384 draws as 383.998.

**Overlap removal is checked by filled region, not by outline.** The outline is supposed
to change; what must not change is which points are inside the glyph. Both the before and
after are filled with the non-zero winding rule and compared over a dense grid of sample
points.

**And the whole thing is diffed against fontTools.** The original Slice is a thin
interface over `fontTools.varLib.instancer`, so `tools/compare-with-fonttools.py` runs
the real library on the same input with the same request and compares the results field
by field: every glyph outline, every advance, `maxp`, the `head` bounding box and flags,
`hhea`'s extremes, `OS/2`'s weight and width classes and average width,
`post.italicAngle`, the name IDs, `fvar`, `STAT`, and which lookups each `GSUB`/`GPOS`
feature runs. Seven cases, from pinning everything to keeping two axes with one
restricted. All seven match, apart from two documented size differences.

That comparison is the only check here that can see a step being skipped altogether,
which is a category no amount of internal consistency testing reaches. It found six
parity defects that every other test was blind to, including `OS/2.usWeightClass` never
being updated — so an instance pinned at `wght=1000` announced itself to the operating
system as Light — and Recursive's `rvrn` feature never being resolved, so slicing at
`CRSV=1` produced a font that had quietly lost its cursive `a`.

## Overlap removal

The union is done by [`linesweeper`](https://crates.io/crates/linesweeper), a robust
Bentley–Ottmann sweep line over Bézier paths, in pure Rust. fontTools delegates this to
Skia's path ops through `skia-pathops`, which has no WebAssembly build.

The whole operation is one call — the union of the glyph with nothing, under the non-zero
rule, is the glyph normalised — and the result comes back as contours with an explicit
`parent`, so which ones are holes is read off the tree rather than recovered from
geometry.

That is worth saying because the first implementation used `flo_curves` and was three
times the size. `flo_curves` ships `path_remove_interior_points`, which looks like exactly
the right function and is not: `GraphPath::from_path` reverses every anti-clockwise
contour to clockwise before building its graph, so by the time winding is counted nothing
can cancel out, and an `o` comes back with its counter filled in solid. Driving the ray
caster underneath it worked — label each contour, record the reversals, sum the crossings
with them undone — but it also meant reconstructing nesting depth afterwards by
ray-casting a point on each contour's own boundary, since `flo_curves` returns an
*even-odd* set with every contour turning the same way. Switching engines deleted 134
lines and 17 crates.

Depth still has to become a direction, because `glyf` and `CFF` are both filled with the
non-zero rule and disagree about which way an outer contour runs: TrueType clockwise,
PostScript counter-clockwise. Fill is unaffected by a global flip, but a font whose
contours run against its format's convention confuses tools that read direction as
meaning.

Every glyph goes through the sweep. There used to be a cheaper screen in front of it, a
bounding-box test, and it could not see a horizontal edge crossing a vertical one — the
way a heavy `n`'s shoulder tucks into its stem — so those glyphs came out with their
overlaps intact. Contours the merge leaves enclosing nothing, which a hairline spike in
the source turns into, are dropped rather than written back as stray paths.

Tested against a counter, a counter inside a counter (a circled letter — the case that
rules out the tempting shortcut of unioning the outer-wound contours and subtracting the
inner-wound ones), overlapping rings, same-direction nesting, a self-intersecting bow
tie, and the real `n` and spiked acute from Google Sans Flex Condensed Black.
`tools/overlap-engine-eval/` is the harness the engine choice was measured with, and
`tools/overlap-check.py` checks a merged font for anything still overlapping, with
skia-pathops rather than our own engine.

### Variable positioning

A `GDEF` item variation store — variable kerning and anchor positions — is re-tented onto
the narrowed axes like everything else. Re-tenting leaves a residual, the value at the
*new* default location, and that residual is written back into the `GPOS` value records,
anchors and ligature carets that address the store.

The residual is zero whenever every other axis stays at its default, which is why the
first implementation could get away without it. Pinning an axis *away* from its default is
what makes it non-zero: the kerning at `CASL=1` is not the kerning at `CASL=0`, and once
`CASL` is gone the difference has to live in the values themselves. A quarter of the
variable fonts in Google Fonts have both a store and more than one axis, so it is a case
worth getting right rather than refusing.

Checked against fontTools on the real Recursive, `CASL` pinned at 1 with `wght` restricted
to 300:700: **6,439 kerning pairs, identical at wght 300, 500 and 700**. The interior
sample is the one that matters — a store re-tented without its residual is exactly right at
the default and wrong everywhere else.

`GSUB`/`GPOS` *feature variations* — the conditional substitutions `rvrn` uses — are
resolved separately.

`MVAR` is re-tented the same way, so the font-wide metrics — ascender, descender, x-height,
cap height, underline — still move across whatever range is left. This too used to be
"apply at the new default and drop", which left them right at that one location and frozen
elsewhere: narrowed to wght 300:700, a font kept its 300 x-height at 700. Checked against
fontTools on Recursive: x-height 526 / 534 / 538 and underline −205 / −188 / −179 across
the three sampled weights, identical on both sides.

## Where it deliberately differs from the original

Everything the original does, this does, and the results are diffed against fontTools to
prove it. These are the places where it does something *else* on purpose.

**The Bit Flag Editor is prefilled from the font.** The original always starts every
checkbox unchecked, so leaving the editor alone clears whatever bits the input had. Here
the boxes show the font's real values, and an untouched editor is a no-op. Bits the
editor does not expose are preserved either way.

**Name records 16, 17, 21 and 22 are preserved.** The original never reads them into the
editor, and since a blank optional row means "delete this record", it strips all four
from every font it touches. Here they are loaded like the rest, so they survive unless
you clear them.

**An explicit `[default]` in a range is rejected.** The original's regular expression
accepts `200:700[400]`, parses the default out, and then discards it — producing a font
that is not what was asked for. Level 4 sub-spacing is not implemented here either, so
the entry is refused with an explanation instead.

**Contradictory bit combinations are reported.** Setting REGULAR alongside BOLD, or
letting the two BOLD bits disagree, produces a warning under the editor. The original
ships whatever you tick.

**Overlap removal on its own is a valid request.** The original refuses any job that does
not narrow the design space. Removing overlaps changes the font, so it is allowed through.

**There is no save dialog, and no editable path field.** A browser gives neither. The
output name is derived from the input and the axis settings — `Recursive-VF.ttf` at
`wght=800` becomes `Recursive-VF-wght800.ttf` — and the container is chosen from a
dropdown rather than inferred from a filename you type. The original's "the path field
does not match the loaded font" check exists only because that field is editable, so it
has nothing to guard here.

**No "Check for Updates".** Reloading the page is the update.

## What it does not do yet

Stated plainly, because a font tool that is quiet about its gaps is worse than one that
has them.

- **CFF 1.0 outlines.** `glyf` and `CFF2` are both instanced; a plain `CFF ` font is not
  variable in the first place, and writing one is a second outline format for no gain, so
  it is refused rather than mangled. A fully pinned CFF2 font stays CFF2, with its blends
  resolved and its variation store dropped, which is what fontTools does too.
- **The WOFF2 `glyf` transform, when *writing*.** Reading WOFF2 is complete, transformed
  `glyf`/`loca`/`hmtx` included. Writing it uses the null transform (transformVersion 3)
  that the specification provides for exactly this: the output is a conformant WOFF2 that
  every browser and fontTools reads, but on a `glyf`-heavy font it lands about 19% larger
  than what fontTools or `woff2_compress` would emit, because the outlines are
  brotli-compressed as they stand rather than re-encoded into the transform's streams.
  (On a small variable subset, where `gvar` and the layout tables dominate, it is
  actually a shade smaller.) See `crates/slice-core/src/font/woff2.rs` for the numbers.
- **`avar` version 2, for *partial* instancing.** Version 2 lets axes move each other
  through an item variation store, and narrowing an axis would mean rewriting it. Refused,
  as it is by fontTools, which raises `NotImplementedError` for the same job. Pinning every
  axis works and is exact: the store is evaluated at the requested location, which is what
  makes the difference between an instance that is right and one that is 173 units out.
- **Pruning emptied features and unreferenced lookups.** fontTools removes a feature
  whose lookups it has just emptied, and then the lookups nothing references. This keeps
  them: an empty feature runs nothing and an unreachable lookup is never reached, so the
  font behaves identically and is a few hundred bytes larger. Renumbering lookup indices
  across the script list, the feature list and every chained-context rule is easy to get
  wrong, and the size does not justify the risk.
- **Threading.** The engine runs on the main thread, so the page is unresponsive while a
  large font is processed. The progress dialog says so rather than showing a bar that
  pretends to move. A Web Worker is the answer and is not here yet.

## Licences

GPL-3.0-or-later, as the original is. See [LICENSE](LICENSE).

The wordmark uses the Recursive subset the original embedded, and the test fixtures are
Recursive subsets too; both under the SIL Open Font License. See
[thirdparty/](thirdparty/).

The Rust dependencies are Apache-2.0 / MIT.

## Acknowledgements

[Slice](https://github.com/source-foundry/Slice) by Christopher Simpkins and the Source
Foundry authors, whose interface this keeps.
[fontTools](https://github.com/fonttools/fonttools), whose instancer the sub-space solver
is a port of. [fontations](https://github.com/googlefonts/fontations), which does the
reading and writing and is also the yardstick the results are measured against.
[Recursive](https://github.com/arrowtype/recursive) by Stephen Nixon.
