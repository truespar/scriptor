use super::*;
use loro::LoroDoc;

/// An image's placement round-trips through the `images` map (per-field, so a concurrent resize +
/// crop would converge), and a run carrying an `img~{id}` mark round-trips `Run.image` through
/// `write_runs` / `read_paragraphs` - the editable-image foundation (images-editing P1).
#[test]
fn image_placement_and_run_mark_round_trip() -> Result<()> {
    use loro::{ExpandType, StyleConfig, StyleConfigMap};
    let doc = LoroDoc::new();
    // The hosting doc configures the img~ mark key (CollabDoc does this via `configure_marks`).
    let mut styles = StyleConfigMap::new();
    styles.insert("img~3".into(), StyleConfig { expand: ExpandType::None });
    doc.config_text_style(styles);

    // Inline placement round-trips.
    let inline = ImagePlacement {
        media: "image1.png".into(),
        w_emu: 914400,
        h_emu: 685800,
        ..Default::default()
    };
    write_image(&doc, 3, &inline)?;
    doc.commit();
    assert_eq!(read_image(&doc, 3), Some(inline));

    // A placeholder run carrying img~3 round-trips `Run.image`.
    append_paragraph(&doc, &[Run { image: Some(3), ..Run::plain(IMAGE_PLACEHOLDER.to_string()) }], None)?;
    doc.commit();
    assert!(
        read_paragraphs(&doc)?.iter().flat_map(|p| &p.runs).any(|r| r.image == Some(3)),
        "the image run mark round-trips"
    );

    // A floating placement with a crop round-trips every field (LWW per key).
    let floating = ImagePlacement {
        media: "image2.png".into(),
        w_emu: 100,
        h_emu: 200,
        crop_l: 5000,
        crop_r: 5000,
        floating: true,
        behind: true,
        h_from: "page".into(),
        x_emu: 12,
        wrap: "square".into(),
        ..Default::default()
    };
    write_image(&doc, 4, &floating)?;
    doc.commit();
    assert_eq!(read_image(&doc, 4), Some(floating));

    // Delete clears the placement.
    delete_image(&doc, 4)?;
    doc.commit();
    assert_eq!(read_image(&doc, 4), None);
    Ok(())
}

/// Word's outline headings carry numbering on the *style* (Rubrik1 -> a list, Rubrik2 -> the same
/// list at the next level via basedOn), not on the paragraph. `resolve` must surface the effective
/// num_id / num_ilvl through the basedOn chain so "1." / "1.1" markers render.
#[test]
fn style_numbering_resolves_through_basedon() {
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="Rubrik1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>
  <w:pPr><w:numPr><w:numId w:val="27"/></w:numPr></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Rubrik2"><w:name w:val="heading 2"/><w:basedOn w:val="Rubrik1"/>
  <w:pPr><w:numPr><w:ilvl w:val="1"/></w:numPr></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;
    let t = parse_styles(xml);
    let r1 = t.resolve(Some("Rubrik1"));
    assert_eq!((r1.num_id, r1.num_ilvl), (Some(27), None), "Rubrik1: list 27, level defaults to 0");
    let r2 = t.resolve(Some("Rubrik2"));
    assert_eq!((r2.num_id, r2.num_ilvl), (Some(27), Some(1)), "Rubrik2 inherits list 27, sets level 1");
    let n = t.resolve(Some("Normal"));
    assert_eq!(n.num_id, None, "Normal has no numbering");
}

/// A table style's `pPr` spacing sits between docDefaults and the paragraph style for cells.
/// `TableGrid` sets `after=0 line=240`, so a cell paragraph is single-spaced with no space-after
/// instead of inheriting docDefaults' body spacing (after=160, line=259) - which otherwise
/// inflated every table row and over-paginated dense tables.
#[test]
fn table_style_spacing_layers_below_the_paragraph_style() {
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="table" w:styleId="TableGrid"><w:basedOn w:val="TableNormal"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr></w:style>
<w:style w:type="table" w:styleId="TableNormal"><w:name w:val="Normal Table"/></w:style>
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;
    let t = parse_styles(xml);
    // A body paragraph keeps docDefaults spacing.
    let body = t.resolve(None);
    assert_eq!((body.space_after, body.line_spacing), (Some(160), Some(259)), "body keeps docDefaults");
    // A cell paragraph in a TableGrid table picks up the table style's spacing.
    let cell = t.resolve_in_table(None, Some("TableGrid"));
    assert_eq!((cell.space_after, cell.line_spacing), (Some(0), Some(240)), "table style overrides docDefaults");
    // A styleless table (no tblStyle) is unchanged from the body resolution.
    let none = t.resolve_in_table(None, None);
    assert_eq!((none.space_after, none.line_spacing), (Some(160), Some(259)), "no table style = docDefaults");
}

/// A style-less body paragraph inherits the DEFAULT paragraph style (`w:default="1"`, "Normal"),
/// not bare docDefaults: when Normal overrides docDefaults to single spacing / no space-after
/// (FDO76248), `resolve(None)` must follow Normal (after=0, line=240), not docDefaults
/// (after=200, line=276) - else the body over-paginates. When Normal sets nothing, docDefaults
/// stand (the common case, unchanged).
#[test]
fn styleless_paragraph_follows_the_default_style_over_docdefaults() {
    let over = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="200" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr></w:style>
</w:styles>"#;
    let t = parse_styles(over);
    assert_eq!(t.default_para_style.as_deref(), Some("Normal"), "default paragraph style recorded");
    let body = t.resolve(None);
    assert_eq!((body.space_after, body.line_spacing), (Some(0), Some(240)),
        "style-less body follows Normal's override, not docDefaults");

    // Common case: Normal declares no spacing -> docDefaults stand unchanged.
    let inherit = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="200" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;
    let t2 = parse_styles(inherit);
    let body2 = t2.resolve(None);
    assert_eq!((body2.space_after, body2.line_spacing), (Some(200), Some(276)),
        "Normal with no override leaves docDefaults intact");
}

#[test]
fn table_style_borders_resolve_through_basedon() {
    // TableGrid carries the grid lines (top/left/bottom/right/insideH/insideV); its base
    // TableNormal carries none. A conditional `w:tblStylePr` band's borders must NOT leak into
    // the style's base borders.
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="table" w:styleId="TableNormal"><w:name w:val="Normal Table"/></w:style>
<w:style w:type="table" w:styleId="TableGrid"><w:basedOn w:val="TableNormal"/>
  <w:tblPr><w:tblBorders>
    <w:top w:val="single" w:sz="4" w:color="auto"/>
    <w:left w:val="single" w:sz="4" w:color="auto"/>
    <w:bottom w:val="single" w:sz="4" w:color="auto"/>
    <w:right w:val="single" w:sz="4" w:color="auto"/>
    <w:insideH w:val="single" w:sz="4" w:color="auto"/>
    <w:insideV w:val="single" w:sz="4" w:color="auto"/>
  </w:tblBorders></w:tblPr>
  <w:tblStylePr w:type="firstRow"><w:tblPr><w:tblBorders>
    <w:bottom w:val="single" w:sz="24" w:color="FF0000"/>
  </w:tblBorders></w:tblPr></w:tblStylePr>
</w:style>
</w:styles>"#;
    let t = parse_styles(xml);
    let b = t.resolve_table_borders(Some("TableGrid"));
    // Every edge inherits the single 4-eighths line; the firstRow band's 24-eighths bottom is
    // NOT folded into the base (so `bottom` stays 4, not 24).
    assert_eq!(b.top.as_ref().map(|e| e.size_eighths), Some(4), "top from style");
    assert_eq!(b.bottom.as_ref().map(|e| e.size_eighths), Some(4), "base bottom, not the band's 24");
    assert!(b.inside_h.is_some() && b.inside_v.is_some(), "interior grid lines resolved");
    // A style with no borders (TableNormal) and an unstyled table resolve to nothing.
    assert!(t.resolve_table_borders(Some("TableNormal")).top.is_none());
    assert!(t.resolve_table_borders(None).top.is_none());
}

#[test]
fn table_style_run_props_resolve_through_the_table_style() {
    // TableGrid sets blue (00B0F0) size-9 (sz 18) run text; a cell with no direct/paragraph
    // formatting inherits it. The table style sits below the paragraph style, so a paragraph
    // style colour would still win - here Normal sets none, so the table style shows through.
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="table" w:styleId="TableGrid"><w:rPr><w:color w:val="00B0F0"/><w:sz w:val="18"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;
    let t = parse_styles(xml);
    let cell = t.resolve_in_table(None, Some("TableGrid"));
    assert_eq!(cell.color.as_deref(), Some("00B0F0"), "cell inherits the table style colour");
    assert_eq!(cell.size, Some(18), "cell inherits the table style size");
    // No table style -> the run props come from the paragraph chain only (here, nothing).
    let body = t.resolve_in_table(None, None);
    assert_eq!((body.color, body.size), (None, None), "no table style = no inherited run props");
}

