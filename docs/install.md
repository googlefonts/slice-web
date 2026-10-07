---
layout: default
permalink: /install/
title: "Install"
description: "Nothing at all for the browser; one build for the command line"
---

## The browser version

There is nothing to install.

**<https://googlefonts.github.io/slice-web/app/>**

It is a static page and a WebAssembly module. Any current version of Firefox, Chrome,
Edge or Safari runs it. Your font is read, sliced and returned by the browser itself, so
there is no account, no upload and no server-side component — you can load the page, go
offline, and slice a font with the network disconnected.

If you would rather host it yourself, build it and copy the directory anywhere that
serves static files:

```sh
git clone https://github.com/googlefonts/slice-web
cd slice-web
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
./build.sh
python3 -m http.server --directory dist 8080
```

The only requirement of the host is that it serves `.wasm` with the
`application/wasm` content type, which every static host does by default.

## The command line

`slice` is a single binary with no runtime dependencies — no Python, no fontTools, no
system libraries beyond libc.

```sh
git clone https://github.com/googlefonts/slice-web
cd slice-web
cargo build --release -p slice-cli
```

The binary lands at `target/release/slice`. Copy it onto your `PATH`:

```sh
install -m755 target/release/slice ~/.local/bin/slice
```

Needs a Rust toolchain, which you can get from [rustup.rs](https://rustup.rs). Any stable
Rust from 1.85 onwards will do; that floor comes from the `read-fonts` dependency rather
than from this code.

### Linux, macOS, Windows

The same two commands on all three. There is nothing platform-specific in the engine, and
no code signing or notarisation step, because there is no desktop bundle to sign — this is
where the browser version earns its keep relative to the original, which needs a
per-platform installer, an Apple Developer certificate and a notarisation round-trip for
every macOS release.

On Windows use PowerShell and `cargo build --release -p slice-cli`; the binary is
`target\release\slice.exe`.

## Verifying a build

```sh
cargo test --workspace       # 192 tests
tests/suite/run.py           # the 298-case conformance corpus, both implementations
```

The corpus bootstraps its own virtual environment with PyQt5 and fontTools on first run,
because it drives the *original* Slice as a library to compare against. That is only
needed for the corpus; nothing in Slice itself uses Python.

## What about a desktop app?

There isn't one, and that is deliberate. The original ships a `.dmg` and a `.exe` because
a PyQt5 application has to; a page that works on every platform at one URL does not. If
you want something that behaves like an installed app, every current browser will install
this one as a standalone window — in Chrome and Edge, *Install page as app* from the
address bar.
