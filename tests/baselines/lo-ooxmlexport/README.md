# Conformance baseline: LibreOffice `sw/qa` ooxmlexport corpus

These JSON files are the checked-in expected result of running Scriptor's conformance checks over
the LibreOffice `sw/qa/extras/ooxmlexport/data` corpus. `scripts/corpus-gate.ps1` compares a fresh
run against them, so a change that makes a previously-good document fail is caught.

## Files

- `roundtrip.json` - per-doc status of `scriptor roundtrip <corpus>`: `stable` (parse →
  re-serialize is byte-identical), `unstable`, or `error` (not openable - encrypted CFB, an
  `.odt` mis-named `.docx`).
- `resave.json` - per-doc status of `scriptor resave <corpus>`, which opens each doc through the
  CRDT and saves it straight back with no edit: `lossless` (every part the model does not own came
  back byte-identical, none were dropped, and no visible text disappeared from
  `word/document.xml`), `lossy`, or `error`.
- `validation.json` - per-doc status of remodelling each doc through the CRDT and validating the
  output with the Open XML SDK (`tools/ooxml-validate`): `valid`, `invalid`, or `unreadable`.

Each is `{corpus, count, docs: {file → status}}`, sorted by file name for a stable diff.

A check added after a baseline was captured has nothing to diff against. The gate reports it as
`NO BASELINE`, skips it, and names it again in the summary rather than failing - so a new layer
cannot break the gate for everyone before someone with a corpus has run `-Update`.

## Why 2 documents are recorded `lossy`

They lose visible text from `word/document.xml` on a save that changed nothing. Down from 35.

- **Nested tables: 22 documents, now 0.** A table inside a cell cannot be modeled - a cell owns a
  slice of the flat paragraph list - and the importer skipped its paragraphs silently. It is now
  captured verbatim and re-emitted in place, opaque like an embedded object.
- **Text boxes: 11 documents, now 1.** Two related defects in picture detection. A picture inside a
  `w:txbxContent` made its text box look like an ordinary picture run, and an `<a:blip>` inside an
  `<a:blipFill>` - a shape's *background fill* rather than a picture - did the same to a box with a
  picture fill. Either way the box was declined for verbatim capture and the modeled image path
  emitted the picture alone, hoisting it to body level and dropping every word in the box.

The two that remain:

- `fdo61343.docx` - a VML **canvas group** holding a picture shape *and* text-box shapes. The
  picture is not inside a `w:txbxContent`, so it is a genuine picture by every test the capture
  oracle has, the run is not captured, and the whole canvas collapses to one image. Fixing it means
  capturing any drawing that contains a text box *and* suppressing that drawing's pictures from
  placeholder insertion, or they emit twice.
- `tdf170003_bottomSpacing.docx` - not loss in the ordinary sense: a cached `PAGE` field result
  (`- 2 -`) is replaced by the computed placeholder (`1`), which is what the field path is for. It
  shows up here only because the placeholder is shorter than the cached text.

Two things this measure deliberately does **not** count as loss, both learned by getting it wrong
first. An entity reference is one character, so `'` rewritten as `&apos;` is not a lost apostrophe -
counting only text events reported 38 documents falsely. And a `w:tab` is one character only as a
run child; the identical element inside `<w:pPr><w:tabs>` is a tab *stop*, and counting those
reported 115 documents falsely.

The element histogram behind `--elements` reports far more documents than this, and most of it is
not loss: merging two adjacent runs with identical formatting removes a `w:rPr` and a `w:b` without
changing a character. 189 of the 219 corpus documents that "lose" a `w:b` keep every bold character.
Text loss is the measure that does not produce noise, which is why it is checked by default.

## Why 17 documents are recorded `invalid`

`validation.json` marks 17 documents invalid. **Every one of their inputs is invalid**, verified
with the same validator, and every error in our output also appears in the input - we reproduce
their invalidity, we do not add to it.

Fourteen are section properties. Thirteen carry `w:charSpace` as an unsigned 32-bit value
(`4294961151` for `-6145`) and one, `tdf76683_negativeTwipsMeasure.docx`, carries `w:w="-1"` and
`w:space="-1"` on a column. Word reinterprets both; the Open XML SDK rejects them. They used to be
recorded `valid`, and that was the bug: the exporter synthesized a fresh `sectPr` and dropped
`w:docGrid` and `w:cols` outright, so the output validated - a passing validator bought with silent
data loss.

Two more joined when run-level passthrough widened to catch content the model does not represent.
`tdf111964.docx` produces the identical error its input has. `tdf153255.docx` has 7 errors as
shipped; our output has 3 of those same 7, the dangling `footnoteReference` ids, which are now
preserved instead of deleted along with the references.

`tdf127085.docx` joined when picture detection stopped mistaking a shape's background fill for a
picture: its `mc:Fallback` VML is now captured verbatim instead of being replaced by a modeled
picture, so the undeclared `ID` attribute it has always carried comes back with it. Identical error,
same path, in both input and output.

Scriptor does not repair OOXML it was not asked to touch, so a document that arrives invalid leaves
invalid.

The baseline records the true state and freezes it: these 14 cannot get worse without the gate
noticing, and no other document may go `valid -> invalid`.

**A refinement worth making:** the schema layer measures output validity in isolation, so it cannot
tell "we broke this" from "it arrived broken". Validating the corpus originals once and gating on
`input valid -> output invalid` would classify all 14 automatically and need no baseline entry.

## The corpus is NOT vendored

The corpus documents are LibreOffice `sw/qa` test files (MPL and bug-tracker attachments): usable
as test inputs but not redistributable, so they are not in this repo. Only the results (these
baselines) are committed. Check the corpus out locally and point at it:

```
git clone --depth 1 https://git.libreoffice.org/core   # sw/qa/extras/ooxmlexport/data
pwsh -File scripts/corpus-gate.ps1 -Corpus <path-to>/sw/qa/extras/ooxmlexport/data
```

or set `$env:SCRIPTOR_CORPUS` to that path. It exits non-zero on any regression.

## Updating

After an intentional fidelity change (or when the corpus revision changes), refresh the baseline:

```
pwsh -File scripts/corpus-gate.ps1 -Update
```

Review the diff before committing: an `-Update` that folds in regressions defeats the purpose.
Improvements (a doc going `invalid → valid`) are the expected reason to update.

## Manifest

`manifest.json` records the corpus source + the revision the baseline was captured against, so a
mismatch (new/removed docs) is explainable rather than surprising.