#[test]
fn paragraph_style_highlight_resolves() {
    // A paragraph style can carry a highlight in its rPr; a run with no direct highlight inherits
    // it. ("none" means explicitly no highlight, distinct from unset.)
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="HL"><w:rPr><w:highlight w:val="cyan"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Plain"><w:rPr><w:highlight w:val="none"/></w:rPr></w:style>
<w:style w:type="table" w:styleId="TG"><w:rPr><w:highlight w:val="yellow"/></w:rPr></w:style>
</w:styles>"#;
    let t = parse_styles(xml);
    assert_eq!(t.resolve(Some("HL")).highlight.as_deref(), Some("cyan"), "para style highlight");
    // `highlight="none"` is PRESERVED (as the literal "none"), not folded to unset - it cancels an
    // inherited highlight at render (highlight_rgb("none") paints nothing).
    assert_eq!(t.resolve(Some("Plain")).highlight.as_deref(), Some("none"), "explicit none kept");
    assert_eq!(t.resolve(None).highlight, None, "no style = no highlight");
    // Table styles carry highlight too (cell runs inherit it via resolve_in_table).
    assert_eq!(t.resolve_in_table(None, Some("TG")).highlight.as_deref(), Some("yellow"));
}

/// The main document part is whatever `_rels/.rels` points at (Type .../officeDocument), not
/// necessarily `word/document.xml` - some tools emit `word/trial.xml` (tdf104713 crashed on it).
#[test]
fn main_document_part_reads_the_officedocument_target() {
    let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/trial.xml"/>
</Relationships>"#;
    assert_eq!(main_document_part(rels).as_deref(), Some("word/trial.xml"));
    // No officeDocument relationship -> None, so the caller falls back to the conventional name.
    let none = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="word/styles.xml"/>
</Relationships>"#;
    assert_eq!(main_document_part(none), None);
}

#[test]
fn keep_next_resolves_through_basedon() {
    // Both heading styles carry keepNext; Rubrik2 is basedOn Rubrik1. Resolution must yield
    // keepNext for BOTH (Word keeps every heading level with its body), not just the root.
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="Rubrik1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>
  <w:pPr><w:keepNext/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Rubrik2"><w:name w:val="heading 2"/><w:basedOn w:val="Rubrik1"/>
  <w:pPr><w:keepNext/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;
    let t = parse_styles(xml);
    assert_eq!(t.resolve(Some("Rubrik1")).keep_next, Some(true), "Rubrik1 keepNext");
    assert_eq!(t.resolve(Some("Rubrik2")).keep_next, Some(true), "Rubrik2 keepNext (derived)");
    assert_eq!(t.resolve(Some("Normal")).keep_next, None, "Normal has no keepNext");
}

#[test]
fn contextual_spacing_resolves_through_basedon() {
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="Liststycke"><w:name w:val="List Paragraph"/><w:basedOn w:val="Normal"/>
  <w:pPr><w:contextualSpacing/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;
    let t = parse_styles(xml);
    assert_eq!(t.resolve(Some("Liststycke")).contextual_spacing, Some(true), "List Paragraph opts in");
    assert_eq!(t.resolve(Some("Normal")).contextual_spacing, None, "Normal does not");
}

/// A manual page break (`<w:br w:type="page"/>`) and `w:pageBreakBefore` import onto the
/// paragraph's props and round-trip back to OOXML, so dense docs paginate like Word.
#[test]
fn page_breaks_import_and_round_trip() -> Result<()> {
    let doc = LoroDoc::new();
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>One</w:t></w:r><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:pPr><w:pageBreakBefore/></w:pPr><w:r><w:t>Two</w:t></w:r></w:p></w:body></w:document>"#;
    import_document_xml(&doc, xml)?;
    doc.commit();
    let paras = read_paragraphs(&doc)?;
    assert_eq!(paras.len(), 2);
    assert!(paras[0].props.page_break_after, "manual break recorded on the first paragraph");
    assert!(paras[1].props.page_break_before, "pageBreakBefore on the second paragraph");
    let out = export_document_xml(&doc, &PageGeometry::default(), &[], &[], false)?;
    assert!(out.contains("<w:br w:type=\"page\"/>"), "manual break round-trips: {out}");
    assert!(out.contains("<w:pageBreakBefore/>"), "pageBreakBefore round-trips: {out}");
    Ok(())
}

/// A manual column break (`<w:br w:type="column"/>`) imports onto the paragraph and round-trips as
/// a column break (NOT a page break - that mapping is a layout-time decision, see the wasm layer).
/// `parse_page_geometry` flags a document with `w:cols w:num >= 2` as multi-column.
#[test]
fn column_break_imports_and_round_trips() -> Result<()> {
    let doc = LoroDoc::new();
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>One</w:t></w:r><w:r><w:br w:type="column"/></w:r></w:p><w:p><w:r><w:t>Two</w:t></w:r></w:p></w:body></w:document>"#;
    import_document_xml(&doc, xml)?;
    doc.commit();
    let paras = read_paragraphs(&doc)?;
    assert!(paras[0].props.column_break_after, "column break recorded on the first paragraph");
    assert!(!paras[0].props.page_break_after, "kept distinct from a page break in the model");
    let out = export_document_xml(&doc, &PageGeometry::default(), &[], &[], false)?;
    assert!(out.contains("<w:br w:type=\"column\"/>"), "column break round-trips: {out}");

    // Multi-column detection: a single-column sectPr is not multi-column; w:num="2" is.
    let single = br#"<w:document xmlns:w="x"><w:body><w:sectPr><w:cols w:space="720"/></w:sectPr></w:body></w:document>"#;
    let multi = br#"<w:document xmlns:w="x"><w:body><w:sectPr><w:cols w:num="2" w:space="720"/></w:sectPr></w:body></w:document>"#;
    assert!(!parse_page_geometry(single).multi_column, "single column section");
    assert!(parse_page_geometry(multi).multi_column, "two-column section flagged");
    Ok(())
}

/// `parse_sections` returns one [`SectionCols`] per `w:sectPr` in document order (in-paragraph
/// sections + the body-final one), reading `w:num`/`w:space`/`w:equalWidth` + per-`w:col` widths,
/// and skipping a `w:sectPrChange`'s OLD nested `sectPr`.
#[test]
fn parse_sections_reads_per_section_columns() {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:sectPr><w:cols w:space="720"/></w:sectPr></w:pPr><w:r><w:t>S1</w:t></w:r></w:p>
<w:p><w:pPr><w:sectPr><w:cols w:num="2" w:space="709"/></w:sectPr></w:pPr><w:r><w:t>S2</w:t></w:r></w:p>
<w:sectPr><w:cols w:num="3" w:equalWidth="0"><w:col w:w="2000" w:space="100"/><w:col w:w="3000" w:space="120"/><w:col w:w="2500"/></w:cols></w:sectPr>
</w:body></w:document>"#;
    let secs = parse_sections(xml);
    assert_eq!(secs.len(), 3, "one SectionCols per sectPr in order");
    assert_eq!((secs[0].count, secs[0].space), (1, 720), "section 1 single-column");
    assert_eq!((secs[1].count, secs[1].space), (2, 709), "section 2 two equal columns");
    assert_eq!(secs[2].count, 3, "section 3 three columns");
    assert!(!secs[2].equal_width, "section 3 uneven widths");
    assert_eq!(secs[2].widths, vec![(2000, 100), (3000, 120), (2500, 0)], "per-column widths");

    // A tracked section change (w:sectPrChange) carries an OLD sectPr that must NOT be counted.
    let changed = br#"<w:document xmlns:w="x"><w:body><w:sectPr><w:cols w:num="2" w:space="708"/>
<w:sectPrChange w:id="1" w:author="A"><w:sectPr><w:cols w:num="9"/></w:sectPr></w:sectPrChange>
</w:sectPr></w:body></w:document>"#;
    let cs = parse_sections(changed);
    assert_eq!(cs.len(), 1, "only the real section, not the change's old one");
    assert_eq!(cs[0].count, 2, "the real 2-column geometry, not the old 9");
}

/// `parse_section_props` byte-slices each `<w:sectPr>` VERBATIM (attributes + children) in
/// document order - in-paragraph sections then the body-final one - and skips the OLD nested
/// sectPr inside a `w:sectPrChange` / `w:pPrChange`.
#[test]
fn parse_section_props_slices_each_sectpr_verbatim() {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pPrChange w:id="9" w:author="A"><w:pPr><w:sectPr><w:cols w:num="9"/></w:sectPr></w:pPr></w:pPrChange><w:sectPr w:rsidR="AA"><w:headerReference w:type="default" r:id="rId1"/><w:pgSz w:w="11906" w:h="16838"/></w:sectPr></w:pPr><w:r><w:t>S1</w:t></w:r></w:p>
<w:sectPr><w:sectPrChange w:id="1" w:author="A"><w:sectPr><w:cols w:num="7"/></w:sectPr></w:sectPrChange><w:footerReference w:type="default" r:id="rId2"/><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/></w:sectPr>
</w:body></w:document>"#;
    let secs = parse_section_props(xml);
    assert_eq!(secs.len(), 2, "the in-paragraph section + the body-final one; no nested-change ones");
    // Section 1: verbatim, keeps its rsid attribute + header ref, not the pPrChange's old sectPr.
    assert!(secs[0].starts_with("<w:sectPr w:rsidR=\"AA\">"), "verbatim attrs: {}", secs[0]);
    assert!(secs[0].contains("r:id=\"rId1\"") && !secs[0].contains("w:num=\"9\""), "own ref, not the old: {}", secs[0]);
    // Section 2 (final): landscape + its own footer ref. Its `w:sectPrChange` child (a tracked
    // section-property change) is preserved VERBATIM inside the slice - the skip guard prevents
    // that nested old sectPr from being counted as a SEPARATE section (secs.len() == 2, not 3),
    // it does not strip a legitimate child.
    assert!(secs[1].contains("w:orient=\"landscape\"") && secs[1].contains("r:id=\"rId2\""), "final verbatim: {}", secs[1]);
    assert!(secs[1].contains("<w:sectPrChange") && secs[1].contains("w:num=\"7\""), "the tracked change child is kept verbatim: {}", secs[1]);
}

