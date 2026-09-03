# Scriptor

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

> The collaborative Microsoft Word (`.docx`) editor engine: view, edit, redline, and compare Word
> documents in the browser or headless, with native track changes, real-time multi-user
> collaboration, and AI agents editing alongside people. Self-hosted.

Clients are stuck with Word. Scriptor takes a different route than a Word plugin: an editor that
reads and writes Word's own file format (**OOXML**) directly, written in **Rust** and compiled to
**WebAssembly**, so it renders and edits `.docx` documents in the browser with near-Word fidelity.
No conversion in or out: the file that comes back is the same `.docx`, including native track changes
(`w:ins` / `w:del` / `w:rPrChange` / `w:pPrChange` / `w:moveFrom` / `w:moveTo`) and threaded comments.

### Why not a Word add-in, or a converter?

An Office.js add-in needs a live Word instance behind every document, so it is not headless, not
embeddable in your product, and not self-hostable. Using converters (`.docx` → HTML/Markdown →
`.docx`) can drop revision marks, threaded comments, numbering, and every part of the file you did
not touch. Scriptor edits the format itself: model what you edit, and preserve everything else in
the native OOXML format (see [`docs/passthrough.md`](docs/passthrough.md)).

## What it does

- **Lossless `.docx` read/write.** Parse → model → serialize is byte-stable for anything you do not
  edit; unmodeled content (OLE objects, charts, SmartArt, text boxes …) is preserved as-is.
- **Native track changes (redlining).** Tracked typing, deletion, run + paragraph formatting,
  paragraph splits/joins, moves, and table revisions; accept/reject one change, all, or by author;
  Word's four display modes (All Markup / Simple / No Markup / Original).
- **Compare tool.** Compare two `.docx` files and write a third in which every difference is a
  tracked change. `scriptor compare --check` verifies the result: accepting everything reproduces
  B, rejecting everything reproduces A.
- **High-fidelity page rendering.** Pages rendered to `<canvas>` (or headless RGBA): the
  `styles.xml` hierarchy, numbering, tables (grid / merges / borders / shading), inline + anchored
  images, headers/footers, `PAGE`/`NUMPAGES` fields, tab stops - with **metric-compatible font
  substitution** (Calibri→Carlito, Times New Roman→Tinos, …) using bundled open clones, so line
  breaks and pagination track Word's.
