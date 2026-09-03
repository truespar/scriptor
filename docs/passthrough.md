# Verbatim passthrough of unmodeled content

How Scriptor preserves **unmodeled content** - OLE objects, text boxes, non-picture VML shapes,
charts, SmartArt, content controls - through a `scriptor-crdt` round-trip, so `to_docx_bytes`
re-emits them instead of dropping them.

This is the project invariant - *"model what you edit; preserve everything else as verbatim
passthrough XML so round-trip stays byte-stable"* - applied inside `document.xml`.
`scriptor-ooxml` already gives byte-verbatim passthrough at the **part** level; this closes the
gap inside the one part `scriptor-crdt` **regenerates** from the model.

## Why it exists

`scriptor-crdt` does not preserve `document.xml` byte-for-byte - it captures the modeled subset
and regenerates the body from the loro model on export (`export_document_xml_via_nodes`). Import
is a whitelist state machine (`import_document_xml`): it recognizes modeled elements, and
anything else would have nowhere to live in the model. Without passthrough, these vanish on the
first round-trip:

| Content | Element | Handling |
|---|---|---|
| OLE objects (embedded Excel, etc.), ActiveX | `<w:object>`, `<w:control>` | run-level passthrough |
| Text boxes | `<w:drawing>`/`<w:pict>` wrapping `<w:txbxContent>` | run-level passthrough |
| Non-picture VML shapes (lines, callouts) | `<v:shape>`/`<w:pict>` without `<v:imagedata>` | run-level passthrough |
| Charts / SmartArt / WPS shapes | `<w:drawing>` with no `<pic:pic>`; `<mc:AlternateContent>` non-picture Choice | run-level passthrough |
| Content controls / custom XML (block-level) | `<w:sdt>`, `<w:customXml>` | wrapper passthrough, content stays editable |

(Pictures - inline, anchored, and table-cell - *are* modeled and round-trip via the image path.
Passthrough covers everything that is **not** a picture.)

## The mechanism: mirror the image path

An image is a single placeholder run holding `U+FFFC` (OBJECT REPLACEMENT CHARACTER) carrying an
`img~{id}` Peritext mark, with the picture's placement in a document-level `images` LoroMap keyed
by that id. Passthrough is the same, one map over:

- **A placeholder run** - `U+FFFC` carrying a **`raw~{id}`** Peritext mark (`ExpandType::None`,
  exactly like `img~`). It occupies one codepoint in the paragraph's run flow, so it holds its
  position and survives edits around it, and it re-emits in document order.
- **A document-level `RAW` LoroMap** - `id` → `{ xml: <verbatim source>, kind: <"object"|"pict"|…> }`.
  The `xml` is the raw source byte span of the captured `<w:r>` (see byte-fidelity below); `kind`
  is a diagnostic/GC hint.
- **Import** (`parse_passthrough` inside `import_document_xml`) - when a `<w:r>` contains
  unmodeled content, the entire `<w:r>…</w:r>` source span is captured, given an id in `RAW`, and
  a placeholder run with the `raw~{id}` mark is inserted at the current position.
- **Export** (`run_xml` / `para_xml`) - a run carrying a `raw~` mark emits `RAW[id].xml`
  **verbatim**, not `<w:r><w:t>` (the same branch point where an `img~` run emits a
  `<w:drawing>` and suppresses the placeholder text).

`Run.raw: Option<u64>` mirrors `Run.image`, read back from the `raw~` mark in `runs_from_delta`.

### Granularity: capture the whole `<w:r>`