/// `w:lineRule="exact"` imports as an `Exact` rule (line value = twips, not 240ths) and round-trips
/// with its rule preserved. `auto` (or absent) stays a multiplier (`line_rule` None).
#[test]
fn exact_line_rule_imports_and_round_trips() -> Result<()> {
    let doc = LoroDoc::new();
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:spacing w:line="240" w:lineRule="exact"/></w:pPr><w:r><w:t>Exact</w:t></w:r></w:p><w:p><w:pPr><w:spacing w:line="360" w:lineRule="auto"/></w:pPr><w:r><w:t>Auto</w:t></w:r></w:p></w:body></w:document>"#;
    import_document_xml(&doc, xml)?;
    doc.commit();
    let paras = read_paragraphs(&doc)?;
    assert_eq!(paras[0].props.line_rule, Some(LineRule::Exact), "exact rule imported");
    assert_eq!(paras[0].props.line_spacing, Some(240));
    assert_eq!(paras[1].props.line_rule, None, "auto stays the multiplier (no rule)");
    let out = export_document_xml(&doc, &PageGeometry::default(), &[], &[], false)?;
    assert!(out.contains("w:lineRule=\"exact\""), "exact rule round-trips: {out}");
    Ok(())
}

/// A text box's content (`<w:txbxContent>`) is a floating object, not body text - its paragraphs
/// must NOT enter the body flow (else chart/textbox labels paginate as body, e.g. FDO74774).
#[test]
fn textbox_content_is_excluded_from_body() -> Result<()> {
    let doc = LoroDoc::new();
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Body line</w:t></w:r></w:p><w:p><w:r><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>Inside the box</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p></w:body></w:document>"#;
    import_document_xml(&doc, xml)?;
    doc.commit();
    let texts: Vec<String> = read_paragraphs(&doc)?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .collect();
    assert!(texts.iter().any(|t| t.contains("Body line")), "body text kept: {texts:?}");
    assert!(!texts.iter().any(|t| t.contains("Inside the box")), "textbox text excluded: {texts:?}");
    Ok(())
}

/// A `nextPage` section break forces a page after the section's last paragraph; a `continuous`
/// one does not. The break type lives on the section AFTER the break (here the body-final sectPr),
/// and is applied to the previous section's last paragraph as a page-break-after.
#[test]
fn section_break_forces_a_page_unless_continuous() -> Result<()> {
    let make = |final_type: &str| -> Result<bool> {
        let doc = LoroDoc::new();
        let xml = format!(
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>\
<w:p><w:pPr><w:sectPr><w:type w:val=\"nextPage\"/></w:sectPr></w:pPr><w:r><w:t>S1</w:t></w:r></w:p>\
<w:p><w:r><w:t>S2</w:t></w:r></w:p>\
<w:sectPr><w:type w:val=\"{final_type}\"/></w:sectPr></w:body></w:document>"
        );
        import_document_xml(&doc, xml.as_bytes())?;
        doc.commit();
        Ok(read_paragraphs(&doc)?[0].props.page_break_after)
    };
    assert!(make("nextPage")?, "a nextPage section break paginates");
    assert!(!make("continuous")?, "a continuous section break stays on the page");
    Ok(())
}

/// An EMPTY paragraph carrying a continuous section break is flagged `continuous_break` (not
/// `section_end` / `page_break_after`), so the layout can consolidate it away the way Word does -
/// no line, no space-after (tdf169986 + the `*bottomSpacing` continuous-break fixtures).
#[test]
fn empty_continuous_break_carrier_is_flagged() -> Result<()> {
    let doc = LoroDoc::new();
    // Mirrors tdf169986: an empty carrier with a big `w:after`, then body text, then a body-final
    // sectPr whose section is `continuous` (the break after the carrier creates no page).
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:spacing w:after="2000"/><w:sectPr w:rsidR="00B0219B"></w:sectPr></w:pPr></w:p>
<w:p><w:r><w:t>Body</w:t></w:r></w:p>
<w:sectPr><w:type w:val="continuous"/></w:sectPr></w:body></w:document>"#;
    import_document_xml(&doc, xml)?;
    doc.commit();
    let p0 = &read_paragraphs(&doc)?[0].props;
    assert!(p0.continuous_break, "the empty carrier is flagged a continuous-break carrier");
    assert!(!p0.section_end, "a continuous break is NOT a page-creating section end");
    assert!(!p0.page_break_after, "a continuous break creates no page after the carrier");
    Ok(())
}

