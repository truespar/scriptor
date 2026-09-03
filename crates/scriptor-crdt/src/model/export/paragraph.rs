//! Paragraphs and runs: `w:p`, `w:pPr`, `w:r`, `w:rPr`.
//! 
//! The innermost layer of the writer. A run emits whatever range wrappers open on it,
//! then its own properties and text, honouring the tracked-change element that
//! encloses it.

use super::*;

/// Rebuild a `<w:pBdr>` from the compact `edge=val,sz,space,color` list [`ParaProps::border`] stores
/// (`t|l|b|r` edge keys). Returns `None` when nothing renders, so the caller emits no empty element.
fn pbdr_xml(compact: &str) -> Option<String> {
    let mut edges = String::new();
    for tok in compact.split('|').filter(|t| !t.is_empty()) {
        let Some((edge, rest)) = tok.split_once('=') else { continue };
        let name = match edge {
            "t" => "w:top",
            "l" => "w:left",
            "b" => "w:bottom",
            "r" => "w:right",
            _ => continue,
        };
        let mut it = rest.split(',');
        let val = it.next().unwrap_or("single");
        let sz = it.next().unwrap_or("4");
        let space = it.next().unwrap_or("0");
        let color = it.next().unwrap_or("auto");
        edges.push_str(&format!(
            "<{name} w:val=\"{}\" w:sz=\"{}\" w:space=\"{}\" w:color=\"{}\"/>",
            xml_escape(val),
            xml_escape(sz),
            xml_escape(space),
            xml_escape(color),
        ));
    }
    if edges.is_empty() { None } else { Some(format!("<w:pBdr>{edges}</w:pBdr>")) }
}

/// Serialize the contents of a `w:pPr` (pStyle, numPr, spacing, ind, jc) for a style + props pair.
/// Shared by the current paragraph props and the old props inside a `w:pPrChange`.
fn ppr_body(style: Option<&str>, p: &ParaProps) -> String {
    let mut inner = String::new();
    if let Some(style) = style {
        inner.push_str(&format!("<w:pStyle w:val=\"{}\"/>", xml_escape(style)));
    }
    // The rest follows the `CT_PPr` schema SEQUENCE: keepNext, pageBreakBefore, framePr, numPr,
    // pBdr, shd, spacing, ind, contextualSpacing, jc. The order is load-bearing - e.g.
    // contextualSpacing emitted before numPr made `w:numPr` a schema violation on every list
    // paragraph that also suppressed same-style spacing (22 corpus hits before the reorder).
    match p.keep_next {
        Some(true) => inner.push_str("<w:keepNext/>"),
        Some(false) => inner.push_str("<w:keepNext w:val=\"false\"/>"),
        None => {}
    }
    if p.page_break_before {
        inner.push_str("<w:pageBreakBefore/>");
    }
    // A text frame's `w:framePr` (its attributes kept verbatim from import).
    if let Some(f) = &p.frame {
        inner.push_str(&format!("<w:framePr {f}/>"));
    }
    if let Some(num) = p.num_id {
        let ilvl = p.num_ilvl.unwrap_or(0);
        inner.push_str(&format!(
            "<w:numPr><w:ilvl w:val=\"{ilvl}\"/><w:numId w:val=\"{num}\"/></w:numPr>"
        ));
    }
    if let Some(b) = &p.border
        && let Some(xml) = pbdr_xml(b)
    {
        inner.push_str(&xml);
    }
    // Paragraph shading (`pPr/w:shd`) - parsed since the shading work but previously never
    // re-emitted, so a save dropped every paragraph fill.
    if let Some(s) = &p.shading {
        inner.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
            xml_escape(s)
        ));
    }
    if p.line_spacing.is_some() || p.space_before.is_some() || p.space_after.is_some() {
        inner.push_str("<w:spacing");
        if let Some(sb) = p.space_before {
            inner.push_str(&format!(" w:before=\"{sb}\""));
        }
        if let Some(sa) = p.space_after {
            inner.push_str(&format!(" w:after=\"{sa}\""));
        }
        if let Some(ls) = p.line_spacing {
            let rule = p.line_rule.map(|r| r.as_str()).unwrap_or("auto");
            inner.push_str(&format!(" w:line=\"{ls}\" w:lineRule=\"{rule}\""));
        }
        inner.push_str("/>");
    }
    if p.indent_left.is_some() || p.indent_right.is_some() || p.indent_first.is_some() {
        inner.push_str("<w:ind");
        if let Some(l) = p.indent_left {
            inner.push_str(&format!(" w:left=\"{l}\""));
        }
        if let Some(r) = p.indent_right {
            inner.push_str(&format!(" w:right=\"{r}\""));
        }
        match p.indent_first {
            Some(f) if f >= 0 => inner.push_str(&format!(" w:firstLine=\"{f}\"")),
            Some(f) => inner.push_str(&format!(" w:hanging=\"{}\"", -f)),
            None => {}
        }
        inner.push_str("/>");
    }
    match p.contextual_spacing {
        Some(true) => inner.push_str("<w:contextualSpacing/>"),
        Some(false) => inner.push_str("<w:contextualSpacing w:val=\"false\"/>"),
        None => {}
    }
    if let Some(a) = p.align {
        inner.push_str(&format!("<w:jc w:val=\"{}\"/>", a.to_ooxml()));
    }
    inner
}

