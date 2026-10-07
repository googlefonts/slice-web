---
layout: default
permalink: /license/
title: "Licences"
description: "The application, this documentation, and the third-party work both build on"
---

## The application

Slice is free software under the **GNU General Public License, version 3**. The full text
is in [LICENSE](https://github.com/googlefonts/slice-web/blob/main/LICENSE).

It carries that licence because the program it reimplements does:
[Slice](https://github.com/source-foundry/Slice) by Source Foundry (Christopher Simpkins)
is GPL-3.0, and this project's interface and behaviour derive from it.

## This documentation

The documentation is distributed under the same terms as the application. The manual's
source is
[`docs/manual/manual.md`](https://github.com/googlefonts/slice-web/blob/main/docs/manual/manual.md);
the PDF and this website are both generated from it.

## Third-party work

Recorded in full in
[`thirdparty/`](https://github.com/googlefonts/slice-web/tree/main/thirdparty).

**The Slice icon** is the original project's, carried over so this version is recognisable
as the same tool. It is a derivative of the
["cheesecake" icon](https://www.flaticon.com/free-icon/cheesecake_3400263) by flaticon.com,
used under the Flaticon license, which permits commercial and personal use, modification
and derivative works.

**Recursive**, by Stephen Nixon, is used for the wordmark and as a test fixture, under the
SIL Open Font License 1.1. The wordmark font is the subset the original Slice used: the
variable font sliced at `MONO=0, CASL=0.5, wght=800, slnt=0, CRSV=1` — with Slice — and
then subset to the letters of the word.

**The Rust dependencies** are Apache-2.0 or MIT, which GPL-3.0 permits combining with.
`cargo tree` lists them in full.

Unlike the original, this version does not ship **IBM Plex Mono**: the interface asks for
the reader's own monospace font instead of shipping one, which saves every visitor a
download for a face most systems already have.

## What the licence means for a font you slice

Nothing. The GPL covers Slice, not its output. A font you put through Slice comes out
under whatever licence it went in with, and that licence is your responsibility — most
open font licences, the SIL OFL included, have conditions about modification and naming
that apply to an instance you generate. The OFL in particular requires that a modified
version not use a Reserved Font Name, which is worth checking before you publish an
instance.