- **Real-time collaborative editing.** The document is a [loro](https://github.com/loro-dev/loro)
  CRDT, merged across peers by an `axum` websocket relay that runs standalone or embeds as a library
  in your own server.
- **AI agents that redline.** A headless, attributed agent API (`scriptor-client::AgentPeer`):
  perceive the document (outline / read-node / find), propose **atomic batches** of tracked changes,
  comment, and review. Every change is stamped with the agent's `w:author`, policies are pluggable,
  and the whole surface is reachable from any language over a JSON wire contract. It needs no Word
  instance, and an agent emits only its edits rather than the whole document.

Both kinds of participant run on one CRDT. A human over a websocket and a headless agent emit the
same authored tracked-change operations, so multi-user editing and agent participation share a
single mechanism, and attribution survives any merge order.

## Architecture

Rust Cargo workspace (the engine) and a pnpm + Turborepo TypeScript workspace
(the browser editor and framework wrappers).

### Rust crates (`crates/`)

| Crate | Responsibility |
|---|---|
| `scriptor-ooxml` | Lossless OOXML model (`zip` + `quick-xml`): revisions + accept/reject, comment threading, passthrough of unmodeled XML. |
| `scriptor-crdt` | OOXML ↔ loro CRDT binding (`CollabDoc`): tracked suggestions, comments, tables, images, concurrent merge - re-serialized to valid, Word-openable OOXML. |
| `scriptor-edit` | The one edit path: typed `EditOp` + `EditContext` shared by the editor and agents. |
| `scriptor-compare` | Document comparison: deterministic diff replayed as authored tracked changes, plus the accept=B / reject=A oracle. |
| `scriptor-layout` | Canvas layout engine: shaping (cosmic-text / rustybuzz / swash) → line-break → pagination → paint. |
| `scriptor-fonts` | Metric-compatible font policy + bundled open clones (Apache-2.0 / OFL). |
| `scriptor-wasm` | wasm-bindgen glue: open / relayout / paint / edit from the browser, plus native headless rendering. |
| `scriptor-server` | `axum` websocket relay: rooms, snapshot-on-join, live broadcast, in-memory or Postgres persistence. Also embeddable as a library (room actor + `Persistence` trait). |
| `scriptor-client` | Agent participation: `AgentPeer` (perceive / propose / comment / review) + the JSON wire contract. |
| `scriptor-cli` | The `scriptor` test bench (see [Testing](#testing--fidelity)). |

### TypeScript packages (`packages/`)

| Package | Responsibility |
|---|---|
| `@truespar/scriptor-core` | Framework-agnostic editor core: a headless canvas view (render, caret, selection, editing) over the wasm engine, plus the live-collaboration provider. |
| `@truespar/scriptor-ui` | Word-style chrome: ribbon, reviewing pane, side-by-side compare view, rulers, status bar. Optional. |
| `@truespar/scriptor-wasm` | The Rust engine compiled to WebAssembly, with typed bindings. |
| `@truespar/scriptor-vue` / `-react` / `-svelte` / `-web-component` | Thin framework wrappers over the core. |

`apps/playground` is the development playground (Vite).

## Quick start

Prerequisites: Rust (stable), Node ≥ 22, pnpm. The browser editor additionally needs
[`wasm-pack`](https://rustwasm.github.io/wasm-pack/) and the `wasm32-unknown-unknown` target; the
schema validator needs the .NET 9 SDK.

The CLI test bench needs only Rust:

```sh
cargo run -p scriptor-cli -- inspect file.docx            # list parts, count revisions/comments
cargo run -p scriptor-cli -- roundtrip in.docx out.docx   # assert byte-stable round-trip
cargo run -p scriptor-cli -- redline in.docx out.docx --author "Agent"  # inject a tracked change
cargo run -p scriptor-cli -- accept in.docx out.docx      # accept tracked changes (--id N for one)
cargo run -p scriptor-cli -- compare a.docx b.docx out.docx --check     # redline two docs + oracle
cargo run -p scriptor-cli -- render file.docx out-dir     # paint pages to PNG
```

For the editor playground, run `pnpm build` first. It compiles the Rust engine to WebAssembly, and
the dev server cannot resolve `@truespar/scriptor-wasm` until it has.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

pnpm install && pnpm build                                # builds the wasm package + the TS packages
pnpm --filter @scriptor/playground dev                    # Vite dev server on :5174
cargo run -p scriptor-server                              # optional: collab relay on :8091
```

The playground is the full Word-style editor (open a `.docx`, track changes, compare documents). For
embedding the editor in your app, the headless Rust agent path, and collaboration wiring, see
[`docs/usage.md`](docs/usage.md).

## Why Word fidelity is hard

ECMA-376 specifies OOXML's syntax, not Word's layout behaviour. It defines `w:spacing` and its
attributes, but not the rule that Word uses the larger of one paragraph's space-after and the next
paragraph's space-before instead of adding them, nor that a document opting into a legacy
compatibility mode gets the sum instead. Rules of that kind have to be measured from Word's output.

Small errors compound down the page. Two points of paragraph spacing is invisible at the top of page
1 and has become an extra page by page 6, so layout bugs usually show up as a page-count mismatch
and are fixed by measuring rather than by reading the spec.

Style resolution produces most of them. The precedence order is document defaults, then table style,
then numbering, then paragraph style, then direct formatting. A paragraph in a table cell with no
explicit style still takes the default paragraph style, which outranks the table style; inverting
those two shifted every paragraph in every table and added a page to a six-page contract.

Fonts produce the rest. Documents ask for Calibri, Cambria and Times New Roman, none of which can be
redistributed, so Scriptor substitutes metric-compatible open clones (Carlito, Caladea, Tinos,
Arimo, Gelasio). These match advance widths, so lines usually break in the same places, but they do
not match every kerning pair, and a long line occasionally wraps a word early.

Word is also backward compatible with its own earlier behaviour, through `w:compat` flags,
`w:cantSplit`, and rules like `titlePg` leaving a first-page header blank instead of falling back to
the default one.

## Testing & fidelity

Microsoft Word is the reference implementation, but opening every output by hand does not scale
across thousands of documents. The testing framework has: round-trip and schema checks that need no
reference renderer, verification against Word itself, a scanner ranking unsupported OOXML elements,
and pixel and geometry diffs against LibreOffice or Word. See [`docs/testing.md`](docs/testing.md).

The checked-in baselines under [`tests/baselines/`](tests/baselines/lo-ooxmlexport/README.md) record
the expected per-document result, so a regression fails the run.

### Where it stands today

Over the LibreOffice `sw/qa/extras/ooxmlexport` corpus (**1,347 documents**, revision `337ed97`),
against the checked-in baselines:

| Check | Result |
|---|---|
| Round-trip byte-stability (parse → re-serialize is identical) | 1,343 / 1,347 stable; the 4 failures are encrypted CFB containers Scriptor cannot open |
| Schema validity after a full CRDT remodel (Open XML SDK) | 1,139 / 1,340 valid |

Individual real-world documents are also compared against Word itself. `scripts/word-geometry.ps1`
uses Word over COM to record every paragraph's page number and x/y position, and
`scripts/geometry-diff.ps1` reports the per-paragraph difference from Scriptor's layout of the same
file.

Both checks measure preservation rather than rendering. A document can round-trip byte-for-byte and
still paginate differently from Word, and nothing in the repo catches that automatically.

### Testing against real documents

The test data is mostly synthetic: regression files written by LibreOffice developers for specific
bugs. It says little about the documents people work in day to day, such as a long contract with a
numbered clause scheme, a filing with a rotated stamp in the footer, or a report template with a
table spanning several pages.

Reports of documents that render incorrectly are the most useful contribution, and they require no
code. The file itself is not required either, since most Word documents are confidential; a
description of what Word shows and what Scriptor shows is enough to start. See
[CONTRIBUTING.md](CONTRIBUTING.md).

## Status

Scriptor is early stage, so expect to find documents that paginate differently from Word.

Known gaps:

**Layout and rendering**

- Body paragraphs do not split across a page boundary. One that does not fit moves whole to the next
  page, leaving a short page and shifting the content after it. Table rows do split, at line
  granularity, honouring `w:cantSplit`.
- Substitute fonts match advance widths but not every kerning decision, so a long line occasionally
  wraps one word early. Bulleted paragraphs with hanging indents are the worst case.
- `w:evenAndOddHeaders` (distinct odd/even page headers) round-trips verbatim but is not honoured
  when rendering.
- `w:w` character-width scaling is not applied.
- Style-level paragraph borders (`w:pBdr`) are not re-emitted into a regenerated `styles.xml`.
- Text inside anchored text boxes renders, including rotated stamps, but is not editable.

**Model coverage**

- Nested tables (depth ≥ 2) are not modelled. They are preserved verbatim across a save, so nothing
  is lost, but they are opaque: not laid out and not editable, like an embedded object. Modelling
  them means giving a cell block items rather than a paragraph count.
- One corpus document still loses text from a text box, down from 11; `scriptor resave` reports it.
- A table inside a header or footer is not modelled: the header story is a flat paragraph list.
  Opening and saving is safe, because an unedited header or footer is passed through verbatim, but
  editing one that contains a table flattens it to plain paragraphs - the cell text survives, the
  structure does not. Body tables are unaffected and round-trip fully.
- A comment body is modelled as plain text, so run formatting, paragraph properties and any table
  inside a comment are not represented. As with headers, opening and saving is safe - an untouched
  comment set is passed through verbatim - but editing any comment re-emits the whole part and the
  other comments in it lose their formatting. Comment text, author, threading and resolved state
  round-trip either way.
- Footnotes, endnotes, OMML math and `w:sym` are neither modelled nor caught by the run-level
  passthrough, so they are dropped on save. Replacing the importer's whitelist with a catch-all
  passthrough is the largest piece of open work. Unmodeled content that is covered (OLE objects,
  charts, SmartArt, text boxes, content controls) survives verbatim - see
  [`docs/passthrough.md`](docs/passthrough.md).
- The non-All-Markup display modes are render-only. Edit in All Markup.

**Live collaboration**

- Table structure is not yet held in the CRDT, so a peer joining a live session does not see tables
  in that document; open it through the standalone path instead. Media does not transfer to a
  joining peer either.
- The bundled relay (`scriptor-server`) has no authentication: any client that knows a document id
  can join it. This is deliberate, since the relay is meant to be embedded as a library or run
  behind your own authenticating proxy, but it should not be exposed on a public port as-is. See
  [SECURITY.md](SECURITY.md).

**User interface**

- The UI is themeable through `--scr-*` CSS custom properties, but some components (the rulers'
  canvas ticks, the reviewing pane's per-change colours) still paint hardcoded values and do not
  follow a dark theme correctly.

## Contributing

Open an issue or a pull request. There is no CLA, no contributor agreement, and no requirement to
discuss a change first. Contributions are dual-licensed MIT OR Apache-2.0, the same as the
project, and you keep the copyright in what you write.

Reports of documents that render incorrectly are more useful than code.
[CONTRIBUTING.md](CONTRIBUTING.md) covers how to report one when you cannot share the file, how to
run the corpus check, and what is not required of you.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
MIT is shorter and is compatible with GPLv2; Apache-2.0 additionally grants an explicit patent
licence. Take whichever suits you - you do not have to satisfy both. See [LICENSE](LICENSE).

The bundled substitute fonts are third-party works under their own licenses: SIL OFL 1.1,
Apache-2.0, the GUST Font License (LPPL 1.3c), and, for Liberation Sans Narrow, GPLv2 with the
font-embedding exception. That exception is written so that bundling the font and rendering from it
does not place the GPL on this software or on documents it produces.
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) covers the fonts and the dependency tree;
`crates/scriptor-fonts/fonts/NOTICES.md` lists every face, its license, and how to remove it.
