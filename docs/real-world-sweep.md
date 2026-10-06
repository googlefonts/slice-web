---
layout: "default"
title: "What happens on 775 real fonts"
description: "Slicing 775 real variable fonts from Google Fonts"
---

The conformance corpus in `tests/suite/` is 298 cases over 14 fixtures, thirteen of them
synthetic. Passing all of it says the program does what we thought to ask for. This is the
other question: pointed at fonts nobody designed a test around, does it crash, does it
produce a readable font, and does that font agree with fontTools?

Reproduce with:

```sh
cargo build --release -p slice-cli
tools/corpus-sweep.py --corpus /path/to/google/fonts --compare --json sweep.json
```

## The corpus

`google/fonts` at the checkout used here: 3869 font files across `ofl`, `apache` and
`ufl`, of which **775 carry an `fvar`**. Every one of them has `glyf` outlines — Google
Fonts ships **no CFF2 variable fonts at all**, so this sweep says nothing about the CFF2
path, which remains exercised only by a four-glyph synthetic fixture.

Sizes run from 31 KB to 49 MB, median 235 KB. The large end is CJK: the Chiron and Noto
Serif SC families.

## Result

Two jobs per font — every axis pinned at its default, and the first axis restricted to its
upper half — so 1481 jobs from 775 fonts.

| | |
|---|---|
| jobs succeeded | **1479 / 1481** (99.9%) |
| **static** (every axis pinned) | **774 / 775** (99.9%) |
| **partial** (one axis narrowed) | **705 / 706** (99.9%) |
| panics | **0** |
| outputs fontTools could not open | **0** |
| glyphs compared against fontTools | **177,154** |
| outline or advance disagreements | **0** |
| inputs modified | **0** of 775, sha256-verified before and after |

Both remaining failures are the same font, and neither is a wrong answer: fontTools cannot
instance it, so there is no reference to compare against. See below.

Nothing produced a malformed font, and nothing crashed.

## The one real defect this found

`axisregistry/tests/data/OpenSansCondensed-Italic[wght].ttf` has a two-axis `STAT` with an
`AxisValue` whose `AxisIndex` is 2. fontTools 4.62.1 cannot instance it at all —
`instantiateVariableFont` raises `IndexError: list index out of range` from
`designAxes[axisValueTable.AxisIndex].AxisTag`.

This program did instance it, and produced a valid 3431-glyph font. That looked like a
robustness win and was not: we survived because we were not looking. The out-of-range
record was copied straight through. An `AxisValue` naming an axis the design-axis array
does not have cannot describe a location, so it is now dropped — that font's `STAT` goes
from 9 axis values to 1, all well-formed. Fixed in `5acd6cb`, with a regression test that
asserts in both directions, since dropping *every* record would also satisfy "no dangling
record survived".

The tier-3 line still reports this font, because fontTools cannot produce a reference to
compare against. That is a limitation of the comparison, not a disagreement.

## The gap this quantified, and closed

The first run of this sweep found that a `GDEF` item variation store -- variable kerning
and anchors -- blocked partial instancing on **365 of the 706 variable fonts that have an
axis worth narrowing, 52%**, of which 184 had more than one axis. That was the largest
limitation in the program, and the corpus could never have shown it: the one fixture with a
`GDEF` variation store was used only by cases that pinned every axis, and at the time this
was written that looked harmless, on the belief that a static instance legitimately drops
the store. It does not -- each value has to be resolved at the pinned location first -- and
the static path was not even dropping it: it copied `GDEF` and `GPOS` through untouched, so
every static instance kerned like its default master. That was found in October 2026 from a
user's report, and `static.gdef-varstore.kerning-applied-at-the-pinned-location` now holds
it; see `tools/kerning-compare.py` in `tools/README.md` for what it measured.

It is fixed. `varstore::rebuild`, written for CFF2's `HVAR`, re-tents a store that carries
its own deltas, and that is the same operation `GDEF` needs; wiring it up took nine lines
and a guard. Partial instancing went from **341/706 (48.3%) to 705/706 (99.9%)** on the
same corpus, and `partial.variable-kerning-survives-a-restricted-range` now holds it, with
a check that samples an interior location because kerning frozen at the default is right
at the default and wrong everywhere else.

This is the argument for sweeping a real corpus in one sentence: the gap was documented
before the sweep, as one bullet among eight, weighted the same as "the progress bar does
not animate". The sweep is what said it was the whole ballgame.

## Speed

Median slice 23 ms, p95 100 ms, slowest 2.67 s (a 49 MB CJK font). The whole corpus is
54 seconds of CPU in the Rust binary; the sweep's twelve-minute wall time is almost
entirely fontTools building reference instances to compare against.

## What this does and does not license

It licenses a strong claim about both instancing paths: 1479 of 1481 jobs over 775 real
fonts, 177,154 sampled glyphs compared against the reference implementation, zero
disagreements, zero crashes, zero malformed outputs.

It licenses nothing about CFF2, because the corpus contains no CFF2 variable font. It
licenses nothing about overlap removal, which the sweep does not exercise — that remains
the least externally validated part of the program, because there is no reference
implementation to diff it against.