/// `settings_legacy_spacing`: legacy (summing) paragraph spacing is selected by
/// `w:doNotUseHTMLParagraphAutoSpacing` or a `compatibilityMode` of Word 2003 or older;
/// modern modes (12+) and an absent setting consolidate (max).
#[test]
fn settings_legacy_spacing_gates() {
    let wrap = |compat: &str| {
        format!(
            r#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:compat>{compat}</w:compat></w:settings>"#
        )
    };
    let mode = |v: u32| {
        format!(r#"<w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="{v}"/>"#)
    };
    assert!(
        settings_legacy_spacing(wrap("<w:doNotUseHTMLParagraphAutoSpacing/>").as_bytes()),
        "the doNotUseHTMLParagraphAutoSpacing flag selects legacy summing (tdf145716)"
    );
    assert!(
        !settings_legacy_spacing(
            wrap(r#"<w:doNotUseHTMLParagraphAutoSpacing w:val="false"/>"#).as_bytes()
        ),
        "an explicitly-false flag stays modern"
    );
    assert!(
        settings_legacy_spacing(wrap(&mode(11)).as_bytes()),
        "compatibilityMode 11 (Word 2003) is legacy (tdf153964)"
    );
    for m in [12, 14, 15] {
        assert!(
            !settings_legacy_spacing(wrap(&mode(m)).as_bytes()),
            "compatibilityMode {m} consolidates"
        );
    }
    assert!(!settings_legacy_spacing(wrap("").as_bytes()), "no setting at all is modern");
}

/// A self-closing `<w:p/>` is an empty paragraph - Word lays each out as a full blank line. It
/// arrives from quick-xml as `Event::Empty` (no Start/End pair), so the importer must append it
/// explicitly; otherwise blank lines vanish and pagination collapses (firstheadernofooter.docx
/// folded its two pages into one until this was fixed).
#[test]
fn self_closing_empty_paragraphs_are_imported() -> Result<()> {
    let doc = LoroDoc::new();
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>X</w:t></w:r></w:p><w:p/><w:p/><w:p/><w:p><w:r><w:t>Y</w:t></w:r></w:p></w:body></w:document>"#;
    let (stats, _) = import_document_xml(&doc, xml)?;
    doc.commit();
    assert_eq!(stats.paragraphs, 5, "two text + three empty paragraphs all counted");
    let paras = read_paragraphs(&doc)?;
    assert_eq!(paras.len(), 5, "all five paragraphs land in the flow");
    let text = |p: &Paragraph| p.runs.iter().map(|r| r.text.as_str()).collect::<String>();
    assert_eq!(text(&paras[0]), "X");
    assert!(
        paras[1].runs.is_empty() && paras[2].runs.is_empty() && paras[3].runs.is_empty(),
        "the middle three carry no runs"
    );
    assert_eq!(text(&paras[4]), "Y");
    Ok(())
}

/// A picture sharing a paragraph with a text box must anchor to the body paragraph, not be
/// pushed past it by the text box's own `<w:p>`s. `parse_images` counts only body paragraphs, so
/// the drawing after a 2-paragraph text box still reports `para_index = 0` (header2 of
/// 090716_*.docx crashed the whole import at index 2 of a 2-block header before this).
#[test]
fn drawing_after_a_text_box_anchors_to_the_body_paragraph() {
    let xml = br#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<w:p><w:pict><v:shape><v:textbox><w:txbxContent>
  <w:p><w:r><w:t>Banner line one</w:t></w:r></w:p>
  <w:p><w:r><w:t>Banner line two</w:t></w:r></w:p>
</w:txbxContent></v:textbox></v:shape></w:pict>
<w:drawing><wp:inline><wp:extent cx="100" cy="100"/><a:blip r:embed="rId9"/></wp:inline></w:drawing></w:p>
<w:p><w:r><w:t>Body</w:t></w:r></w:p></w:hdr>"#;
    let imgs = parse_images(xml);
    assert_eq!(imgs.len(), 1, "one real drawing (the VML text box is not a picture)");
    assert_eq!(imgs[0].para_index, 0, "anchored to the body paragraph, not past the text box");
    assert_eq!(imgs[0].embed, "rId9");
}

/// A legacy VML picture (`<w:pict>` -> `<v:shape style=...>` + `<v:imagedata r:id>`) is captured
/// like a `w:drawing`: size from the CSS `style`, media from the imagedata, `position:absolute`
/// => floating. Lets VML images render at all (tdf87569_vml / n751054 / fdo79915).
#[test]
fn vml_pict_image_is_captured() {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:v="urn:schemas-microsoft-com:vml">
<w:p><w:r><w:pict><v:shape id="s1" style="position:absolute;margin-left:198.75pt;margin-top:87pt;width:27.3pt;height:27.3pt;mso-position-horizontal-relative:page"><v:imagedata r:id="rId6" o:title="logo"/></v:shape></w:pict></w:r></w:p>
<w:p><w:r><w:t>body</w:t></w:r></w:p></w:document>"#;
    let imgs = parse_images(xml);
    assert_eq!(imgs.len(), 1, "the VML picture is captured");
    let im = &imgs[0];
    assert_eq!(im.embed, "rId6", "media ref from v:imagedata r:id");
    assert!(im.anchored, "position:absolute -> floating");
    assert_eq!(im.h_from, "page", "mso-position-horizontal-relative=page maps through");
    assert_eq!(im.w_emu, (27.3 * 12700.0) as i64, "width from the CSS style (pt -> EMU)");
    assert_eq!(im.para_index, 0, "anchored to its paragraph");
    // A non-picture VML shape (a text box: style but no imagedata) is NOT captured.
    let tb = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:v="urn:schemas-microsoft-com:vml">
<w:p><w:r><w:pict><v:shape style="width:100pt;height:50pt"><v:textbox><w:txbxContent><w:p><w:r><w:t>hi</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p></w:document>"#;
    assert!(parse_images(tb).is_empty(), "a styled text-box shape with no imagedata is not a picture");
}

/// [`parse_passthrough`] captures the non-picture drawing family - a chart `<w:drawing>` (no
/// `<a:blip>`), a WordprocessingShape text box, and a non-picture `<w:pict>` (VML line, no
/// `<v:imagedata>`) - each as its verbatim `<w:r>` span at the right paragraph, while leaving a
/// **real** picture run to the modeled image path (the `parse_images` oracle rejects it, so it is
/// not double-captured). See `docs/passthrough.md`.
#[test]
fn parse_passthrough_captures_non_picture_drawings() {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:v="urn:schemas-microsoft-com:vml"><w:body>
<w:p><w:r><w:drawing><wp:inline><wp:extent cx="5000" cy="5000"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId5"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
<w:p><w:r><w:drawing><wp:anchor><wp:extent cx="9000" cy="4000"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>inside the shape</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>
<w:p><w:r><w:pict><v:line from="0,0" to="100,100" strokecolor="black"/></w:pict></w:r></w:p>
<w:p><w:r><w:drawing><wp:inline><wp:extent cx="100" cy="100"/><a:graphic><a:graphicData><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
</w:body></w:document>"#;
    let raws = parse_passthrough(xml);
    assert_eq!(raws.len(), 3, "chart + shape + VML line captured; the real picture is not");
    assert!(
        raws.iter().all(|r| r.xml.starts_with("<w:r>") && r.xml.ends_with("</w:r>")),
        "every captured span is an exact <w:r>...</w:r> slice"
    );
    assert_eq!((raws[0].para_index, raws[1].para_index, raws[2].para_index), (0, 1, 2));
    assert!(raws[0].xml.contains("<c:chart"), "chart span: {}", raws[0].xml);
    assert!(raws[1].xml.contains("<wps:wsp>") && raws[1].xml.contains("inside the shape"), "shape span: {}", raws[1].xml);
    assert!(raws[2].xml.contains("<v:line"), "VML line span: {}", raws[2].xml);
    assert!(raws.iter().all(|r| !r.xml.contains("<a:blip")), "no captured span is a real picture");
    // The `txbxContent` inside the WordprocessingShape must not advance the anchor index - the VML
    // line stays at paragraph 2, the picture at 3 (proven by the para_index tuple above).
}

/// A UTF-8 BOM on the part (LibreOffice writes one on its XML parts) must not shift the
/// byte-slice captures: quick-xml skips the BOM without counting it in `buffer_position`,
/// which used to shift every captured span 3 bytes left - the closing `</w:r>` truncated to
/// `</w`, i.e. non-well-formed output Word rejects (dashed_line_custdash_* corpus docs).
#[test]
fn bom_prefixed_part_captures_exact_spans() {
    let mut xml = b"\xEF\xBB\xBF".to_vec();
    xml.extend_from_slice(br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:v="urn:schemas-microsoft-com:vml"><w:body>
<w:p><w:r><w:pict><v:line from="0,0" to="100,100" strokecolor="black"/></w:pict></w:r></w:p>
</w:body></w:document>"#);
    let raws = parse_passthrough(&xml);
    assert_eq!(raws.len(), 1, "the VML line is captured despite the BOM");
    assert!(
        raws[0].xml.starts_with("<w:r>") && raws[0].xml.ends_with("</w:r>"),
        "exact span, not shifted: {}",
        raws[0].xml
    );
}

/// A `<v:shape>` inside a `<v:group>` expresses its geometry in the group's `coordsize` units,
/// not CSS points: the group here is 10.15pt wide with coordsize 128905, so its child's
/// `width:128587` is ~10.12pt - reading it as points made a 45-metre image whose paint-time
/// resize overflowed usize on wasm32 (the NOBA footer icons). The child also inherits the
/// group's anchor.
#[test]
fn vml_group_scales_child_shape_geometry() {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:v="urn:schemas-microsoft-com:vml">
<w:p><w:r><w:pict><v:group style="position:absolute;margin-left:-1.3pt;margin-top:4.9pt;width:10.15pt;height:10.15pt;mso-position-horizontal-relative:page" coordsize="128905,128905">
<v:shape id="Image 4" style="position:absolute;width:128587;height:128587;visibility:visible"><v:imagedata r:id="rId12" o:title=""/></v:shape>
</v:group></w:pict></w:r></w:p>
<w:p><w:r><w:pict><v:shape style="position:absolute;width:27.3pt;height:27.3pt"><v:imagedata r:id="rId6"/></v:shape></w:pict></w:r></w:p></w:document>"#;
    let imgs = parse_images(xml);
    assert_eq!(imgs.len(), 2, "both the grouped and the plain VML picture are captured");
    let grouped = &imgs[0];
    // 128587 group units of a 10.15pt/128905-unit box = ~10.12pt = ~128,556 EMU.
    let expect = (128_587.0_f64 / 128_905.0 * 10.15 * 12_700.0) as i64;
    assert!(
        (grouped.w_emu - expect).abs() < 100,
        "group units scale to the group box (got {} EMU, expected ~{expect})",
        grouped.w_emu
    );
    assert_eq!(
        grouped.x_emu,
        (-1.3 * 12_700.0) as i64,
        "the child sits at the group's page position"
    );
    assert!(grouped.anchored, "the group's position:absolute anchors its children");
    assert_eq!(grouped.h_from, "page", "the group's mso-position-*-relative is inherited");
    // The group scope must not leak: the second, UNgrouped shape still reads CSS points.
    let plain = &imgs[1];
    assert_eq!(plain.w_emu, (27.3 * 12_700.0) as i64, "outside the group, pt lengths apply");
    assert_eq!(plain.para_index, 1);
}

/// Word emits a modern drawing as `<mc:AlternateContent>` carrying the SAME picture twice: a
/// DrawingML `<mc:Choice>` and a legacy VML `<mc:Fallback>`. Only the Choice may be ingested -
/// reading both rendered every such icon twice at subtly different anchors (the NOBA checklist
/// icons doubled across the section headings).
#[test]
fn alternate_content_fallback_is_not_ingested_twice() {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wpg"><w:drawing><wp:anchor behindDoc="1"><wp:extent cx="128905" cy="128905"/><a:blip r:embed="rId12"/></wp:anchor></w:drawing></mc:Choice><mc:Fallback><w:pict><v:group style="position:absolute;width:10.15pt;height:10.15pt" coordsize="128905,128905"><v:shape style="position:absolute;width:128587;height:128587"><v:imagedata r:id="rId12"/></v:shape></v:group></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p></w:document>"#;
    let imgs = parse_images(xml);
    assert_eq!(imgs.len(), 1, "one picture, not a Choice + Fallback pair");
    assert_eq!(imgs[0].embed, "rId12", "the DrawingML Choice is the one ingested");
}

/// An image run serializes to a `<w:drawing>` - `wp:inline` for an inline picture, `wp:anchor` with
/// position + wrap + `behindDoc` for a floating one - with the blip rel + extent + crop.
#[test]
fn image_run_exports_a_drawing() -> Result<()> {
    use loro::{ExpandType, StyleConfig, StyleConfigMap};
    let doc = LoroDoc::new();
    let mut styles = StyleConfigMap::new();
    styles.insert("img~1".into(), StyleConfig { expand: ExpandType::None });
    doc.config_text_style(styles);
    write_image(
        &doc,
        1,
        &ImagePlacement { media: "image1.png".into(), w_emu: 914400, h_emu: 685800, ..Default::default() },
    )?;
    append_paragraph(&doc, &[Run { image: Some(1), ..Run::plain(IMAGE_PLACEHOLDER.to_string()) }], None)?;
    doc.commit();

    let xml = export_document_xml(&doc, &PageGeometry::default(), &[], &[], false)?;
    assert!(xml.contains("<w:drawing>"), "{xml}");
    assert!(xml.contains("<wp:inline"), "inline picture");
    assert!(xml.contains("r:embed=\"rIdImg1\""), "blip rel");
    assert!(xml.contains("<wp:extent cx=\"914400\" cy=\"685800\"/>"), "extent");
    assert!(!xml.contains('\u{FFFC}'), "the placeholder char is not emitted as text");

    // Floating + crop variant.
    write_image(
        &doc,
        1,
        &ImagePlacement {
            media: "image1.png".into(),
            w_emu: 100,
            h_emu: 200,
            crop_l: 5000,
            floating: true,
            behind: true,
            wrap: "square".into(),
            ..Default::default()
        },
    )?;
    doc.commit();
    let xml = export_document_xml(&doc, &PageGeometry::default(), &[], &[], false)?;
    assert!(xml.contains("<wp:anchor"), "floating picture: {xml}");
    assert!(xml.contains("behindDoc=\"1\""), "behind");
    assert!(xml.contains("<wp:wrapSquare"), "square wrap");
    assert!(xml.contains("<a:srcRect l=\"5000\""), "crop");
    Ok(())
}

/// A header/footer image run serializes to a `<w:drawing>` from the supplied placements map (the
/// regression behind "header logo dropped on save"): `export_hdr_ftr_xml` used to pass an empty
/// images map, so image runs fell back to their placeholder text and the picture vanished.
#[test]
fn header_image_run_serializes_a_drawing() {
    let para = Paragraph {
        style: None,
        props: ParaProps::default(),
        runs: vec![Run { image: Some(7), ..Run::plain(IMAGE_PLACEHOLDER.to_string()) }],
        prop_change: None,
        mark_change: None,
    };
    let mut images = std::collections::HashMap::new();
    images.insert(
        7u64,
        ImagePlacement { media: "word/media/image1.png".into(), w_emu: 500, h_emu: 400, ..Default::default() },
    );
    let xml = export_hdr_ftr_xml(&[para], true, &images);
    assert!(xml.contains("<w:hdr"), "header tag: {xml}");
    assert!(xml.contains("<w:drawing>"), "image run emits a drawing: {xml}");
    assert!(xml.contains("r:embed=\"rIdImg7\""), "blip rel id: {xml}");
    assert!(xml.contains("<wp:extent cx=\"500\" cy=\"400\"/>"), "extent: {xml}");
    assert!(!xml.contains('\u{FFFC}'), "placeholder char not emitted as text: {xml}");
}

/// Empty annotation spans sized to `paras` (no in-cell comment / field / bookmark / hyperlink
/// markers) - the property-parity baseline, where the legacy `tbl_xml` and the grid codec must
/// agree byte-for-byte. Returns the owned span tables + the maps so the borrow outlives the call.
fn empty_spans(
    paras: &[Paragraph],
) -> (
    SpanGrid,
    OptSpanGrid,
    std::collections::HashMap<u64, String>,
) {
    let copens: Vec<Vec<Vec<u64>>> = paras.iter().map(|p| vec![Vec::new(); p.runs.len()]).collect();
    let fopens: Vec<Vec<Option<u64>>> = paras.iter().map(|p| vec![None; p.runs.len()]).collect();
    (copens, fopens, std::collections::HashMap::new())
}

/// T2.4: the loro grid codec (`export_table_grid` over the import projection) reproduces the legacy
/// in-memory `tbl_xml` serializer **byte-for-byte** for a property-bearing table - table + cell
/// borders, default + per-cell margins, cell shading, widths, gridSpan, vMerge, row height, style.
/// This is the equivalence guard that makes the eventual main-path flip (T2.7) safe: the grid is
/// proven to emit the same `<w:tbl>` the body+flat-flow path does.
#[test]
fn grid_codec_matches_legacy_tbl_xml_for_properties() -> Result<()> {
    let border = || Some(Border { size_eighths: 8, color: "000000".into() });
    let all_edges = EdgeBorders {
        top: border(),
        left: border(),
        bottom: border(),
        right: border(),
        inside_h: border(),
        inside_v: border(),
    };
    let one_edge = EdgeBorders { top: border(), ..Default::default() };
    let margins =
        CellMargins { top: Some(15), left: Some(108), bottom: Some(15), right: Some(108) };

    // Row 0: a vMerge-restart cell carrying every per-cell property, then a bare cell.
    // Row 1: a single gridSpan-2 vMerge-continue cell absorbing both columns.
    let rich = TableCell {
        para_count: 1,
        grid_span: 1,
        vmerge: VMerge::Restart,
        borders: one_edge.clone(),
        margins: Some(margins),
        width: Some(1440),
        shading: Some("FFFF00".into()),
        ..Default::default()
    };
    let bare = TableCell { para_count: 1, grid_span: 1, ..Default::default() };
    let spanned = TableCell {
        para_count: 1,
        grid_span: 2,
        vmerge: VMerge::Continue,
        ..Default::default()
    };
    let table = Table {
        col_widths: vec![2000, 3000],
        rows: vec![
            TableRow {
                cells: vec![rich, bare],
                height: Some(320),
                height_exact: true,
                ..Default::default()
            },
            TableRow { cells: vec![spanned], ..Default::default() },
        ],
        style: Some("TableGrid".into()),
        borders: all_edges,
        cell_margins: Some(margins),
        ..Default::default()
    };

    // Plain single-run cells: the run codec round-trips these exactly, so any diff is a property
    // diff (the thing under test), not a run-fidelity artifact. Row-major, cell by cell.
    let para = |t: &str| Paragraph {
        style: None,
        props: ParaProps::default(),
        runs: vec![Run::plain(t)],
        prop_change: None,
        mark_change: None,
    };
    let cell_paras = vec![para("A1"), para("B1"), para("wide")];

    // Legacy serializer with empty annotation spans.
    let (copens, fopens, maps) = empty_spans(&cell_paras);
    let img_maps: std::collections::HashMap<u64, ImagePlacement> = std::collections::HashMap::new();
    let ids = IdAlloc::new();
    let sp = ExportSpans {
        ids: &ids,
        copens: &copens,
        ccloses: &copens,
        fopens: &fopens,
        fcloses: &fopens,
        bopens: &copens,
        bcloses: &copens,
        fields: &maps,
        bookmarks: &maps,
        links: &maps,
        images: &img_maps,
        raw: &maps,
    };
    let mut cursor = 0usize;
    let legacy = tbl_xml(&table, &cell_paras, &mut cursor, &sp);

    // The grid codec over the import projection.
    let doc = LoroDoc::new();
    let g = crate::table_crdt::TableGrid::open(doc.get_map("t"))?;
    populate_grid_from_table(&g, &table, &cell_paras)?;
    doc.commit();
    let grid_xml = export_table_grid(&g)?;

    assert_eq!(grid_xml, legacy, "grid codec must match the legacy tbl_xml byte-for-byte");
    Ok(())
}

#[test]
fn parse_page_geometry_keeps_negative_margins() {
    // Word allows negative page margins (content bleeds past the physical margin, enlarging the
    // usable area). They must parse as-is, NOT fall back to the 1-inch default - the fallback
    // shrinks the page and over-paginates (tdf105490, tdf119952, tdf143384).
    let xml = br#"<w:document xmlns:w="x"><w:body><w:sectPr>
<w:pgSz w:w="12240" w:h="15840"/>
<w:pgMar w:top="-1440" w:right="864" w:bottom="-950" w:left="1944" w:header="360" w:footer="360" w:gutter="0"/>
</w:sectPr></w:body></w:document>"#;
    let g = parse_page_geometry(xml);
    assert_eq!(g.margin_top, -1440, "negative top margin parsed, not defaulted to 1440");
    assert_eq!(g.margin_bottom, -950, "negative bottom margin parsed");
    assert_eq!(g.margin_left, 1944);
    assert_eq!(g.margin_right, 864);
}

/// T2.5: the grid codec reproduces the legacy `tbl_xml` **byte-for-byte** for a table carrying
/// tracked structural + property revisions - a tblPrChange, a tracked-inserted row with a
/// trPrChange, and a tracked-deleted cell with a tcPrChange (each `old` snapshot non-trivial). This
/// proves the `Track` + `TablePropChange` containers round-trip exactly through the grid.
#[test]
fn grid_codec_matches_legacy_tbl_xml_for_tracked_revisions() -> Result<()> {
    let border = || Some(Border { size_eighths: 8, color: "000000".into() });
    let all_edges = EdgeBorders {
        top: border(),
        left: border(),
        bottom: border(),
        right: border(),
        inside_h: border(),
        inside_v: border(),
    };
    let one_edge = EdgeBorders { top: border(), ..Default::default() };
    let m = CellMargins { top: Some(15), left: Some(108), bottom: Some(15), right: Some(108) };
    let m2 = CellMargins { top: Some(0), left: Some(60), bottom: Some(0), right: Some(60) };
    let date = "2026-01-02T03:04:05Z";

    let bare = || TableCell { para_count: 1, grid_span: 1, ..Default::default() };
    // A tracked-deleted cell: only the change + the tcPrChange (old props) live on it.
    let deleted_cell = TableCell {
        para_count: 1,
        grid_span: 1,
        change: Some(Track { kind: TrackKind::Del, author: "Ann".into(), date: date.into(), id: 12 }),
        prop_change: Some(TablePropChange {
            author: "Ann".into(),
            date: date.into(),
            id: 13,
            old: TablePropSnapshot::Cell {
                width: Some(720),
                grid_span: 1,
                vmerge: VMerge::None,
                borders: one_edge.clone(),
                margins: Some(m),
                shading: Some("EEEEEE".into()),
            },
        }),
        ..Default::default()
    };

    let table = Table {
        col_widths: vec![2000, 3000],
        rows: vec![
            // A tracked-inserted row whose height was also tracked-changed (trPrChange: old height).
            TableRow {
                cells: vec![bare(), bare()],
                height: Some(320),
                height_exact: true,
                cant_split: false,
                justify: None,
                change: Some(Track { kind: TrackKind::Ins, author: "Ann".into(), date: date.into(), id: 11 }),
                prop_change: Some(TablePropChange {
                    author: "Ann".into(),
                    date: date.into(),
                    id: 14,
                    old: TablePropSnapshot::Row { height: Some(200), height_exact: false },
                }),
            },
            TableRow { cells: vec![deleted_cell, bare()], ..Default::default() },
        ],
        style: Some("TableGrid".into()),
        justify: None,
        borders: all_edges.clone(),
        cell_margins: Some(m),
        look: None,
        // A tracked table-property change (tblPrChange: old style + borders + cell margins).
        prop_change: Some(TablePropChange {
            author: "Ann".into(),
            date: date.into(),
            id: 10,
            old: TablePropSnapshot::Table {
                style: Some("PlainTable".into()),
                borders: all_edges,
                cell_margins: Some(m2),
            },
        }),
    };

    let para = |t: &str| Paragraph {
        style: None,
        props: ParaProps::default(),
        runs: vec![Run::plain(t)],
        prop_change: None,
        mark_change: None,
    };
    let cell_paras = vec![para("A1"), para("B1"), para("A2"), para("B2")];

    let (copens, fopens, maps) = empty_spans(&cell_paras);
    let img_maps: std::collections::HashMap<u64, ImagePlacement> = std::collections::HashMap::new();
    let ids = IdAlloc::new();
    let sp = ExportSpans {
        ids: &ids,
        copens: &copens,
        ccloses: &copens,
        fopens: &fopens,
        fcloses: &fopens,
        bopens: &copens,
        bcloses: &copens,
        fields: &maps,
        bookmarks: &maps,
        links: &maps,
        images: &img_maps,
        raw: &maps,
    };
    let mut cursor = 0usize;
    let legacy = tbl_xml(&table, &cell_paras, &mut cursor, &sp);

    let doc = LoroDoc::new();
    let g = crate::table_crdt::TableGrid::open(doc.get_map("t"))?;
    populate_grid_from_table(&g, &table, &cell_paras)?;
    doc.commit();
    let grid_xml = export_table_grid(&g)?;

    assert_eq!(grid_xml, legacy, "grid codec must match the legacy tbl_xml for tracked revisions");
    Ok(())
}

/// T2.7 step 1: the doc-level **node-walk** export (`export_document_xml_via_nodes`, reading table
/// nodes + grids) reproduces the legacy **body-walk** export (`export_document_xml` over
/// `Vec<BodyItem>` + flat-flow cell paragraphs) **byte-for-byte** for a document that interleaves a
/// property-bearing table between two top-level paragraphs. This is the doc-level equivalence guard
/// that de-risks flipping `to_document_xml` onto the containers.
#[test]
fn doc_level_node_walk_matches_legacy_body_walk() -> Result<()> {
    let page = PageGeometry::default();

    let border = || Some(Border { size_eighths: 8, color: "000000".into() });
    let all_edges = EdgeBorders {
        top: border(),
        left: border(),
        bottom: border(),
        right: border(),
        inside_h: border(),
        inside_v: border(),
    };
    let m = CellMargins { top: Some(15), left: Some(108), bottom: Some(15), right: Some(108) };
    let cell = || TableCell { para_count: 1, grid_span: 1, ..Default::default() };
    let table = Table {
        col_widths: vec![2000, 3000],
        rows: vec![
            TableRow { cells: vec![cell(), cell()], ..Default::default() },
            TableRow { cells: vec![cell(), cell()], ..Default::default() },
        ],
        style: Some("TableGrid".into()),
        borders: all_edges,
        cell_margins: Some(m),
        ..Default::default()
    };
    let para = |t: &str| Paragraph {
        style: None,
        props: ParaProps::default(),
        runs: vec![Run::plain(t)],
        prop_change: None,
        mark_change: None,
    };
    let cell_paras = vec![para("A1"), para("B1"), para("A2"), para("B2")];

    // Doc A - legacy: top paragraphs + cell paragraphs all as flat roots, structure in `body`.
    let a = LoroDoc::new();
    append_paragraph(&a, &[Run::plain("Intro")], None)?;
    for p in &cell_paras {
        append_paragraph(&a, &p.runs, p.style.as_deref())?;
    }
    append_paragraph(&a, &[Run::plain("Outro")], None)?;
    a.commit();
    let body = vec![BodyItem::Paragraph, BodyItem::Table(Box::new(table.clone())), BodyItem::Paragraph];
    let legacy = export_document_xml(&a, &page, &[], &body, false)?;

    // Doc B - main path: top paragraphs + a table NODE hosting the grid (via the proven projection).
    let b = LoroDoc::new();
    append_paragraph(&b, &[Run::plain("Intro")], None)?;
    let tn = create_table_node(&b)?;
    populate_grid_from_table(&open_table_grid(&b, tn)?, &table, &cell_paras)?;
    append_paragraph(&b, &[Run::plain("Outro")], None)?;
    b.commit();
    let nodes = export_document_xml_via_nodes(&b, &page, &[], false)?;

    assert_eq!(nodes, legacy, "node-walk export must match the legacy body-walk byte-for-byte");
    Ok(())
}

/// A picture inside a **table cell** re-emits its `<w:drawing>` on the live save path. Regression:
/// the tables-crdt flip serialized cell paragraphs with an empty image map, so a cell image run
/// emitted nothing at all (dropped) - which silently lost every picture in a table-heavy document.
#[test]
fn cell_image_round_trips_through_the_node_export() -> Result<()> {
    use loro::{ExpandType, StyleConfig, StyleConfigMap};
    let doc = LoroDoc::new();
    let mut styles = StyleConfigMap::new();
    styles.insert("img~7".into(), StyleConfig { expand: ExpandType::None });
    doc.config_text_style(styles);
    append_paragraph(&doc, &[Run::plain("Intro")], None)?;
    write_image(
        &doc,
        7,
        &ImagePlacement { media: "image1.png".into(), w_emu: 914400, h_emu: 685800, ..Default::default() },
    )?;

    let table = Table {
        col_widths: vec![2500],
        rows: vec![TableRow {
            cells: vec![TableCell { para_count: 1, grid_span: 1, ..Default::default() }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let cell_paras = vec![Paragraph {
        style: None,
        props: ParaProps::default(),
        runs: vec![Run { image: Some(7), ..Run::plain(IMAGE_PLACEHOLDER.to_string()) }],
        prop_change: None,
        mark_change: None,
    }];
    let tn = create_table_node(&doc)?;
    populate_grid_from_table(&open_table_grid(&doc, tn)?, &table, &cell_paras)?;
    doc.commit();

    let xml = export_document_xml_via_nodes(&doc, &PageGeometry::default(), &[], false)?;
    assert!(xml.contains("<w:tbl>"), "{xml}");
    assert!(xml.contains("<w:drawing>"), "the cell picture must round-trip: {xml}");
    assert!(xml.contains("r:embed=\"rIdImg7\""), "the cell image's blip rel");
    assert!(!xml.contains('\u{FFFC}'), "the placeholder char must not be emitted as text");
    Ok(())
}

/// A grid cell paragraph now carries FULL paragraph fidelity (style + alignment + spacing +
/// numbering + tracked pPrChange / mark), not just `{style, text}` - so the codec reproduces the
/// legacy `tbl_xml` cell `<w:pPr>` byte-for-byte. This closes the cell-paragraph-props gap that
/// blocked moving cell content into the grid (the swap's first prerequisite).
#[test]
fn grid_codec_preserves_cell_paragraph_props() -> Result<()> {
    let table = Table {
        col_widths: vec![2500],
        rows: vec![TableRow {
            cells: vec![TableCell { para_count: 1, grid_span: 1, ..Default::default() }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let cell_paras = vec![Paragraph {
        style: Some("Heading2".into()),
        props: ParaProps {
            align: Some(Align::Center),
            space_after: Some(120),
            line_spacing: Some(360),
            num_id: Some(2),
            num_ilvl: Some(1),
            ..Default::default()
        },
        runs: vec![Run::plain("Cell heading")],
        prop_change: None,
        mark_change: None,
    }];

    let (copens, fopens, maps) = empty_spans(&cell_paras);
    let img_maps: std::collections::HashMap<u64, ImagePlacement> = std::collections::HashMap::new();
    let ids = IdAlloc::new();
    let sp = ExportSpans {
        ids: &ids,
        copens: &copens,
        ccloses: &copens,
        fopens: &fopens,
        fcloses: &fopens,
        bopens: &copens,
        bcloses: &copens,
        fields: &maps,
        bookmarks: &maps,
        links: &maps,
        images: &img_maps,
        raw: &maps,
    };
    let mut cursor = 0usize;
    let legacy = tbl_xml(&table, &cell_paras, &mut cursor, &sp);

    let doc = LoroDoc::new();
    let g = crate::table_crdt::TableGrid::open(doc.get_map("t"))?;
    populate_grid_from_table(&g, &table, &cell_paras)?;
    doc.commit();
    let grid_xml = export_table_grid(&g)?;

    assert_eq!(grid_xml, legacy, "cell paragraph props must round-trip through the grid");
    // Sanity: the props actually serialized (so assert_eq isn't trivially true on both-dropped).
    assert!(grid_xml.contains("<w:pStyle w:val=\"Heading2\"/>"), "{grid_xml}");
    assert!(grid_xml.contains("<w:numPr>"));
    Ok(())
}

/// T2.7: Enter / Backspace inside a table cell. A cell paragraph is part of the flat index
/// (`block_seq`), so `split_paragraph` / `join_paragraph` operate on the cell's block list, and a
/// join across the cell boundary is refused.
#[test]
fn split_and_join_a_cell_paragraph() -> Result<()> {
    let doc = LoroDoc::new();
    append_paragraph(&doc, &[Run::plain("Intro")], None)?;
    let tn = create_table_node(&doc)?;
    {
        let g = open_table_grid(&doc, tn)?;
        g.push_row("r0")?;
        g.push_col("c0")?;
        g.set_cell("r0", "c0", "Hello world")?;
    }
    doc.commit();

    let texts = |d: &LoroDoc| -> Vec<String> {
        read_paragraphs(d)
            .unwrap()
            .iter()
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    };
    // Flat index: [0] = "Intro" (top-level), [1] = the cell paragraph.
    assert_eq!(texts(&doc), ["Intro", "Hello world"]);

    // Enter at "Hello| world" splits the cell paragraph in two (within the cell).
    split_paragraph(&doc, 1, 5)?;
    doc.commit();
    assert_eq!(texts(&doc), ["Intro", "Hello", " world"]);
    assert_eq!(open_table_grid(&doc, tn)?.cell_block_count("r0", "c0")?, 2);

    // Backspace at the start of " world" joins it back into "Hello".
    let caret = join_paragraph(&doc, 2)?;
    doc.commit();
    assert_eq!(caret, 5);
    assert_eq!(texts(&doc), ["Intro", "Hello world"]);
    assert_eq!(open_table_grid(&doc, tn)?.cell_block_count("r0", "c0")?, 1);

    // Joining the cell's first paragraph into the top-level "Intro" crosses a cell boundary - refused.
    assert!(join_paragraph(&doc, 1).is_err());
    Ok(())
}

/// A style-definition edit round-trips through the STYLE_OVERRIDES loro map as a per-field
/// override: two separate edits to different fields of one style MERGE (the second doesn't clobber
/// the first), and unset fields stay `None` (= inherit the base / basedOn chain).
#[test]
fn style_override_round_trips_per_field_through_the_loro_map() -> Result<()> {
    let doc = LoroDoc::new();
    // First edit: resize Heading1 to 32 half-points (16pt) and make it not-bold.
    write_style_override(
        &doc,
        "Heading1",
        &StyleProps { size: Some(32), bold: Some(false), ..StyleProps::default() },
    )?;
    doc.commit();
    // Second edit (a later session / different control): set the colour only.
    write_style_override(
        &doc,
        "Heading1",
        &StyleProps { color: Some("FF0000".into()), ..StyleProps::default() },
    )?;
    doc.commit();
    let over = read_style_overrides(&doc);
    let h1 = over.get("Heading1").expect("Heading1 override present");
    assert_eq!(h1.size, Some(32), "size from the first edit survives the second");
    assert_eq!(h1.bold, Some(false), "explicit not-bold recorded (Some(false) != None)");
    assert_eq!(h1.color.as_deref(), Some("FF0000"), "colour from the second edit merged in");
    assert_eq!(h1.italic, None, "untouched field stays None = inherit");
    assert_eq!(h1.font, None);
    Ok(())
}

/// `apply_overrides` folds the loro overrides onto a parsed base table: an edited field wins,
/// every other field still resolves through the `basedOn` chain over docDefaults.
#[test]
fn apply_overrides_wins_per_field_over_the_parsed_base() {
    let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="28"/></w:rPr></w:style>
</w:styles>"#;
    let mut t = parse_styles(xml);
    assert_eq!(t.resolve(Some("Heading1")).size, Some(28), "base Heading1 size before edit");
    let mut over = std::collections::HashMap::new();
    over.insert("Heading1".to_string(), StyleProps { size: Some(40), ..StyleProps::default() });
    t.apply_overrides(&over);
    let h1 = t.resolve(Some("Heading1"));
    assert_eq!(h1.size, Some(40), "edited size wins");
    assert_eq!(h1.bold, Some(true), "unedited bold still resolves from the base style");
    assert_eq!(h1.font.as_deref(), Some("Calibri"), "unedited font still inherits docDefaults");
}

/// The from-scratch export path serializes the EFFECTIVE table, so a style edit folded in via
/// `apply_overrides` appears in `styles.xml` (the imported-doc rewrite of an already-defined style
/// lands with the editing UI).
#[test]
fn export_styles_xml_emits_an_overridden_style() {
    let mut t = StyleTable::word_default();
    let before = export_styles_xml(&t);
    assert!(before.contains("w:styleId=\"Heading1\""), "Heading1 present in the scratch table");
    let mut over = std::collections::HashMap::new();
    over.insert("Heading1".to_string(), StyleProps { size: Some(48), ..StyleProps::default() });
    t.apply_overrides(&over);
    let after = export_styles_xml(&t);
    // The Heading1 block now carries the edited size (48 half-points = 24pt).
    let h1_at = after.find("w:styleId=\"Heading1\"").expect("Heading1 block");
    let h1_end = after[h1_at..].find("</w:style>").expect("Heading1 close") + h1_at;
    assert!(after[h1_at..h1_end].contains("w:sz w:val=\"48\""), "edited size in the Heading1 block");
}

/// An imported `styles.xml`'s already-defined style is patched in place on export (Modify-Style):
/// the edited fields change, every UNMODELED child (`w:next`, `w:link`, `w:outlineLvl`, `w:keepNext`)
/// stays, no element is duplicated, and the result parses back to the new values.
#[test]
fn merge_styles_into_xml_patches_an_edited_style_preserving_unmodeled_props() {
    let src = r#"<?xml version="1.0"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:link w:val="Heading1Char"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="0"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:rFonts w:asciiTheme="majorHAnsi"/><w:b/><w:color w:val="2F5496"/><w:sz w:val="32"/><w:szCs w:val="32"/></w:rPr></w:style>
</w:styles>"#;
    let table = parse_styles(src.as_bytes());
    let mut overrides = std::collections::HashMap::new();
    overrides.insert(
        "Heading1".to_string(),
        StyleProps { size: Some(48), color: Some("FF0000".into()), ..StyleProps::default() },
    );
    let out = merge_styles_into_xml(src, &table, &overrides);

    let at = out.find("w:styleId=\"Heading1\"").unwrap();
    let end = out[at..].find("</w:style>").unwrap() + at;
    let block = &out[at..end];
    // Edited fields took.
    assert!(block.contains("<w:sz w:val=\"48\"/>"), "size patched: {block}");
    assert!(block.contains("<w:szCs w:val=\"48\"/>"), "szCs patched");
    assert!(block.contains("<w:color w:val=\"FF0000\"/>"), "colour patched");
    // Unmodeled children preserved verbatim.
    assert!(block.contains("<w:next w:val=\"Normal\"/>"), "w:next preserved");
    assert!(block.contains("<w:link w:val=\"Heading1Char\"/>"), "w:link preserved");
    assert!(block.contains("<w:outlineLvl w:val=\"0\"/>"), "w:outlineLvl preserved");
    assert!(block.contains("<w:keepNext/>"), "w:keepNext preserved");
    assert!(block.contains("<w:b/>"), "unedited bold preserved");
    assert!(block.contains("<w:spacing w:before=\"240\" w:after=\"0\"/>"), "unedited spacing preserved");
    // No duplication of the patched elements.
    assert_eq!(block.matches("<w:sz ").count(), 1, "exactly one w:sz");
    assert_eq!(block.matches("<w:color ").count(), 1, "exactly one w:color");
    // Round-trips: parsing the patched XML resolves to the edited values.
    let t2 = parse_styles(out.as_bytes());
    let h1 = t2.resolve(Some("Heading1"));
    assert_eq!(h1.size, Some(48), "patched size parses back");
    assert_eq!(h1.color.as_deref(), Some("FF0000"), "patched colour parses back");
    assert_eq!(h1.bold, Some(true), "bold still resolves");
}

/// A partial spacing edit (only `after`) keeps the other spacing attributes (`before`, `line`) -
/// spacing is patched at the attribute level, not by replacing the whole element.
#[test]
fn merge_styles_into_xml_partial_spacing_keeps_other_attributes() {
    let src = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="Body"><w:name w:val="Body"/><w:pPr><w:spacing w:before="240" w:after="0" w:line="259" w:lineRule="auto"/></w:pPr></w:style>
</w:styles>"#;
    let table = parse_styles(src.as_bytes());
    let mut overrides = std::collections::HashMap::new();
    overrides.insert("Body".to_string(), StyleProps { space_after: Some(120), ..StyleProps::default() });
    let out = merge_styles_into_xml(src, &table, &overrides);
    assert!(out.contains("w:before=\"240\""), "before kept: {out}");
    assert!(out.contains("w:after=\"120\""), "after edited");
    assert!(out.contains("w:line=\"259\""), "line kept");
    let t2 = parse_styles(out.as_bytes());
    let b = t2.resolve(Some("Body"));
    assert_eq!((b.space_before, b.space_after, b.line_spacing), (Some(240), Some(120), Some(259)));
}

/// Editing a field of a style that has no `w:rPr` yet creates one (before `</w:style>`) without
/// disturbing the existing `w:pPr`.
#[test]
fn merge_styles_into_xml_creates_rpr_when_absent() {
    let src = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="Plain"><w:name w:val="Plain"/><w:pPr><w:spacing w:after="0"/></w:pPr></w:style>
</w:styles>"#;
    let table = parse_styles(src.as_bytes());
    let mut overrides = std::collections::HashMap::new();
    overrides.insert("Plain".to_string(), StyleProps { size: Some(28), ..StyleProps::default() });
    let out = merge_styles_into_xml(src, &table, &overrides);
    assert!(out.contains("<w:rPr><w:sz w:val=\"28\"/><w:szCs w:val=\"28\"/></w:rPr>"), "rPr created: {out}");
    assert!(out.contains("<w:pPr><w:spacing w:after=\"0\"/></w:pPr>"), "existing pPr intact");
    assert_eq!(parse_styles(out.as_bytes()).resolve(Some("Plain")).size, Some(28));
}

/// A runtime-added custom style (in the effective table's gallery, undefined by the source) is
/// appended to an imported `styles.xml` on export, with its name / basedOn / formatting, and parses
/// back.
#[test]
fn merge_styles_into_xml_appends_an_added_style() {
    let src = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;
    let mut table = parse_styles(src.as_bytes());
    let mut overrides = std::collections::HashMap::new();
    overrides.insert("MyQuote".to_string(), StyleProps { italic: Some(true), size: Some(28), ..StyleProps::default() });
    let mut added = std::collections::HashMap::new();
    added.insert("MyQuote".to_string(), AddedStyle { name: "My Quote".into(), based_on: Some("Normal".into()) });
    table.apply_overrides(&overrides);
    table.apply_added_styles(&added);

    let out = merge_styles_into_xml(src, &table, &overrides);
    let at = out.find("w:styleId=\"MyQuote\"").expect("added style appended");
    let end = out[at..].find("</w:style>").unwrap() + at;
    let block = &out[at..end];
    assert!(block.contains("<w:name w:val=\"My Quote\"/>"), "name: {block}");
    assert!(block.contains("<w:basedOn w:val=\"Normal\"/>"), "basedOn");
    assert!(block.contains("<w:i/>"), "italic");
    assert!(block.contains("<w:sz w:val=\"28\"/>"), "size");
    let t2 = parse_styles(out.as_bytes());
    let q = t2.resolve(Some("MyQuote"));
    assert_eq!((q.italic, q.size), (Some(true), Some(28)), "added style round-trips");
}

/// Style alignment (`w:jc`): parsed from the style's pPr, resolved through `basedOn`, exported in
/// the scratch path, and patched into an imported block - all round-tripping.
#[test]
fn style_alignment_parses_resolves_exports_and_patches() {
    // Parse + resolve (Title centred, a derived style inherits it).
    let src = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:pPr><w:jc w:val="center"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Sub"><w:name w:val="Sub"/><w:basedOn w:val="Title"/></w:style>
</w:styles>"#;
    let t = parse_styles(src.as_bytes());
    assert_eq!(t.resolve(Some("Title")).align, Some(Align::Center), "jc parsed + resolved");
    assert_eq!(t.resolve(Some("Sub")).align, Some(Align::Center), "alignment inherits via basedOn");

    // Scratch export emits jc, and round-trips.
    let mut scratch = StyleTable::word_default();
    let mut over = std::collections::HashMap::new();
    over.insert("Heading1".to_string(), StyleProps { align: Some(Align::Right), ..StyleProps::default() });
    scratch.apply_overrides(&over);
    let xml = export_styles_xml(&scratch);
    let at = xml.find("w:styleId=\"Heading1\"").unwrap();
    let end = xml[at..].find("</w:style>").unwrap() + at;
    assert!(xml[at..end].contains("<w:jc w:val=\"right\"/>"), "scratch export emits jc: {}", &xml[at..end]);
    assert_eq!(parse_styles(xml.as_bytes()).resolve(Some("Heading1")).align, Some(Align::Right));

    // Imported-doc patch: editing Title's alignment to justify patches the existing jc in place.
    let table = parse_styles(src.as_bytes());
    let mut ov = std::collections::HashMap::new();
    ov.insert("Title".to_string(), StyleProps { align: Some(Align::Justify), ..StyleProps::default() });
    let patched = merge_styles_into_xml(src, &table, &ov);
    let ta = patched.find("w:styleId=\"Title\"").unwrap();
    let te = patched[ta..].find("</w:style>").unwrap() + ta;
    assert!(patched[ta..te].contains("<w:jc w:val=\"both\"/>"), "patched jc -> both (justify): {}", &patched[ta..te]);
    assert_eq!(patched[ta..te].matches("<w:jc ").count(), 1, "no duplicate jc");
    assert_eq!(parse_styles(patched.as_bytes()).resolve(Some("Title")).align, Some(Align::Justify));
}

/// Style `w:pageBreakBefore` (tdf89377): parsed from a style's pPr, resolved through `basedOn`,
/// scratch-exported, and patched into an imported block - all round-tripping. A "page break before"
/// style is what makes a paragraph using it start a new page.
#[test]
fn style_page_break_before_parses_resolves_exports_and_patches() {
    let src = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="NewPageBreak"><w:name w:val="NewPageBreak"/><w:pPr><w:pageBreakBefore/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Derived"><w:name w:val="Derived"/><w:basedOn w:val="NewPageBreak"/></w:style>
<w:style w:type="paragraph" w:styleId="Plain"><w:name w:val="Plain"/></w:style>
</w:styles>"#;
    let t = parse_styles(src.as_bytes());
    assert_eq!(t.resolve(Some("NewPageBreak")).page_break_before, Some(true), "pageBreakBefore parsed + resolved");
    assert_eq!(t.resolve(Some("Derived")).page_break_before, Some(true), "inherits via basedOn");
    assert_eq!(t.resolve(Some("Plain")).page_break_before, None, "a plain style has none");

    // Scratch export emits pageBreakBefore, round-trips.
    let mut scratch = StyleTable::word_default();
    let mut over = std::collections::HashMap::new();
    over.insert("Heading1".to_string(), StyleProps { page_break_before: Some(true), ..StyleProps::default() });
    scratch.apply_overrides(&over);
    let xml = export_styles_xml(&scratch);
    let at = xml.find("w:styleId=\"Heading1\"").unwrap();
    let end = xml[at..].find("</w:style>").unwrap() + at;
    assert!(xml[at..end].contains("<w:pageBreakBefore/>"), "scratch export emits it: {}", &xml[at..end]);
    assert_eq!(parse_styles(xml.as_bytes()).resolve(Some("Heading1")).page_break_before, Some(true));

    // Imported-doc patch: adding pageBreakBefore to Plain patches its pPr (created), preserving order.
    let table = parse_styles(src.as_bytes());
    let mut ov = std::collections::HashMap::new();
    ov.insert("Plain".to_string(), StyleProps { page_break_before: Some(true), ..StyleProps::default() });
    let patched = merge_styles_into_xml(src, &table, &ov);
    let pa = patched.find("w:styleId=\"Plain\"").unwrap();
    let pe = patched[pa..].find("</w:style>").unwrap() + pa;
    assert!(patched[pa..pe].contains("<w:pageBreakBefore/>"), "patched into Plain: {}", &patched[pa..pe]);
    assert_eq!(parse_styles(patched.as_bytes()).resolve(Some("Plain")).page_break_before, Some(true));
}