When a `<w:r>` contains unmodeled content, the **entire run** is captured verbatim rather than
splitting its modeled text from the unmodeled object. Object-bearing runs are almost always
dedicated (a `<w:r>` holding just the `<w:object>`/`<w:pict>`/shape), so this is lossless in
practice and far simpler than sub-run splitting. The run is non-editable inline (you can't edit
an OLE object's XML by typing) - you select, delete, or replace it, matching Word treating an
embedded object as an atom.

### Detection: `parse_images` is the oracle

A `<w:r>` is captured verbatim when it contains `<w:object>`, `<w:control>`, `<w:pict>`, a
`<w:drawing>`, or an `<mc:AlternateContent>` - **and** feeding the run's byte slice to
`parse_images` yields **no modeled picture**. Because capture and image-modeling share one
detection function they can never disagree: a `<w:drawing>` with an `<a:blip>` (or a `<w:pict>`
with `<v:imagedata>`) stays on the modeled image path; anything else is preserved verbatim.
Raw-name matching means the sliced run needs no ancestor namespace declarations.

**Known boundary:** a non-picture shape whose text box contains an inline picture is left on the
image path (the oracle sees the nested picture and declines to capture) - the inner image still
renders, but the shape frame is not preserved. Rare; closing it would require distinguishing a
run's *own* drawing from a picture nested inside its `txbxContent`.

## Byte-fidelity: capture the source span, not re-serialized events

Re-serializing captured content through quick-xml **events** would rewrite namespace prefixes and
attribute order - not byte-stable. Instead the **raw byte span** of the `<w:r>` is sliced from
the original `document.xml`: record `reader.buffer_position()` at the unmodeled start tag and at
its matching end tag, slice the original bytes. Re-emitting that exact slice makes an *untouched*
passthrough run round-trip **byte-identical**.

## Namespaces

Captured spans use prefixes declared on ancestors (`w`, `wp`, `a`, `pic`, `mc`, `wps`, `wpg`,
`v`, `o`, `w10`, `m`, …). The regenerated body writes a fixed `DOC_HEAD` that declares the
**complete standard WordprocessingML namespace set** (the set Word itself emits), so a
passthrough run's prefixes always resolve in the output.

## Relationships and parts

Passthrough content references other package parts by relationship id - an OLE object points at
`word/embeddings/oleObject1.bin` via `r:id`, a VML fill at a media part. Both survive:

1. **The referenced parts** - `to_docx_bytes` clones `source_parts` verbatim and only replaces
   the parts it regenerates, so embeddings and media pass through untouched.
2. **The relationships** - `to_docx_bytes` only *appends* relationships (it never regenerates
   `document.xml.rels`), so every original relationship a passthrough span references survives.
   A dangling reference would make Word repair/strip the object.

**GC:** when the placeholder run is deleted, its `RAW` entry - and any rels/parts referenced only
by it - become orphans, collected on the same sweep that GCs orphan images.

## CRDT, editing, collaboration

- **Position + edits.** The placeholder is one codepoint with a Peritext mark, so
  inserting/deleting text around it keeps it correctly ordered - identical to an inline image.
- **Not editable inline.** There is no caret path *into* the captured XML; the run is an opaque
  atom: select, delete, or replace.
- **Collaboration.** The `RAW` entry is immutable, so concurrent edits around a passthrough run
  merge trivially - only the placeholder char + mark participate in the CRDT text merge.
- **Undo/redo.** Insertion/deletion of the placeholder run is a normal loro op.

## Tracked changes

An embedded object wrapped in a revision (`<w:ins>`/`<w:del>` around its `<w:r>`) keeps its
redline: `parse_passthrough` records the enclosing track (`RawItem.track`), the placeholder run
is inserted tracked (`insert_raw_placeholder_tracked`), and the `para_xml` raw branch re-wraps
the verbatim span in its `<w:ins>`/`<w:del>` on export - a direct mirror of the image-track path.
Because the placeholder is an ordinary tracked run, native **accept** keeps the object (bare run)
and **reject** removes it, with no passthrough-specific resolve code.
(`tracked_embedded_object_resolves_via_accept_reject`)

## Rendering

Passthrough is primarily a **save-fidelity** feature. The canvas paints a neutral labelled
placeholder box at the run's position, mirroring the inline-image path: `passthrough_xml()`
exposes the `rawxml` map; a `Run.raw` branch in `resolve_blocks` produces a
`scriptor_layout::Placeholder` with a `passthrough_label`-sniffed caption ("Embedded Object" /
"Chart" / "Shape" / "Text Box" / …) and a neutral ~2in×1.2in footprint; `layout_doc` reserves a
line of the box height (breaking pages like an inline image); `Renderer::paint_placeholder`
draws fill + soft border + a muted centred caption from both the body and cell paths. A caret
line makes the box a selectable stop.
(`passthrough_placeholder_reserves_a_line_and_places_a_box`,
`passthrough_object_renders_a_placeholder_box`)

## Block-level wrappers: content controls stay editable

Unmodeled *block* wrappers between paragraphs - `<w:sdt>` content controls and `<w:customXml>` -
round-trip **without freezing their content**, so a common editable control stays editable
instead of becoming an opaque block. The import
catch-all descends into `<w:sdtContent>`, so the inner blocks stay ordinary editable body
paragraphs; the wrapper's verbatim opening
(`<w:sdt><w:sdtPr>…</w:sdtPr><w:sdtContent>` / `<w:customXml…><w:customXmlPr>…</w:customXmlPr>`)
is captured and re-emitted around them on export:

- `parse_block_wraps` (a streaming pass, body-level only) captures each wrapper's opening + the
  `body_nodes` index range it encloses; a stack handles nesting (an outer wrapper's prefix stops
  before an inner `<w:sdt>`). The fixed close (`</w:sdtContent></w:sdt>` / `</w:customXml>`) is
  derived from the opening.
- `from_document_xml` anchors each wrapper to its enclosed nodes: the opening XML goes in the
  `blockwrap` map (by id), and `wrapopen` / `wrapclose` id lists ride the **first** / **last**
  inner block node's meta (outer-first opens, inner-first closes). Anchoring to nodes - not
  indices - means inserting or deleting paragraphs around the control keeps the wrapping correct.
- `export_document_xml_via_nodes` emits each node's `wrapopen` prefixes before it and `wrapclose`
  suffixes after, reconstructing the control around its (possibly edited) inner blocks.

**Boundary:** the wrapper opening is byte-verbatim, but the inner blocks are model-serialized
(an untouched simple paragraph round-trips byte-stable; anything the model normalizes follows
the serializer). Run-level and cell/row-level `<w:sdt>` wrappers are dropped, as before - their
content is already editable.
(`block_sdt_wrapper_round_trips_and_stays_editable`,
`nested_block_wrappers_round_trip_and_survive_edits`,
`oracle_holds_with_a_block_sdt_content_control`)

## Part-level passthrough

Everything above concerns unmodeled content *inside* `word/document.xml`. Whole parts are a
separate mechanism with the same goal: `source_parts` holds the opened package, and a save re-zips
it with only the parts the model owns replaced.

On a save that changed nothing, exactly four parts may differ:

| Part | Why |
|---|---|
| `word/document.xml` | regenerated from the model |
| `word/styles.xml` | merged into - modeled props patched in place, canonical quick styles appended |
| `[Content_Types].xml`, `word/_rels/document.xml.rels` | patched so synthesized `rIdImg`/`rIdLnk` ids resolve |

Everything else - theme, font table, settings, numbering, document properties, media, embeddings,
headers, footers, comments - comes back byte-identical.

**Headers, footers and comments are conditional, and that distinction is load-bearing.** Both are
re-rendered from a model that does not represent everything the part can hold: a header story is a
flat paragraph list, so a table in a header flattens to loose paragraphs, and a comment body is
plain text, so run formatting and a table inside a comment are discarded. Re-rendering them
unconditionally therefore destroyed content on a document that had only been opened and saved.
They are now rewritten only when actually edited - a `dirty` flag set by `hf_part_doc_mut` for
header and footer parts, and an equality check against the comments snapshotted at import.

The modelling limits still apply once you *do* edit one. See the Status section of the README.

## Correctness bar

**Scriptor reproduces every byte of OOXML it was given, except what the user or an agent actually
edited.** Not deleting is the floor; not *touching* is the bar. Rewriting an element or attribute
nobody asked us to change is a defect however harmless it looks - including "repairing" a value the
schema validator objects to. A document that arrives invalid leaves invalid, because a valid output
would be evidence we altered something. The 14 corpus documents recorded `invalid` in
[`tests/baselines/lo-ooxmlexport/`](../tests/baselines/lo-ooxmlexport/README.md) are that rule
working: they used to record `valid` only because the exporter deleted the section properties
carrying their malformed values.

The bar is a **byte-stable round-trip**: for an untouched document, `from_docx_bytes` →
`to_docx_bytes` reproduces every `<w:object>`/`<w:pict>`/non-picture `<w:drawing>`
byte-identically; a referenced `r:id` still resolves and its part is present; and the redline
oracle (accept-all = B, reject-all = A) holds with passthrough runs present. The CLI `coverage`
scan counts these elements as modeled-as-passthrough.

Two different corpus layers guard this, and they are not interchangeable. `scriptor roundtrip`
repacks the container and never builds the model, so it cannot see a defect in the save path at
all. `scriptor resave` opens each document through the CRDT and saves it straight back, which is
what catches part-level loss - see [`testing.md`](testing.md) layers 1 and 2. The header defect
above survived 1,347 documents precisely because only the first layer existed; adding the second
showed 325 of them were affected.

## The remaining hole: inside `document.xml`

Part-level passthrough is now sound, but it stops at the boundary of the one part that must be
rewritten. `parse_passthrough` captures a whole `<w:r>` verbatim only when a **whitelist** fires -
`w:object`, `w:control`, `w:drawing`, `w:pict`, `mc:AlternateContent`. Anything else the model does
not represent is simply not emitted, and there is no mechanism that notices.

`scriptor resave --elements` measures it by diffing the element histogram of `word/document.xml`
before and after a no-op save, needing no whitelist of interesting elements: anything that appears
fewer times afterwards is gone. Against the LibreOffice corpus (1,347 documents), **45 documents
survive and 1,296 lose something.** Ranked by first loss:

| Element | Documents | What it is |
|---|---:|---|
| `w:cols` | 576 | section column layout - 18 documents lose a genuine multi-column section |
| `w:b`, `w:color`, `w:bCs` | 201 | run formatting on paragraph marks |
| `a14:useLocalDpi`, `a:graphicFrameLocks`, `o:lock` | 109 | drawing extensions and locks |
| `w:bidi`, `w:docGrid`, `w:adjustRightInd` | 132 | section and paragraph layout properties |
| `w:bookmarkStart` | 28 | **named** bookmarks - 50 documents lose one, including `_Ref…` cross-reference targets |
| `w:r`, `w:br` | 25 | whole runs, mostly the footnote/endnote reference case |

The check discounts what Word regenerates on open (`w:proofErr`, `w:lastRenderedPageBreak`, and
`_GoBack` / `_Toc*` bookmarks), because counting those buried the signal - `_GoBack` alone accounts
for several hundred documents and losing it is harmless.

`--elements` is **off by default**, so the corpus gate still measures the part-level bar it can
actually hold. Turning it on is how this backlog gets burned down.

### What run-level capture now covers

`parse_passthrough` also captures a run whose content the model does not reproduce **when the run
carries no text** - a footnote or endnote reference, `w:sym`, a plain `<w:br/>` line break,
`w:ruby`, `w:ptab`, a separator. Those were neither modeled nor captured, so the run imported empty
and exported as nothing; a footnote survived in `footnotes.xml` while the reference pointing at it
vanished, leaving it orphaned and invisible in Word.

The no-text condition is deliberate, not a shortcut. Capturing a run makes it opaque - selectable
and deletable, never inline-editable - which is right for a run whose whole content is an unmodeled
element and wrong for prose that merely carries one alongside its text. A text-bearing run stays
modeled and still loses that child; closing *that* gap needs the model to represent the element, not
a wider capture.

This could not ship until placeholders were positional, and the reason is worth keeping. A captured
run's placeholder used to be appended at the **end of its paragraph** rather than inserted where the
run sat, so capturing a mid-paragraph run *moved its content*:

```
original run order:  text | text | text | SYM | text | text
after save        :  text | text | text | SYM
```

On the same document that shift also left comment id 16 anchored across two table cells, emitting
its markup twice and making a **valid** document schema-invalid. The whitelist had masked it
completely, because OLE objects, charts, shapes and content controls nearly always occupy a
paragraph of their own, where the two positions coincide. `RawItem::text_offset` now records the
codepoints of modeled text before the run, and the import inserts there.

## Known gaps

- No render chrome for a content control (a subtle boundary/tag in the canvas).
- No block passthrough for wholly opaque unmodeled blocks that have *no* editable inner
  content.
- Editing a header or footer that contains a table flattens the table, and editing any comment
  re-emits the whole comments part, so the other comments in it lose their run formatting. Both
  are safe until you edit; fixing them means giving those stories the same node-tree model the
  body has.
- Everything in the table above: content inside `document.xml` that the model does not represent
  is dropped on save, with no edit required. This is the largest open correctness gap, and the
  section above explains why widening the capture cannot close it until placeholders are
  positional.
- A run that carries text *and* an unmodeled child keeps the text and loses the child. Capture is
  all-or-nothing per run, and making prose opaque to save one element is the wrong trade.
- A **nested table** (a `<w:tbl>` inside a `<w:tc>`) is preserved verbatim on its enclosing cell,
  not modeled: a cell owns a slice of the flat paragraph list, which cannot express a table. Its
  content survives a save but is opaque - not laid out, not editable. Runs inside one are skipped by
  `parse_passthrough` (`tbl_depth >= 2`), because the table already carries them; capturing both
  emitted the same `v:shape` ids twice and made a valid document invalid.
- `m:oMath` is a sibling of runs at paragraph level, not a run child, so run-level capture cannot
  reach it. OMML needs block-level passthrough.
- Modeled properties are re-emitted from the model, so a value the model cannot represent exactly is
  normalized rather than reproduced. `tdf153255.docx` carries fractional page margins
  (`w:top="1133.8582677165355"`); they come back as integer twips. This is the one place the
  reproduce-don't-touch rule genuinely cannot hold as stated - the editor needs margins as numbers -
  and closing it means either sub-twip precision in the model or remembering the original spelling
  while the value is unchanged.

## Non-goals

- **Modeling the objects.** OLE/charts/SmartArt are not parsed into an editable model.
  Passthrough *preserves* them; it does not *understand* them.
- **Editing captured content.** The captured XML is opaque: select/delete/replace, never
  inline-edit.
- **Rendering them faithfully** in the canvas engine (only the placeholder box).
- **Byte-fidelity after the run is *touched*.** An untouched passthrough run round-trips
  byte-identically; once a revision or a neighbouring structural edit forces regeneration around
  it, the captured span is still re-emitted verbatim but its surrounding whitespace/formatting
  follows the model's serializer.
