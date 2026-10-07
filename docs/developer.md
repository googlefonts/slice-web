---
layout: default
permalink: /developer/
title: "Developer documentation"
description: "Building from source, running the tests, and contributing a change"
---

## Getting set up

Needs a Rust toolchain (1.85 or newer, from [rustup.rs](https://rustup.rs)), and for the
browser build the `wasm32-unknown-unknown` target and `wasm-pack`:

```sh
git clone https://github.com/googlefonts/slice-web
cd slice-web

rustup target add wasm32-unknown-unknown
cargo install wasm-pack

cargo build --release -p slice-cli   # the command line
./build.sh                           # the browser app, into dist/
./build.sh --dev                     # faster, bigger .wasm, panics keep their messages
```

Serve the built page with anything static:

```sh
python3 -m http.server --directory dist 8080
```

Python 3.12 with fontTools is needed only to run the conformance corpus and the
comparison harnesses, never to build or use Slice. `tests/suite/run.py` bootstraps its own
virtual environment on first run.

## How the code is laid out

```
crates/slice-core   the engine: no browser-specific dependencies, so all of it
                    can be tested from a native test binary
crates/slice-cli    a command-line front end, for tests and build scripts
crates/slice-web    the Leptos interface, compiled to WebAssembly
tools/              scripts that produce or re-check committed artefacts
tests/suite/        the conformance corpus: declarative cases and a shared checker
testdata/           fixtures, copied from the original repository
docs/               this website, and the manual's source
```

The engine is built on [fontations](https://github.com/googlefonts/fontations):
`read-fonts` for reading, `write-fonts` for writing, and `skrifa` both as a dependency and
as the thing many tests are checked against.

Inside `slice-core`, the pieces worth knowing first are `axes.rs` (the axis-cell grammar),
`job.rs` (a slice request and its validation), `instancer/` (the variation maths — static,
partial, CFF2, and the sub-space `solver`), `overlaps.rs` (the boolean union) and
`finalize.rs` (recalculating everything a rewritten font invalidates).

## Running the tests

```sh
cargo test --workspace              # 192 tests
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings

tests/suite/run.py                  # 302 conformance cases, both implementations
tests/suite/run.py --runner ours    # just this one
tests/suite/run.py --case axis.     # filter by id prefix
tests/suite/run.py --verbose        # per-check detail on failures

tools/browser-smoke.sh              # that it starts in a real browser and reads a font
tools/browser-slice-test.py         # that pressing Slice produces the right font
tools/compare-with-fonttools.py     # that the result matches fontTools
tools/compare-cff2-with-fonttools.py

tools/commit-stamp-check.sh         # that the build stamp in the status bar follows HEAD
```

CI runs all of these on every push. Three things it deliberately does **not** run: the
manual build, which would mean carrying a TeX installation on the build machine; the
775-font corpus sweep, which needs a checkout of `google/fonts`; and the build-stamp
probe, which clones the repository and builds it three times to answer a question about
cargo's caching, and is worth running when `crates/slice-web/build.rs` changes rather
than on every push.

## How correctness is established

There is no Rust equivalent of fontTools' instancer, so the variation maths here is a
port, and a port is worth only as much as the evidence that it matches. Five independent
checks, described in full in [the evidence pages](../evidence/):

- **The sub-space solver** is checked against fontTools' own parametrised test table,
  lifted verbatim from a pinned release by `tools/gen-solver-vectors.py`.
- **Instancing** is checked against skrifa, a separate implementation of the same
  specification: drawing the variable font at location L must give the same outlines as
  drawing our instance at L at its own default. Static instances agree to **0 font
  units**.
- **CFF2** charstrings are compared program by program against fontTools, and then drawn
  and compared again.
- **Overlap removal** is checked by filled region rather than by outline, because the
  outline is meant to change and what must not change is which points are inside.
- **The whole pipeline** is diffed field by field against fontTools, which is the only
  check that can catch a step being skipped altogether.

## Contributing

Pull requests are welcome at
[github.com/googlefonts/slice-web](https://github.com/googlefonts/slice-web).

A change to behaviour wants a case in `tests/suite/cases/`, not only a Rust unit test. The
cases are declarative JSON evaluated by a shared checker and run against *both*
implementations, so a case cannot be quietly written to flatter one of them.
[`tests/suite/README.md`](https://github.com/googlefonts/slice-web/blob/main/tests/suite/README.md)
describes the format, the available check kinds and the fixtures.

Two rules the corpus is under, both learned the hard way:

> **A case the original passes, this implementation must also pass.** A case the original
> fails is adjudicated on evidence before anything is concluded from it — either the test
> is wrong and gets fixed, or the original has a defect and this implementation must not
> copy it. Nothing is resolved by changing an expectation to match whatever a program
> happens to do.

> **A number that goes into a commit message, a document or a comment needs a committed
> script that reproduces it.** The probes under `tests/suite/probes/` and `tools/` exist
> for exactly this. A claim whose evidence lives only in a shell history is a claim nobody
> can check.

If you change the documentation, `docs/manual/manual.md` is the manual's only source —
run `docs/manual/build.py` afterwards, which regenerates the LaTeX, the PDF and the web
page together. `docs/test-suite.md` is generated too, by `tests/suite/gen-docs.py`.

## Reporting a problem

File it on the [issue tracker](https://github.com/googlefonts/slice-web/issues). A font
that reproduces the problem is worth more than anything else you can include; if you
cannot share it, the output of `slice info yourfont.ttf --json` describes the design space
without shipping the outlines. From the web version, quote the commit hash in the status
bar: it is a link to the exact commit the page was built from, and it identifies what you
were running in a way the version number cannot, since every push is republished.

Problems with the *original* Slice belong on
[its own tracker](https://github.com/source-foundry/Slice/issues) — though if the
behaviour is one of the four defects catalogued in
[the adjudication](../adjudication.html), it is already known.
