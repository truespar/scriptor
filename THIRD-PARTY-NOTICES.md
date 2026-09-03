# Third-party notices

Scriptor's own source is dual-licensed MIT OR Apache-2.0 (see [LICENSE](LICENSE)). This file
covers everything in the tree that is *not* Scriptor's own source, and it is the file to read
before redistributing a build.

Two categories matter, and they matter differently:

- **Bundled fonts** are shipped binary assets. They travel with the crate and end up inside your
  build. Their terms bind you.
- **Dependencies** are fetched by Cargo and pnpm, not vendored here. Their terms bind you too,
  but no copy of them lives in this repository.

## Bundled fonts (shipped assets)

The fonts in `crates/scriptor-fonts/fonts/` are third-party works under their own licenses. They
are metric-compatible open clones, bundled so a document naming a proprietary Microsoft font
paginates as it does in Word without shipping that font. **No Microsoft fonts are included.**

| Bundled family | License |
|---|---|
| Carlito, Caladea, Gelasio, Arimo | SIL Open Font License 1.1 |
| Tinos, Cousine | Apache License 2.0 |
| TeX Gyre Schola / Pagella / Bonum | GUST Font License (LPPL 1.3c) |
| Liberation Sans Narrow | **GPL v2 with the font-embedding exception** |
| DejaVu Sans | Bitstream Vera / DejaVu (permissive) |

Two of these are not what a reader who sees only "MIT OR Apache-2.0" would expect, so they are
spelled out here.

**Liberation Sans Narrow** is under GPL v2 with the standard font-embedding exception. That
exception means shipping the font inside a non-GPL application, and rendering or embedding it from
one, does not place the GPL on that application or on the documents it produces. It is a bundled
data asset, not linked code: it is not combined with Scriptor's source, and it does not reach
through the Apache-2.0 half of the dual license.

**The TeX Gyre faces** are under LPPL 1.3c, a free, non-copyleft license that the FSF describes as
"not really copyleft at all"; Debian and Fedora both ship these fonts in main.

All faces are bundled unmodified. If a GPL-licensed asset is unacceptable in your deployment, the
four `LiberationSansNarrow-*.ttf` files can be removed without touching anything else. Full license
texts sit beside the fonts, and [`crates/scriptor-fonts/fonts/NOTICES.md`](crates/scriptor-fonts/fonts/NOTICES.md)
lists every face, its copyright and source, and how to drop it.

## Rust dependencies

The Cargo tree is permissive throughout. There is no GPL-only, AGPL, or LGPL-only crate in it.
Regenerate this picture with `cargo license --avoid-build-deps`. The parts worth knowing:

| Crate(s) | License | Note |
|---|---|---|
| `im`, `sized-chunks`, `bitmaps` | MPL-2.0+ | Weak, file-level copyleft, reached via `loro`. Linking is unrestricted; only modifications to those crates' own files must be shared. Using them does not affect Scriptor's license or yours. |
| `self_cell` | Apache-2.0 OR GPL-2.0 | Via `cosmic-text`. Take it under Apache-2.0. |
| `sync_wrapper`, `unicode-linebreak` | Apache-2.0 only | No MIT option, so an Apache-2.0 notice obligation applies to a redistributed binary. |
| `tiny-skia`, `tiny-skia-path`, `arrayref`, `matchit`, `moxcms`, `pxfm` | BSD-2/3-Clause | Attribution only. |
| `xxhash-rust` | BSL-1.0 | Attribution only. |
| `unicode-ident` | (Apache-2.0 OR MIT) AND Unicode-3.0 | Unicode data license, permissive. |

Everything else in the tree is MIT, Apache-2.0 OR MIT, ISC, Zlib, or Unlicense.

## Node dependencies

The published TypeScript packages ship no bundled third-party code; their runtime dependency is
the Scriptor WASM build. The pnpm tree is build and test tooling (Vite, Turborepo, Biome, Vitest,
Playwright, tsup, TypeScript) and is MIT/ISC/Apache-2.0/BSD, except `lightningcss` (MPL-2.0), which
arrives through Vite as a build tool and is not linked into any published artifact.