/// Serialize a paragraph's properties block (`w:pPr`), adding a `w:pPrChange` (with the old style +
/// props) when the paragraph's properties were tracked-changed. Emits nothing when there is nothing.
pub(crate) fn ppr_xml(para: &Paragraph) -> String {
    let mut inner = ppr_body(para.style.as_deref(), &para.props);
    // The paragraph mark's own rPr (schema: after jc): its insertion/deletion revision, then the
    // mark's size (CT_ParaRPr order: ins/del wrappers precede the run-property children).
    let mut mark_rpr = String::new();
    if let Some(m) = &para.mark_change {
        mark_rpr.push_str(&format!(
            "<{el} w:id=\"{id}\" w:author=\"{author}\"{date}/>",
            el = m.kind.wrapper(),
            id = m.id,
            author = xml_escape(&m.author),
            date = date_attr(&m.date),
        ));
    }
    if let Some(sz) = para.props.mark_size {
        mark_rpr.push_str(&format!("<w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/>"));
    }
    if !mark_rpr.is_empty() {
        inner.push_str(&format!("<w:rPr>{mark_rpr}</w:rPr>"));
    }
    // An in-paragraph section break: the whole `<w:sectPr>` element re-emitted verbatim at its
    // `CT_PPr` slot (after the paragraph-mark rPr, before pPrChange). Preserves the section's page
    // geometry / columns / header-footer refs, instead of the old collapse into one synthesized
    // final sectPr that merged every section's hf refs (overflowing the schema).
    if let Some(sp) = &para.props.sect_pr {
        inner.push_str(sp);
    }
    if let Some(c) = &para.prop_change {
        inner.push_str(&format!(
            "<w:pPrChange w:id=\"{id}\" w:author=\"{author}\"{date}><w:pPr>{old}</w:pPr></w:pPrChange>",
            id = c.id,
            author = xml_escape(&c.author),
            date = date_attr(&c.date),
            old = ppr_body(c.old_style.as_deref(), &c.old),
        ));
    }
    if inner.is_empty() {
        String::new()
    } else {
        format!("<w:pPr>{inner}</w:pPr>")
    }
}

