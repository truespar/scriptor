//! The `w:drawing` element for an image run.

use super::*;

const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";

/// Serialize an image run as `<w:r><w:drawing>` - inline (`wp:inline`) or floating (`wp:anchor`) per
/// [`ImagePlacement::floating`] - from its placement. The blip references `rIdImg{id}` (the rel + the
/// `word/media` part are injected by `to_docx_bytes`). DrawingML namespaces are declared inline (the
/// document head only declares `w` + `r`).
pub(crate) fn drawing_xml(id: u64, p: &ImagePlacement) -> String {
    let (w, h) = (p.w_emu.max(1), p.h_emu.max(1));
    // DrawingML object ids (`wp:docPr`/`pic:cNvPr`) must be NONZERO - Word reports a document
    // whose first image carries id="0" as corrupted and refuses to open it (tdf142700). Model
    // image ids are 0-based, so offset by one; rel ids (`rIdImg{id}`) keep the raw model id.
    let oid = id + 1;
    let srcrect = if p.crop_l != 0 || p.crop_t != 0 || p.crop_r != 0 || p.crop_b != 0 {
        format!(
            "<a:srcRect l=\"{}\" t=\"{}\" r=\"{}\" b=\"{}\"/>",
            p.crop_l, p.crop_t, p.crop_r, p.crop_b
        )
    } else {
        String::new()
    };
    let graphic = format!(
        "<a:graphic xmlns:a=\"{A_NS}\"><a:graphicData uri=\"{PIC_NS}\">\
<pic:pic xmlns:pic=\"{PIC_NS}\">\
<pic:nvPicPr><pic:cNvPr id=\"{oid}\" name=\"Picture {oid}\"/><pic:cNvPicPr/></pic:nvPicPr>\
<pic:blipFill><a:blip r:embed=\"rIdImg{id}\"/>{srcrect}<a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
</pic:pic></a:graphicData></a:graphic>"
    );
    let inner = if p.floating {
        let behind = i32::from(p.behind);
        let posh = if p.h_align.is_empty() {
            format!("<wp:posOffset>{}</wp:posOffset>", p.x_emu)
        } else {
            format!("<wp:align>{}</wp:align>", xml_escape(&p.h_align))
        };
        let posv = if p.v_align.is_empty() {
            format!("<wp:posOffset>{}</wp:posOffset>", p.y_emu)
        } else {
            format!("<wp:align>{}</wp:align>", xml_escape(&p.v_align))
        };
        let wrap = match p.wrap.as_str() {
            "square" => "<wp:wrapSquare wrapText=\"bothSides\"/>".to_string(),
            "topAndBottom" => "<wp:wrapTopAndBottom/>".to_string(),
            "tight" | "through" => format!("<wp:wrap{} wrapText=\"bothSides\"><wp:wrapPolygon edited=\"0\"><wp:start x=\"0\" y=\"0\"/><wp:lineTo x=\"0\" y=\"21600\"/><wp:lineTo x=\"21600\" y=\"21600\"/><wp:lineTo x=\"21600\" y=\"0\"/><wp:lineTo x=\"0\" y=\"0\"/></wp:wrapPolygon></wp:wrap{}>",
                if p.wrap == "through" { "Through" } else { "Tight" },
                if p.wrap == "through" { "Through" } else { "Tight" }),
            _ => "<wp:wrapNone/>".to_string(),
        };
        let hfrom = if p.h_from.is_empty() { "column" } else { p.h_from.as_str() };
        let vfrom = if p.v_from.is_empty() { "paragraph" } else { p.v_from.as_str() };
        format!(
            "<wp:anchor xmlns:wp=\"{WP_NS}\" distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" simplePos=\"0\" \
relativeHeight=\"1\" behindDoc=\"{behind}\" locked=\"0\" layoutInCell=\"1\" allowOverlap=\"1\">\
<wp:simplePos x=\"0\" y=\"0\"/>\
<wp:positionH relativeFrom=\"{hf}\">{posh}</wp:positionH>\
<wp:positionV relativeFrom=\"{vf}\">{posv}</wp:positionV>\
<wp:extent cx=\"{w}\" cy=\"{h}\"/>{wrap}<wp:docPr id=\"{oid}\" name=\"Picture {oid}\"/>{graphic}</wp:anchor>",
            hf = xml_escape(hfrom),
            vf = xml_escape(vfrom),
        )
    } else {
        format!(
            "<wp:inline xmlns:wp=\"{WP_NS}\" distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
<wp:extent cx=\"{w}\" cy=\"{h}\"/><wp:docPr id=\"{oid}\" name=\"Picture {oid}\"/>{graphic}</wp:inline>"
        )
    };
    format!("<w:r><w:drawing>{inner}</w:drawing></w:r>")
}
