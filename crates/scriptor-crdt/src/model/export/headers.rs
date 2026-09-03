//! Header and footer parts, and the section properties that close a body.

use super::*;

/// Serialize a `<w:hdr>` / `<w:ftr>` part body from header/footer paragraphs.
pub fn export_hdr_ftr_xml(
    paras: &[Paragraph],
    is_header: bool,
    images: &std::collections::HashMap<u64, ImagePlacement>,
) -> String {
    let tag = if is_header { "w:hdr" } else { "w:ftr" };
    let (copens, ccloses) = comment_spans(paras);
    // Headers/footers carry no generic fields / bookmarks / hyperlinks in v1 (PAGE/NUMPAGES use the
    // placeholder path), so those span tables + maps are empty. Pictures DO round-trip: `images` maps
    // each `Run.image` id to its placement so an image run emits its `<w:drawing>` (the caller wires
    // the `rIdImg{id}` rel into this part's own `.rels`).
    let empty_f: Vec<Vec<Option<u64>>> = paras.iter().map(|p| vec![None; p.runs.len()]).collect();
    let empty_m: Vec<Vec<Vec<u64>>> = paras.iter().map(|p| vec![Vec::new(); p.runs.len()]).collect();
    let maps = std::collections::HashMap::new();
    let ids = IdAlloc::new();
    let sp = ExportSpans {
        ids: &ids,
        copens: &copens,
        ccloses: &ccloses,
        fopens: &empty_f,
        fcloses: &empty_f,
        bopens: &empty_m,
        bcloses: &empty_m,
        fields: &maps,
        bookmarks: &maps,
        links: &maps,
        images,
        raw: &maps, // header/footer passthrough is body-only in v1 (empty)
    };
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n");
    // The full Word namespace set, not just `w`: an image run emits `r:embed`, and captured
    // passthrough spans (once headers carry them) reference the drawing prefixes.
    out.push_str(&format!("<{tag} {WORD_NS_ATTRS}>"));
    // A header/footer must contain at least one paragraph.
    if paras.is_empty() {
        out.push_str("<w:p/>");
    }
    for (i, para) in paras.iter().enumerate() {
        out.push_str(&para_xml(para, i, &sp));
    }
    out.push_str(&format!("</{tag}>"));
    out
}

/// `CT_SectPr`'s child order (ECMA-376). Used to slot a modeled child back in when the imported
/// `sectPr` did not have one - Word rejects a section whose children are out of schema order.
const SECTPR_ORDER: &[&str] = &[
    "w:headerReference",
    "w:footerReference",
    "w:footnotePr",
    "w:endnotePr",
    "w:type",
    "w:pgSz",
    "w:pgMar",
    "w:paperSrc",
    "w:pgBorders",
    "w:lnNumType",
    "w:pgNumType",
    "w:cols",
    "w:formProt",
    "w:vAlign",
    "w:noEndnote",
    "w:titlePg",
    "w:textDirection",
    "w:bidi",
    "w:rtlGutter",
    "w:docGrid",
    "w:printerSettings",
    "w:sectPrChange",
];

fn sectpr_rank(name: &str) -> Option<usize> {
    SECTPR_ORDER.iter().position(|n| *n == name)
}

/// Merge the model's section properties into the `sectPr` as imported, keeping every child the model
/// does not represent.
///
/// [`sectpr_xml`] synthesizes a `sectPr` from scratch, which is correct for a document built here
/// but silently discarded everything else the original carried - `w:cols` (newspaper columns),
/// `w:type` (whether the section starts a new page), `w:pgNumType`, `w:docGrid`, `w:bidi`,
/// `w:formProt` and the rest. On the corpus that was 576 documents losing `w:cols` alone, 18 of them
/// a genuine multi-column layout, on a save that changed nothing.
///
/// So: walk the imported children in order, substitute the four the model owns, and pass everything
/// else through untouched. Order is preserved because the import was schema-valid; a modeled child
/// the original lacked is slotted in by [`SECTPR_ORDER`] rather than appended.
pub(crate) fn merge_sectpr_xml(
    raw: &str,
    p: &PageGeometry,
    hf: &[HfRef],
    title_pg: bool,
) -> String {
    let Some(children) = sectpr_children(raw) else {
        return sectpr_xml(p, hf, title_pg);
    };

    // Header/footer references. The model manages `default` and `first`, so those are rebuilt from
    // `hf` (the editing API adds and removes them). Any other kind - in practice `even`, which
    // belongs to `w:evenAndOddHeaders` and is not modeled - is kept exactly as imported. Dropping it
    // left its header part in the package with nothing pointing at it, the same orphaning as a
    // footnote whose reference disappears.
    let managed = |kind: &str| kind == "default" || kind == "first";
    let mut refs: String = hf
        .iter()
        .map(|r| {
            let el = if r.is_header { "headerReference" } else { "footerReference" };
            format!(
                "<w:{el} w:type=\"{}\" r:id=\"{}\"/>",
                xml_escape(&r.kind),
                xml_escape(&r.r_id)
            )
        })
        .collect();
    for (name, xml) in &children {
        if (name == "w:headerReference" || name == "w:footerReference")
            && !ref_kind(xml).is_some_and(|k| managed(&k))
        {
            refs.push_str(xml);
        }
    }

    let mut out: Vec<(String, String)> = Vec::with_capacity(children.len() + 4);
    let (mut saw_pgsz, mut saw_pgmar, mut saw_title) = (false, false, false);
    for (name, xml) in children {
        match name.as_str() {
            // Emitted from the model, below / above - never carried over verbatim.
            "w:headerReference" | "w:footerReference" => {}
            "w:pgSz" => {
                saw_pgsz = true;
                out.push((name, pgsz_xml(p)));
            }
            "w:pgMar" => {
                saw_pgmar = true;
                out.push((name, pgmar_xml(p)));
            }
            "w:titlePg" => {
                saw_title = true;
                if title_pg {
                    out.push((name, "<w:titlePg/>".to_string()));
                }
            }
            _ => out.push((name, xml)),
        }
    }

    // Anything the model owns that the original did not carry, slotted in by schema rank.
    let insert = |name: &str, xml: String, out: &mut Vec<(String, String)>| {
        let rank = sectpr_rank(name).unwrap_or(usize::MAX);
        let at = out
            .iter()
            .position(|(n, _)| sectpr_rank(n).is_some_and(|r| r > rank))
            .unwrap_or(out.len());
        out.insert(at, (name.to_string(), xml));
    };
    if !saw_pgsz {
        insert("w:pgSz", pgsz_xml(p), &mut out);
    }
    if !saw_pgmar {
        insert("w:pgMar", pgmar_xml(p), &mut out);
    }
    if title_pg && !saw_title {
        insert("w:titlePg", "<w:titlePg/>".to_string(), &mut out);
    }

    let body: String = out.into_iter().map(|(_, x)| x).collect();
    format!("<w:sectPr>{refs}{body}</w:sectPr>")
}