/// Serialize run properties in the `CT_RPr` schema SEQUENCE order: rFonts, b, i, strike, color,
/// sz, szCs, highlight, u, shd, vertAlign, lang. The order is load-bearing - `w:u` before
/// `w:color`/`w:sz`, or `w:shd` after `w:vertAlign`, is a schema violation the Open XML
/// validator flags on every formatted run (625 corpus hits before the reorder). `shading` is
/// passed separately because it lives on [`Run`], not [`RunProps`] (a tracked format-change's
/// old-props snapshot has no shading).
fn rpr_body(p: &RunProps, shading: Option<&str>) -> String {
    let mut rpr = String::new();
    if let Some(f) = &p.font {
        let f = xml_escape(f);
        rpr.push_str(&format!("<w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:cs=\"{f}\"/>"));
    }
    if p.bold {
        rpr.push_str("<w:b/>");
    }
    if p.italic {
        rpr.push_str("<w:i/>");
    }
    if p.strike {
        rpr.push_str("<w:strike/>");
    }
    if let Some(c) = &p.color {
        rpr.push_str(&format!("<w:color w:val=\"{}\"/>", xml_escape(c)));
    }
    if let Some(sz) = p.size {
        rpr.push_str(&format!("<w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/>"));
    }
    if let Some(h) = &p.highlight {
        rpr.push_str(&format!("<w:highlight w:val=\"{}\"/>", xml_escape(h)));
    }
    if p.underline {
        rpr.push_str("<w:u w:val=\"single\"/>");
    }
    if let Some(s) = shading {
        rpr.push_str(&format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>", xml_escape(s)));
    }
    if let Some(v) = &p.vert_align {
        rpr.push_str(&format!("<w:vertAlign w:val=\"{}\"/>", xml_escape(v)));
    }
    if let Some(l) = &p.lang {
        rpr.push_str(&format!("<w:lang w:val=\"{}\"/>", xml_escape(l)));
    }
    rpr
}

/// Serialize a single run, wrapping it in a `w:ins`/`w:del` revision element if tracked, and adding a
/// `w:rPrChange` inside its `w:rPr` if its formatting was tracked-changed. A run split around field
/// placeholders wraps each fragment independently (`w:fldSimple` may contain `w:ins`/`w:del`, never
/// the other way round), each fragment's wrapper id uniquified through `ids`.
pub(crate) fn run_xml(run: &Run, ids: &IdAlloc) -> String {
    run_xml_inner(run, run.track.as_ref().map(|t| (t, ids)))
}

/// [`run_xml`] without the run's own revision wrapper - for runs merged under a GROUP wrapper
/// emitted by `para_xml` (adjacent same-revision runs must share ONE `w:ins`/`w:del` element).
/// The `w:delText` choice still follows `run.track`, so a deletion's text serializes correctly
/// inside the shared wrapper.
pub(crate) fn run_xml_untracked(run: &Run) -> String {
    run_xml_inner(run, None)
}

fn run_xml_inner(run: &Run, wrap: Option<(&Track, &IdAlloc)>) -> String {
    let is_del = run.track.as_ref().is_some_and(|t| t.kind.is_del_text());

    // One wrapper open per emitted FRAGMENT, its id uniquified per emission (a placeholder run
    // splitting into several fldSimple segments would otherwise repeat the revision id).
    let wrap_open = || match wrap {
        Some((t, ids)) => format!(
            "<{tag} w:id=\"{id}\" w:author=\"{author}\"{date}>",
            tag = t.kind.wrapper(),
            id = ids.wrapper(t.id),
            author = xml_escape(&t.author),
            date = date_attr(&t.date),
        ),
        None => String::new(),
    };
    let wrap_close = || match wrap {
        Some((t, _)) => format!("</{}>", t.kind.wrapper()),
        None => String::new(),
    };

    // `w:rStyle` is the first child of `w:rPr` (schema order); kept verbatim from import so a run's
    // character style survives the round-trip.
    let mut rpr = match &run.char_style {
        Some(cs) => format!("<w:rStyle w:val=\"{}\"/>", xml_escape(cs)),
        None => String::new(),
    };
    // Run-level shading (`w:rPr/w:shd`, kept verbatim from import) rides inside `rpr_body` so it
    // lands at its schema position (after `w:u`, before `w:vertAlign`).
    rpr.push_str(&rpr_body(&RunProps::of(run), run.shading.as_deref()));
    // A tracked run-property change: the run keeps its new props; record the old props in rPrChange.
    if let Some(fc) = &run.fmt_change {
        rpr.push_str(&format!(
            "<w:rPrChange w:id=\"{id}\" w:author=\"{author}\"{date}><w:rPr>{old}</w:rPr></w:rPrChange>",
            id = fc.id,
            author = xml_escape(&fc.author),
            date = date_attr(&fc.date),
            old = rpr_body(&fc.old, None),
        ));
    }
    let rpr = if rpr.is_empty() { String::new() } else { format!("<w:rPr>{rpr}</w:rPr>") };

    // A tab run serializes back to <w:tab/>, not literal tab text.
    if run.text == "\t" {
        return format!("{}<w:r>{rpr}<w:tab/></w:r>{}", wrap_open(), wrap_close());
    }

    let (open, close) = if is_del {
        ("<w:delText xml:space=\"preserve\">", "</w:delText>")
    } else {
        ("<w:t xml:space=\"preserve\">", "</w:t>")
    };

    // PAGE / NUMPAGES: import collapsed each field to one placeholder char (FIELD_PAGE /
    // FIELD_NUMPAGES - the renderer substitutes the live value per page), so export must re-wrap
    // the char as real field markup: emitted as literal text it reaches the saved file as a
    // private-use tofu character and the field is gone. The placeholder can sit mid-run ("Page
    // \u{E000} of \u{E001}" imports as one run), so split around each occurrence. The cached
    // result is unknowable at export time; `w:dirty` tells Word to recompute it on open.
    if run.text.contains(FIELD_PAGE) || run.text.contains(FIELD_NUMPAGES) {
        let mut out = String::new();
        let mut seg = String::new();
        for ch in run.text.chars() {
            let instr = match ch {
                FIELD_PAGE => "PAGE",
                FIELD_NUMPAGES => "NUMPAGES",
                _ => {
                    seg.push(ch);
                    continue;
                }
            };
            if !seg.is_empty() {
                out.push_str(&format!(
                    "{}<w:r>{rpr}{open}{}{close}</w:r>{}",
                    wrap_open(),
                    xml_escape(&seg),
                    wrap_close()
                ));
                seg.clear();
            }
            out.push_str(&format!(
                "<w:fldSimple w:instr=\" {instr} \" w:dirty=\"true\">{}<w:r>{rpr}{open}1{close}</w:r>{}</w:fldSimple>",
                wrap_open(),
                wrap_close()
            ));
        }
        if !seg.is_empty() {
            out.push_str(&format!(
                "{}<w:r>{rpr}{open}{}{close}</w:r>{}",
                wrap_open(),
                xml_escape(&seg),
                wrap_close()
            ));
        }
        return out;
    }

    format!("{}<w:r>{rpr}{open}{}{close}</w:r>{}", wrap_open(), xml_escape(&run.text), wrap_close())
}
