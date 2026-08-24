# Third-party licences

## The Slice icon

`docs/assets/slice-icon.svg` is the original Slice project's icon, carried over so that
this version is recognisable as the same tool. It is itself a derivative of the
["cheesecake" icon](https://www.flaticon.com/free-icon/cheesecake_3400263) released by
flaticon.com, used under the Flaticon license, which permits commercial and personal use,
modification and derivative works. See [Flaticon-License.txt](Flaticon-License.txt).

## TypeRoof

`crates/slice-web/js/font-store.js` is adapted from `lib/js/local-font-storage.mjs` in
[TypeRoof](https://github.com/FontBureau/TypeRoof) by Font Bureau, used under the Apache
License 2.0. The structure is theirs: a promise-wrapped IndexedDB handle, one object store
keyed by a stable name, and the font's bytes kept in the record. Slice stores the slicing
settings alongside the bytes, evicts by least-recent use, and returns plain objects
because the caller is WebAssembly rather than JavaScript. See
[Apache-2.0.txt](Apache-2.0.txt).

Apache-2.0 is one-way compatible with the GNU General Public License v3, which is why this
combination is permitted; the resulting work is distributed under the GPL.

## Recursive

`web/fonts/RecursiveSans-Slice_mod.subset.ttf` and the fonts in `testdata/fonts/` are
derivatives of the [Recursive typeface](https://github.com/arrowtype/recursive) by
Stephen Nixon, used under the SIL Open Font License 1.1. See
[Recursive-OFL.txt](Recursive-OFL.txt).

The file in `web/fonts/` is the subset the original Slice used for its wordmark: the
variable font sliced at `MONO=0, CASL=0.5, wght=800, slnt=0, CRSV=1` and then subset with
`pyftsubset` to the characters in the word "Slice". It is carried over unchanged so this
version's wordmark matches the application it replaces. At 2.8 kB it costs nothing.

`testdata/fonts/` holds the test fixtures copied from the original repository; see
[testdata/README.md](../testdata/README.md).

## Roboto Delta

`testdata/fonts/RobotoDelta-avar2.subset.ttf` is a four-glyph subset of
[Roboto Delta](https://github.com/googlefonts/roboto-delta) by the Roboto Delta Project
Authors, used under the SIL Open Font License 1.1. It is here because it is an `avar`
version 2 font — 39 axes that move each other — and there is no other one to hand: Google
Fonts ships none, and fontTools 4.62.1 can read that table but not build one, so it cannot
be generated the way the other fixtures are. The copy came from
[skeravar](https://github.com/simoncozens/skeravar)'s test data.

## The original Slice

This project reimplements the interface and behaviour of
[Slice](https://github.com/source-foundry/Slice) by Source Foundry (Christopher
Simpkins), which is licensed under the GNU General Public License v3. This project
carries the same licence.

## Rust dependencies

The crates this depends on are Apache-2.0 / MIT, which the GPL-3.0 permits combining
with. `cargo tree` lists them; `cargo about` or `cargo deny` can produce a full report.