/// Split a `<w:sectPr>` into `(element name, verbatim XML)` children, in document order.
///
/// Returns `None` when the input is not a parseable `sectPr`, so the caller falls back to
/// synthesizing rather than emitting something malformed.
fn sectpr_children(raw: &str) -> Option<Vec<(String, String)>> {
    use quick_xml::events::Event;
    let bytes = raw.as_bytes();
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut buf = Vec::new();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut depth = 0usize;
    let mut open: Option<(String, usize)> = None; // (name, byte offset of its `<`)
    let mut seen_sectpr = false;
    loop {
        let before = reader.buffer_position() as usize;
        let ev = reader.read_event_into(&mut buf).ok()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "w:sectPr" && depth == 0 {
                    seen_sectpr = true;
                    depth = 1;
                } else if depth == 1 {
                    open = Some((name, before));
                    depth = 2;
                } else if depth >= 2 {
                    depth += 1;
                }
            }
            Event::Empty(e) if depth == 1 => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let end = reader.buffer_position() as usize;
                out.push((name, String::from_utf8_lossy(&bytes[before..end]).into_owned()));
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "w:sectPr" && depth == 1 {
                    depth = 0;
                } else if depth == 2 {
                    if let Some((n, start)) = open.take() {
                        let end = reader.buffer_position() as usize;
                        out.push((n, String::from_utf8_lossy(&bytes[start..end]).into_owned()));
                    }
                    depth = 1;
                } else if depth > 2 {
                    depth -= 1;
                }
            }
            _ => {}
        }
        buf.clear();
    }
    if seen_sectpr { Some(out) } else { None }
}

/// The `w:type` of a `<w:headerReference>` / `<w:footerReference>` (`default`, `first`, `even`).
fn ref_kind(xml: &str) -> Option<String> {
    let at = xml.find("w:type=\"")? + "w:type=\"".len();
    let rest = &xml[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn pgsz_xml(p: &PageGeometry) -> String {
    let orient = if p.width > p.height { " w:orient=\"landscape\"" } else { "" };
    format!("<w:pgSz w:w=\"{}\" w:h=\"{}\"{orient}/>", p.width, p.height)
}

fn pgmar_xml(p: &PageGeometry) -> String {
    format!(
        "<w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\" \
w:header=\"{}\" w:footer=\"{}\" w:gutter=\"0\"/>",
        p.margin_top,
        p.margin_right,
        p.margin_bottom,
        p.margin_left,
        p.header_dist,
        p.footer_dist,
    )
}

/// Serialize the section properties (`w:sectPr`): header/footer references, page size (+
/// orientation), margins. References come first (OOXML schema order).
///
/// Used only when there is no imported `sectPr` to merge into - a document created from scratch.
/// Everything opened from a file goes through [`merge_sectpr_xml`], which keeps the children this
/// does not know about.
pub(crate) fn sectpr_xml(p: &PageGeometry, hf: &[HfRef], title_pg: bool) -> String {
    let orient = if p.width > p.height { " w:orient=\"landscape\"" } else { "" };
    let mut refs = String::new();
    for r in hf {
        let el = if r.is_header { "headerReference" } else { "footerReference" };
        refs.push_str(&format!("<w:{el} w:type=\"{}\" r:id=\"{}\"/>", xml_escape(&r.kind), xml_escape(&r.r_id)));
    }
    // `<w:titlePg/>` (Different First Page) goes after pgMar per the CT_SectPr schema order.
    let title = if title_pg { "<w:titlePg/>" } else { "" };
    format!(
        "<w:sectPr>{refs}<w:pgSz w:w=\"{w}\" w:h=\"{h}\"{orient}/>\
<w:pgMar w:top=\"{t}\" w:right=\"{r}\" w:bottom=\"{b}\" w:left=\"{l}\" \
w:header=\"{hd}\" w:footer=\"{fd}\" w:gutter=\"0\"/>{title}</w:sectPr>",
        w = p.width,
        h = p.height,
        t = p.margin_top,
        r = p.margin_right,
        b = p.margin_bottom,
        l = p.margin_left,
        hd = p.header_dist,
        fd = p.footer_dist,
    )
}
