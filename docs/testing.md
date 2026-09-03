# Testing & fidelity harness

Microsoft Word decides whether an OOXML file is correct, and the manual loop is to run a command,
open the output `.docx` in Word, and check it survived. That does not scale across a corpus, so
several automated layers stand in. The first four need no reference renderer, neither Word nor
LibreOffice; the visual and geometry layers do.

What runs where:

| Layer | Needs | Runs on |
|---|---|---|
| 1. Round-trip (`scriptor roundtrip`) | the Rust CLI | anywhere |
| 2. Resave (`scriptor resave`) | the Rust CLI | anywhere |
| 3. Schema conformance (`ooxml-validate`) | .NET 9 SDK | anywhere |
| 4. Corpus check (`corpus-gate.ps1`) | PowerShell 7 (`pwsh`), .NET 9 SDK, a local corpus | anywhere |
| 5. Word verification (`word-verify.ps1`) | Windows + Microsoft Word | Windows only |
| 6. Coverage scanner (`scriptor coverage`) | the Rust CLI | anywhere |
| 7. Visual diff (`visual-diff.ps1`) | `pwsh`, LibreOffice or Word, ImageMagick | Windows for `-Reference word` |
| 8. Geometry comparison (`geometry-diff.ps1`) | Windows + Microsoft Word | Windows only |

Layers 1, 2, 3 and 6 need no PowerShell. Layer 4 is a PowerShell script, but PowerShell is
cross-platform: install `pwsh` and it runs on Linux and macOS. Only the layers that automate Word
over COM are Windows-bound. CI runs the portable Rust and TypeScript checks. It does not run the
corpus check, because the corpus is not redistributable and cannot be fetched there.

## 1. Round-trip check (`scriptor roundtrip`)

Asserts parse → model → serialize is **byte-stable** for a file or a whole tree:

```sh
cargo run -p scriptor-cli -- roundtrip in.docx out.docx
cargo run -p scriptor-cli -- roundtrip corpus-dir --json results.json   # non-zero exit on drift
```

## 2. Resave check (`scriptor resave`)

Opens a `.docx` through the CRDT and saves it straight back with **no edit**, asserting that every
part the model does not own comes back byte-identical, that none are dropped, and that no visible
text disappears from `word/document.xml`:

```sh
cargo run -p scriptor-cli -- resave in.docx
cargo run -p scriptor-cli -- resave corpus-dir --json results.json   # non-zero exit on loss
```

This is the only layer that reaches `to_docx_bytes`, the path a browser host calls on Ctrl+S.
Layer 1 is a container repack that never builds the model, and layer 3's `remodel` goes through
`export_docx`, which replaces `word/document.xml` and copies every other part verbatim. So a defect
that lives in the model's save path is invisible to both - which is exactly how header and footer
parts came to be re-rendered on every save, flattening a table in a header to loose paragraphs on a
document nobody had edited.

The set of parts the save path may legitimately rewrite is `REWRITABLE` in
`crates/scriptor-cli/src/main.rs`: the body, the styles it merges into, and the content-type and
relationship parts it patches. Anything else changing is a defect. Headers, footers and the comment
parts are deliberately **not** in that set, because they are re-rendered only when edited.

Inside `word/document.xml` - which must be rewritten, so a part-level check cannot see into it - the
measure is **visible text**: every character in a `w:t` or `w:delText`, wherever it sits. If the
count falls, something was dropped. Nothing else about the body is gated by default, because
nothing else is unambiguous: merging two adjacent runs with identical formatting removes a `w:rPr`
and a `w:b` without changing a character, so an element-level comparison reports loss where there is
none. 189 of the 219 corpus documents that "lose" a `w:b` keep every bold character.

"Every character" needs two qualifications, both learned by getting the count wrong and being
caught by an independent measurement:

- An **entity reference** is one character. quick-xml reports it separately from the text around it,
  so counting only text events made `'` rewritten as `&apos;` read as a lost apostrophe - 38
  documents reported falsely.
- A **`w:tab` is one character only as a run child.** The identical element inside `<w:pPr><w:tabs>`
  is a tab *stop* definition, and counting those made every document whose tab stops the model
  normalizes read as losing text - 115 documents reported falsely.

`--elements` turns on that stricter, noisier element histogram for measuring the modelling backlog.

## 3. Schema conformance (`tools/ooxml-validate`)

A small .NET wrapper over the Open XML SDK's `OpenXmlValidator` (MIT, dev-only): validates
every `.docx` you point it at and exits non-zero on schema errors. Run it over anything
Scriptor writes (`roundtrip` / `remodel` / `compare` / `accept` / `reject` outputs):

```sh
dotnet run --project tools/ooxml-validate -- <file-or-dir>
```

## 4. Corpus regression check (`scripts/corpus-gate.ps1`)

Runs layers 1, 2 and 3 over a corpus, reduces each to a `{file → status}` map, and diffs against a
**checked-in baseline** (`tests/baselines/<corpus-id>/`). Any `stable → unstable`,
`lossless → lossy` or `valid → invalid` transition fails the run, so a change that reintroduces a
fidelity regression fails loudly instead of passing silently. `-Update` refreshes the baseline -
review the diff and fold in improvements, never regressions.

A check added since the baseline was captured has nothing to diff against. It reports
`NO BASELINE` and is skipped rather than failing the run, so a new layer cannot take the gate down
for everyone before someone with a corpus has run `-Update`. Capture it once, review the results,
and commit the new baseline JSON.

