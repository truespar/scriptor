# Bundled fonts: attribution + licenses

Scriptor bundles open, metric-compatible substitute fonts so a document that names a
proprietary Microsoft font lays out (line breaks, line heights, pagination) the same as in
Word, without shipping the proprietary font. **No Microsoft fonts are included** - each
bundled face is an independently authored clone engineered to match the named font's metrics.

| Bundled family | Substitutes for | License | Copyright / source |
|---|---|---|---|
| Carlito | Calibri | SIL OFL 1.1 | Copyright 2013 The Carlito Project Authors - https://github.com/googlefonts/carlito |
| Caladea | Cambria | SIL OFL 1.1 | Copyright 2012 The Caladea Project Authors - https://github.com/huertatipografica/Caladea |
| Gelasio | Georgia | SIL OFL 1.1 | Copyright 2022 The Gelasio Project Authors - https://github.com/SorkinType/Gelasio |
| Arimo | Arial / Arial Nova | SIL OFL 1.1 | Copyright 2020 The Arimo Project Authors - https://github.com/googlefonts/arimo |
| Liberation Sans Narrow | Arial Narrow | **GPLv2 + font-embedding exception** | Copyright 2010 Oracle and/or its affiliates (v1.07.5) - https://github.com/liberationfonts/liberation-sans-narrow |
| Tinos | Times New Roman | Apache-2.0 | Digitized data copyright 2010-2012 Google Corporation |
| Cousine | Courier New | Apache-2.0 | Digitized data copyright 2010-2012 Google Corporation |
| TeX Gyre Schola | Century Schoolbook | **GUST Font License (LPPL 1.3c)** | Copyright 2007-2018 B. Jackowski & J.M. Nowacki (GUST e-foundry) - https://www.gust.org.pl/projects/e-foundry/tex-gyre |
| TeX Gyre Pagella | Book Antiqua / Palatino | **GUST Font License (LPPL 1.3c)** | Copyright 2007-2018 B. Jackowski & J.M. Nowacki (GUST e-foundry) - https://www.gust.org.pl/projects/e-foundry/tex-gyre |
| TeX Gyre Bonum | Bookman Old Style | **GUST Font License (LPPL 1.3c)** | Copyright 2007-2018 B. Jackowski & J.M. Nowacki (GUST e-foundry) - https://www.gust.org.pl/projects/e-foundry/tex-gyre |
| DejaVu Sans (regular) | *(broad-Unicode fallback - not a metric substitute)* | **Bitstream Vera + DejaVu (permissive)** | Bitstream Vera (c) 2003 Bitstream Inc.; DejaVu changes are public domain - https://dejavu-fonts.github.io |

> **Licensing note - the TeX Gyre legal serifs are under the GUST Font License (LPPL 1.3c)**,
> not OFL/Apache. LPPL is a free, **non-copyleft** license (FSF: "not really copyleft at all");
> Debian and Fedora ship TeX Gyre as DFSG-free / in main, and the license permits commercial
> redistribution. Its one substantive clause - that *modified* fonts be renamed - is "requested,
> not legally required," and in any case Scriptor bundles the faces **unmodified**. These cover
> the legal-brief serifs the MS core set omits (Century Schoolbook is mandated by the US Supreme
> Court and several federal circuits; Book Antiqua / Palatino and Bookman are common in filings).
> They are metric-compatible with the URW base-35 fonts (advance-close to Word's cuts; letterforms
> correct-family), with a Word-tuned line-height factor (`line_height_factor` - TeX Gyre's own hhea
> does not match Word's, unlike the Croscore clones). This is a deliberate license exception, like
> Liberation Sans Narrow; to drop it, remove the three `TeXGyre*-*.otf` sets, their `substitute_family`
> arms, and their `line_height_factor` entries (unmapped legal fonts fall back to a generic serif).

> **Licensing note - Liberation Sans Narrow is GPLv2 + font-embedding exception**,
> not OFL/Apache like the rest. There is no OFL/Apache metric clone of Arial
> Narrow: the Narrow faces were *excluded* from the Liberation family when it
> moved to OFL 1.1 at v2.0, and remain under the original GPLv2 + exception. The
> embedding exception is purpose-built for this use - shipping the font in, and
> rendering/embedding it from, a non-GPL application does not impose the GPL on
> that application or on the documents it produces. Bundled **unmodified**. If a
> GPL asset is unacceptable for a given deployment, drop the four
> `LiberationSansNarrow-*.ttf` files and remove the `"arial narrow"` arm in
> `substitute_family` (it falls back to Arimo, the prior behaviour).

Full license texts in this directory:

- `OFL.txt` - SIL Open Font License 1.1 (covers Carlito, Caladea, Gelasio, Arimo).
- `LICENSE-Apache-2.0.txt` - Apache License 2.0 (covers Tinos, Cousine).
- `LICENSE-LiberationSansNarrow.txt` - the font-embedding exception (Liberation Sans Narrow),
  together with `GPL-2.0.txt` - the complete GNU GPL v2 text it extends.
- `GUST-FONT-LICENSE.txt` - GUST Font License / LPPL 1.3c (covers TeX Gyre Schola, Pagella, Bonum).
- `DejaVu-LICENSE.txt` - Bitstream Vera + DejaVu (permissive, non-copyleft) - the DejaVu Sans fallback.

**DejaVu Sans is a fallback, not a substitute.** It stands in for no MS font; `substitute_family`
never returns it. It exists only so the shaper (cosmic-text) can fall back to it PER GLYPH for
characters the metric clones lack (e.g. the U+05C0 Hebrew paseq used as a `|` separator in some
templates), rather than drawing a tofu box. The Bitstream Vera license is permissive (use, modify,
embed, redistribute, commercial), non-copyleft, and DFSG-free (Debian main).

Each font's own copyright statement (and any Reserved Font Name notice required by the OFL)
is preserved in its `name` table. The exact licence per face is whatever its embedded
metadata declares; the table above records it as of the bundled build.

The OFL/Apache clones were obtained as static per-weight TTFs from the Fontsource
distribution (https://fontsource.org) of the upstream Google Fonts / tyPoland projects
linked above. Liberation Sans Narrow (v1.07.5) was obtained from the upstream Liberation
project release (the standard Arial-Narrow-metric faces) and bundled unmodified.
