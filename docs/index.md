---
layout: default
---

<div class="hero" markdown="0">
  <img src="assets/slice-icon.svg" alt="">
  <h1>Slice</h1>
  <p>Build custom design sub-spaces from variable fonts — one static instance, or a
     variable font with fewer axes and narrower ranges. In your browser.</p>
  <p>
    <a class="button" href="app/">Try it now</a>
    <a class="button secondary" href="manual/">Read the manual</a>
  </p>
</div>

Slice takes a variable font and gives you a smaller one. Pin an axis to freeze it, narrow
an axis to keep it variable over less of its range, correct the names and style bits so
the result identifies itself properly, and — new in this version — **remove overlapping
contours**, which after more than a decade still trips up a great deal of design software.

Your font never leaves your machine. The page is static files and a WebAssembly module:
the browser reads the font, slices it, and hands it straight back as a download. There is
no server and nothing to upload. Load the page, go offline, and it still works.

## Where to go

<ul class="cards" markdown="0">
  <li>
    <h3><a href="manual/">User's manual</a></h3>
    <p>The whole tool, start to finish: the three editors, the axis syntax and the rules
       behind it, overlap removal, output formats, the command line, and what every
       error message means. Also as a <a href="manual/slice-manual.pdf">PDF</a>.</p>
  </li>
  <li>
    <h3><a href="install/">Install</a></h3>
    <p>Nothing to install for the browser version. For the command line, one cargo
       build. Linux, macOS and Windows.</p>
  </li>
  <li>
    <h3><a href="evidence/">Evidence</a></h3>
    <p>What is tested and how it was measured: 302 conformance cases in plain English,
       775 real fonts swept, and every defect adjudicated with the measurement behind
       it.</p>
  </li>
  <li>
    <h3><a href="developer/">Developer</a></h3>
    <p>Building from source, running the tests, how the code is laid out, and how to
       contribute a change.</p>
  </li>
</ul>

## How it differs from the original

This is a reimplementation of [Slice](https://github.com/source-foundry/Slice) by Source
Foundry. The interface is deliberately close, because the original's interface is good and
people know it. Four things are different.

**It removes overlaps.** The original never did. fontTools does this through Skia's path
ops, which has no WebAssembly build, so the union here is computed with
[`linesweeper`](https://crates.io/crates/linesweeper), a robust Bentley–Ottmann sweep
line in pure Rust.

**It runs in a browser.** No install, no Python environment, no platform builds to code
sign and notarise.

**It fixes four defects.** Measured, not asserted — see
[the adjudication](adjudication.html). The original clears every exposed `fsSelection`
bit on save, deletes name IDs 16, 17, 21 and 22, ignores the extension you type when
choosing the output container, and accepts `wght=300:1e3` by silently deleting the weight
axis.

**It says what it cannot do.** Measured over every variable font in Google Fonts, 1479 of
1481 slicing jobs succeed and 177,153 sampled glyphs match fontTools exactly; the two that
do not are a font fontTools itself crashes on. What remains unsupported is listed in the
[manual](manual/#known-limitations) rather than left to be discovered.
