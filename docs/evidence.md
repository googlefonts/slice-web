---
layout: default
permalink: /evidence/
title: "Evidence"
description: "What is tested, how it was measured, and what is still unknown"
---

A font tool that quietly gets something wrong is worse than one that refuses, because
nothing tells you. So the claims this project makes are testable, the tests are readable,
and every number quoted anywhere in this repository has a committed script that
reproduces it.

## The documents

<ul class="cards" markdown="0">
  <li>
    <h3><a href="../test-suite.html">The test suite in plain English</a></h3>
    <p>All 301 conformance cases, each with the reasoning for why that is the right
       answer. Generated from the cases themselves, so it cannot drift from them.</p>
  </li>
  <li>
    <h3><a href="../adjudication.html">Adjudication</a></h3>
    <p>Every case the original Slice fails, sorted into "the test was wrong" and "the
       program is wrong", with the measurement behind each verdict.</p>
  </li>
  <li>
    <h3><a href="../real-world-sweep.html">The real-world sweep</a></h3>
    <p>775 real variable fonts from Google Fonts: 177,154 glyphs compared against
       fontTools with no disagreements, and the 52% gap it found — since closed.</p>
  </li>
  <li>
    <h3><a href="../original-behaviour.html">The behaviour map</a></h3>
    <p>The numbered map of the original program's behaviour that the whole suite is
       written against.</p>
  </li>
</ul>

## How it is checked

**Against fontTools, field by field.** The original Slice is a thin interface over
`fontTools.varLib.instancer`, so the comparison harness runs the real library on the same
input with the same request and compares outlines, advances, `maxp`, the `head` bounding
box, `hhea`'s extremes, `OS/2`'s classes, `post.italicAngle`, the name IDs, `fvar`, `STAT`
and which lookups each feature runs. This is the only check that can see a step being
skipped altogether, and it found six defects every other test was blind to.

**Against skrifa**, a separate implementation of the same specification, and the one
browsers and Android render with. Drawing the variable font at location L must give the
same outlines as drawing our instance at L at its own default. For static instances the
observed deviation is **0 font units**, and the test asserts exactly that rather than a
tolerance.

**Against fontTools' own test vectors** for the sub-space solver, lifted verbatim from a
pinned release. All 32 pass.

**By filled region, for overlap removal**, because there the outline is supposed to
change and what must not change is which points are inside the glyph. That alone is
blind to a merge that does nothing, so the result is also checked for overlaps left
behind, by skia-pathops rather than by our own engine.

**Against 775 real fonts**, which is the only check that can say what fraction of the
world a limitation actually affects. It found one that mattered — variable kerning
blocking partial instancing on 52% of them — which is now fixed.

## Two rules

> **A case the original passes, this implementation must also pass.** A case the original
> fails is adjudicated on evidence: either the test is wrong and gets fixed, or the
> original has a defect and this implementation must not copy it. Nothing is resolved by
> changing an expectation to match whatever a program happens to do.

> **A number worth writing down needs a committed script that produces it.** The probes
> under `tests/suite/probes/` and `tools/` exist for that. A claim whose evidence lives
> only in a shell history is a claim nobody can check, including its author later.

## What is still unknown

Stated because a page about evidence that only lists successes is advertising.

- **CFF2 on real fonts.** Google Fonts ships no CFF2 variable fonts at all, so the sweep
  tested none of that path. It is exercised by a four-glyph synthetic fixture and by a
  charstring-level comparison against fontTools, and by nothing else.
- **Overlap removal at scale.** There is no reference implementation to diff it against —
  fontTools' instancer does not remove overlaps — so it is checked for self-consistency
  and by filled region, and on **one** real font by an independent engine. The fixtures
  are rectangles and triangles; real glyphs are curves meeting at shallow angles, which is
  where boolean geometry goes wrong. The one real font is Google Sans Flex Condensed
  Black, after a user found overlaps left in it: `tools/overlap-check.py` found 45 glyphs
  still overlapping or carrying stray paths, from two bugs the filled-region tests could
  not see, and finds none now they are fixed. That is one font of the 775 the sweep reads.
  The engine (`linesweeper`) was chosen by measurement rather than by reputation — see
  `tools/overlap-engine-eval/` — but that measurement used the same simple shapes.
- **The interface.** Ten of the sixty-one behavioural claims describe the running
  application — menus, the status bar, drag and drop, the thread the work runs on — and a
  corpus that drives both programs headlessly cannot reach them. Of the fifty-one it can
  reach, all fifty-one have at least one case.