The corpus itself is **not vendored** (LibreOffice `sw/qa` files are MPL / bug-tracker
attachments - usable as test inputs, not redistributable). Check it out locally and point at
it (or set `$env:SCRIPTOR_CORPUS`):

```powershell
git clone --depth 1 https://git.libreoffice.org/core   # sw/qa/extras/ooxmlexport/data
pwsh -File scripts/corpus-gate.ps1 -Corpus <path>/sw/qa/extras/ooxmlexport/data
```

See [`tests/baselines/lo-ooxmlexport/README.md`](../tests/baselines/lo-ooxmlexport/README.md)
for the baseline format and update workflow.

## 5. Verification against Word (`scripts/word-verify.ps1`)

Some defects are schema-valid yet make Word refuse the file. This opens each output in real
Word via COM with `OpenAndRepair:=false` (a damaged package makes `Open()` throw - that is the
detection) and compares normalized text plus word/revision/comment/page counts against the
original. Each open runs in a watchdog'd child job with a timeout, so a document that blocks
`Open()` on a modal dialog is recorded `open-timeout` rather than hanging the batch. The
script PID-diffs around each job and kills only Word instances it spawned - never a
developer's open documents.

## 6. Coverage scanner (`scriptor coverage`)

Walks the body content parts of one `.docx` or a whole tree, counts every distinct OOXML
element, and buckets them: **GAPS** (unsupported, visible effect - the backlog, ranked by how
many files use them), **IGNORABLE** (no visual effect), **MODELED**. The curated lists live in
`crates/scriptor-cli/src/main.rs` - move an element to `MODELED` when you implement it. Point
it at a wide corpus for a standard-wide backlog:

```sh
cargo run -p scriptor-cli -- coverage file-or-folder
```

## 7. Visual diff (`scripts/visual-diff.ps1`, `scripts/corpus-visual-diff.ps1`)

Renders a document with Scriptor (`scriptor render`) **and** a reference renderer
(LibreOffice headless by default, or Word via COM), rasterizes both at matched DPI, and
`magick compare`s per page → a difference score + a red-overlay diff image per page.
`corpus-visual-diff.ps1` batches this over a folder and ranks documents worst-first.

```powershell
pwsh scripts\visual-diff.ps1 -Docx file.docx                   # vs LibreOffice
pwsh scripts\visual-diff.ps1 -Docx file.docx -Reference word   # vs Word (truest; COM)
pwsh scripts\visual-diff.ps1 -Docx redline.docx -Reference word -Track none  # compare Final view
```

Read the score as a **regression tracker** and the diff image as a per-page inspector, not an
absolute fidelity gauge - it is dominated by reference-renderer differences. Pass `-Track` so
the reference matches the tracked-change display mode you rendered.

Prerequisites: LibreOffice (`soffice`) and ImageMagick (`magick`) on `PATH`; the CLI built
(`cargo build -p scriptor-cli --release`).

## 8. Geometry comparison (`scripts/geometry-diff.ps1` + `scripts/corpus-scorecard.ps1`)

Pixel diffs are noisy and do not say where the problem is. This layer compares **per-paragraph
layout in points** instead: `scriptor geometry` (the engine's caret geometry) against
`scripts/word-geometry.ps1` (Word via COM, forced to Print Layout), pairing paragraphs by text and
reporting page mismatches, vertical and horizontal differences, and list-marker mismatches. A
non-zero median dY in the summary points to a systematic offset; "every paragraph 26 pt high"
means a spacing or metrics bug. `corpus-scorecard.ps1` runs it over a folder and ranks every
document by divergence, with a does-it-even-open column up front.

## 9. The compare oracle (`scriptor compare --check`)

The comparison engine has a structural correctness guarantee: for any pair (A, B),
`compare(A, B)` then **accept-all reproduces B; reject-all reproduces A**. `--check` asserts
both directions and reports pass/fail, which is the corpus-wide check for redline completeness.

## Fixtures

Two `.docx` files are committed, under `crates/scriptor-crdt/tests/fixtures/`. They are shared by
the Rust tests and the browser test project rather than duplicated.

- `sample.docx` - minimal: four parts, two paragraphs. Used where a test needs a document and
  nothing more.
- `rich.docx` - **generated, do not hand-edit.** Sixteen parts, chosen so most of them are things
  the model does not model: a theme, a font table, numbering, settings, custom document properties,
  an embedded binary object and a PNG. That is what makes it useful for the passthrough tests -
  those parts can only survive a save by being carried through verbatim. Its header is a two-cell
  table on purpose, because the header story the model keeps is a flat paragraph list; that is the
  regression behind `an_unedited_header_is_not_re_rendered`. Regenerate with:

  ```sh
  node scripts/make-rich-fixture.mjs
  ```

  The script is deterministic (fixed zip timestamps, fixed part order), so a run on an unchanged
  script leaves the working tree clean. If `git status` shows the fixture as modified after a run,
  that is the bug report.

Both are generated or authored here rather than taken from a corpus, so they carry the repository's
licence. The LibreOffice `sw/qa` files used by the corpus check are deliberately *not* vendored:
they are MPL and bug-tracker attachments, usable as test inputs but not redistributable.

## Word COM hygiene

Never blanket-kill `WINWORD` to clear a stuck COM run - every script here PID-diffs and
targets only the automation instances it launched, so a developer's open documents are never
touched. Keep it that way in new scripts.
