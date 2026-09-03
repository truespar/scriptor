//! Canonical, lossless OOXML model (the core).
//!
//! A `.docx` is an OPC zip of XML parts. This crate reads those parts, lets you edit with
//! tracked changes, and writes them back. The load-bearing invariant is **lossless round-trip**:
//! anything we do not deliberately change is preserved verbatim, byte for byte.
//!
//! The "passthrough" surface implemented here:
//! - [`inspect`]   - read-only probe: list parts, count revisions + comments.
//! - [`roundtrip`] - read a `.docx` and write it back, asserting every part is byte-stable.
//! - [`redline`]   - inject one author-attributed tracked insertion (`w:ins`), touching only
//!   `word/document.xml` and preserving every other part verbatim.
//!
//! The richer *typed* OOXML model (modeling each revision element rather than treating
//! `document.xml` as bytes) builds on this; the passthrough approach already delivers the
//! lossless round-trip + author-attributed agent redline. The final "opens perfectly in Word"
//! check is a human gate.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};

/// Counts of Word revision (track-changes) elements found in `word/document.xml`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RevisionCounts {
    pub ins: usize,
    pub del: usize,
    pub r_pr_change: usize,
    pub p_pr_change: usize,
    pub move_from: usize,
    pub move_to: usize,
}

impl RevisionCounts {
    /// Total tracked-change elements across all kinds.
    pub fn total(&self) -> usize {
        self.ins + self.del + self.r_pr_change + self.p_pr_change + self.move_from + self.move_to
    }
}

/// Metadata for one part of the OPC package (decompressed size).
#[derive(Debug, Clone)]
pub struct PartInfo {
    pub name: String,
    pub size: u64,
}

/// One part (file) of the OPC package, with its decompressed bytes.
#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub data: Vec<u8>,
}

/// Read-only summary of a `.docx` package.
#[derive(Debug)]
pub struct DocxSummary {
    pub parts: Vec<PartInfo>,
    pub revisions: RevisionCounts,
    pub comment_count: usize,
}

/// Result of a [`roundtrip`].
#[derive(Debug)]
pub struct RoundtripReport {
    pub parts: usize,
    pub stable: bool,
    /// Name of the first part whose bytes changed (if any).
    pub first_diff: Option<String>,
}

/// Result of a [`redline`].
#[derive(Debug)]
pub struct RedlineReport {
    pub injected_id: u64,
    pub author: String,
    pub parts: usize,
}

/// Read every part of a `.docx` from disk, in archive order, with decompressed bytes.
pub fn read_parts(path: &Path) -> Result<Vec<Part>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    read_parts_from(file)
}

/// Read every part of a `.docx` held in memory (the browser / wasm path - no filesystem).
pub fn read_parts_bytes(bytes: &[u8]) -> Result<Vec<Part>> {
    read_parts_from(std::io::Cursor::new(bytes))
}

/// Largest single decompressed part we will read. Comfortably above any real document (the biggest
/// thing in a `.docx` is a media blob), small enough that one hostile part can't exhaust memory.
const MAX_PART_BYTES: u64 = 256 * 1024 * 1024;
/// Largest total decompressed package. Bounds a zip bomb: many parts each under the per-part cap.
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// Ceiling on how much we PRE-allocate from a part's declared size. The declared size lives in the
/// zip's own header, so it is attacker-controlled: a 30-byte file can claim 4 GiB. Reading grows the
/// buffer as real bytes arrive, so a modest hint costs nothing and a hostile one cannot allocate.
const MAX_PREALLOC: u64 = 8 * 1024 * 1024;

/// Shared OPC unzip loop over any seekable reader (file on native, cursor on wasm).
fn read_parts_from<R: Read + std::io::Seek>(reader: R) -> Result<Vec<Part>> {
    let mut zip = zip::ZipArchive::new(reader)
        .context("reading the file as a .docx (OPC zip package)")?;

    let mut parts = Vec::with_capacity(zip.len());
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        // Refuse an over-large part on its DECLARED size first (cheap), then enforce the same limit
        // against bytes actually read, since the declaration is not trustworthy in either direction.
        let declared = f.size();
        if declared > MAX_PART_BYTES {
            anyhow::bail!(
                "part '{}' declares {declared} bytes, over the {MAX_PART_BYTES}-byte limit",
                f.name()
            );
        }
        let mut data = Vec::with_capacity(declared.min(MAX_PREALLOC) as usize);
        std::io::copy(&mut (&mut f).take(MAX_PART_BYTES + 1), &mut data)?;
        if data.len() as u64 > MAX_PART_BYTES {
            anyhow::bail!("part '{}' exceeds the {MAX_PART_BYTES}-byte limit", f.name());
        }
        total += data.len() as u64;
        if total > MAX_TOTAL_BYTES {
            anyhow::bail!("package decompresses to over {MAX_TOTAL_BYTES} bytes");
        }
        parts.push(Part { name: f.name().to_string(), data });
    }
    Ok(parts)
}

/// Write parts to a new `.docx` (OPC zip) on disk, in the given order.
pub fn write_parts(path: &Path, parts: &[Part]) -> Result<()> {
    let file =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    write_parts_to(file, parts)
}

/// Serialize parts to an in-memory `.docx` (OPC zip) - the browser / wasm save path.
pub fn write_parts_bytes(parts: &[Part]) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    write_parts_to(&mut buf, parts)?;
    Ok(buf.into_inner())
}

/// Shared OPC zip writer over any seekable writer (file on native, cursor on wasm).
fn write_parts_to<W: Write + std::io::Seek>(writer: W, parts: &[Part]) -> Result<()> {
    let mut zip = zip::ZipWriter::new(writer);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for p in parts {
        zip.start_file(p.name.clone(), options)
            .with_context(|| format!("writing part {}", p.name))?;
        zip.write_all(&p.data)?;
    }
    zip.finish().context("finalizing the zip")?;
    Ok(())
}

/// Probe a `.docx`: list its parts and count tracked-change elements and comments.
pub fn inspect(path: &Path) -> Result<DocxSummary> {
    let parts = read_parts(path)?;

    let mut part_infos: Vec<PartInfo> = parts
        .iter()
        .map(|p| PartInfo { name: p.name.clone(), size: p.data.len() as u64 })
        .collect();
    part_infos.sort_by(|a, b| a.name.cmp(&b.name));

    let document = parts
        .iter()
        .find(|p| p.name == "word/document.xml")
        .context("missing word/document.xml - is this a Word document?")?;
    let revisions = count_revisions(&document.data);

    let comment_count = parts
        .iter()
        .find(|p| p.name == "word/comments.xml")
        .map(|p| {
            let tags: [&[u8]; 1] = [b"w:comment"];
            count_elements(&p.data, &tags)[0]
        })
        .unwrap_or(0);

    Ok(DocxSummary { parts: part_infos, revisions, comment_count })
}

/// Read a `.docx` and write it back, then verify every part is byte-identical.
pub fn roundtrip(input: &Path, output: &Path) -> Result<RoundtripReport> {
    let original = read_parts(input)?;
    write_parts(output, &original)?;
    let rebuilt = read_parts(output)?;
    Ok(diff_parts(&original, &rebuilt))
}

/// Round-trip a `.docx` entirely in memory - read parts, re-serialize, verify every part is
/// byte-identical - without writing an output file. The corpus-gate path: cheap enough to run
/// over thousands of documents.
pub fn roundtrip_bytes(bytes: &[u8]) -> Result<RoundtripReport> {
    let original = read_parts_bytes(bytes)?;
    let rebuilt = read_parts_bytes(&write_parts_bytes(&original)?)?;
    Ok(diff_parts(&original, &rebuilt))
}

/// The round-trip stability check: compare two part sets by name + decompressed bytes.
fn diff_parts(original: &[Part], rebuilt: &[Part]) -> RoundtripReport {
    use std::collections::HashMap;
    let by_name: HashMap<&str, &Vec<u8>> =
        original.iter().map(|p| (p.name.as_str(), &p.data)).collect();

    let mut stable = original.len() == rebuilt.len();
    let mut first_diff = None;
    for p in rebuilt {
        let same = matches!(by_name.get(p.name.as_str()), Some(d) if **d == p.data);
        if !same {
            stable = false;
            if first_diff.is_none() {
                first_diff = Some(p.name.clone());
            }
        }
    }

    RoundtripReport { parts: original.len(), stable, first_diff }
}

/// Inject one author-attributed tracked insertion (`w:ins`) as a new paragraph, preserving
/// every other part - and every other byte of `word/document.xml` - verbatim.
pub fn redline(
    input: &Path,
    output: &Path,
    author: &str,
    date: &str,
    text: &str,
) -> Result<RedlineReport> {
    let mut parts = read_parts(input)?;
    let document = parts
        .iter_mut()
        .find(|p| p.name == "word/document.xml")
        .context("missing word/document.xml")?;

    let injected_id = max_w_id(&document.data) + 1;
    let paragraph = format!(
        "<w:p><w:ins w:id=\"{id}\" w:author=\"{author}\" w:date=\"{date}\">\
<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:ins></w:p>",
        id = injected_id,
        author = xml_escape(author),
        date = xml_escape(date),
        text = xml_escape(text),
    );

    // Surgical insert immediately before the closing </w:body>: a pure byte splice, so the rest
    // of document.xml is preserved exactly.
    let pos = find_last(&document.data, b"</w:body>")
        .context("no </w:body> in word/document.xml")?;
    let mut edited = Vec::with_capacity(document.data.len() + paragraph.len());
    edited.extend_from_slice(&document.data[..pos]);
    edited.extend_from_slice(paragraph.as_bytes());
    edited.extend_from_slice(&document.data[pos..]);
    document.data = edited;

    write_parts(output, &parts)?;
    Ok(RedlineReport { injected_id, author: author.to_string(), parts: parts.len() })
}

fn count_revisions(document_xml: &[u8]) -> RevisionCounts {
    let tags: [&[u8]; 6] = [
        b"w:ins",
        b"w:del",
        b"w:rPrChange",
        b"w:pPrChange",
        b"w:moveFrom",
        b"w:moveTo",
    ];
    let c = count_elements(document_xml, &tags);
    RevisionCounts {
        ins: c[0],
        del: c[1],
        r_pr_change: c[2],
        p_pr_change: c[3],
        move_from: c[4],
        move_to: c[5],
    }
}

/// Count start/empty elements whose qualified name matches each tag in `tags`.
fn count_elements(xml: &[u8], tags: &[&[u8]]) -> Vec<usize> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut counts = vec![0usize; tags.len()];
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let qname = e.name();
                let name = qname.as_ref();
                for (i, tag) in tags.iter().enumerate() {
                    if name == *tag {
                        counts[i] += 1;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    counts
}

/// Largest numeric `w:id="..."` attribute value in the XML (0 if none).
fn max_w_id(xml: &[u8]) -> u64 {
    let needle: &[u8] = b"w:id=\"";
    let mut max = 0u64;
    let mut base = 0usize;
    while let Some(rel) = xml[base..].windows(needle.len()).position(|w| w == needle) {
        let start = base + rel + needle.len();
        let mut j = start;
        while j < xml.len() && xml[j].is_ascii_digit() {
            j += 1;
        }
        if j > start
            && let Ok(s) = std::str::from_utf8(&xml[start..j])
                && let Ok(v) = s.parse::<u64>() {
                    max = max.max(v);
                }
        base = start;
    }
    max
}

/// Byte index of the last occurrence of `needle` in `hay`.
fn find_last(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// Escape the five XML predefined entities for safe attribute/text content.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ── accept / reject tracked changes ──────────────────────────────────────────

/// Which revisions a [`resolve`] call targets.
pub enum Target {
    /// Every revision in the document.
    All,
    /// Only the revision(s) carrying this `w:id`.
    Id(u64),
}

/// Result of a [`resolve`] (accept/reject) pass.
#[derive(Debug)]
pub struct ResolveReport {
    /// Revisions resolved (accepted or rejected).
    pub resolved: usize,
    /// Targeted revisions left untouched because they are not supported yet
    /// (moves, and format/paragraph-property changes on reject).
    pub skipped: usize,
}

/// Accept or reject tracked changes in `word/document.xml`, writing the result to `output`.
///
/// `accept = true` confirms changes (insertions are unwrapped; deletions and `rPrChange` /
/// `pPrChange` records are removed). `accept = false` rejects them (insertions are removed;
/// deletions are restored to normal text). Moves and format-change *rejections* are not yet
/// supported and are reported in `skipped`. Every other part of the package is preserved verbatim.
pub fn resolve(input: &Path, output: &Path, accept: bool, target: Target) -> Result<ResolveReport> {
    let mut parts = read_parts(input)?;
    let document = parts
        .iter_mut()
        .find(|p| p.name == "word/document.xml")
        .context("missing word/document.xml")?;

    let (new_xml, resolved, skipped) = resolve_document(&document.data, accept, &target)?;
    document.data = new_xml;

    write_parts(output, &parts)?;
    Ok(ResolveReport { resolved, skipped })
}

#[derive(Clone, Copy)]
enum Frame {
    /// Element emitted normally; its end is emitted too.
    Normal,
    /// Revision wrapper unwrapped: start/end suppressed, children emitted.
    Unwrap,
    /// Revision subtree dropped: this element opened the drop.
    Drop,
    /// Element nested inside a dropped subtree.
    InsideDrop,
}

#[derive(Clone, Copy)]
enum Kind {
    Ins,
    Del,
    RPrChange,
    PPrChange,
    MoveFrom,
    MoveTo,
}

fn revision_kind(name: &[u8]) -> Option<Kind> {
    match name {
        b"w:ins" => Some(Kind::Ins),
        b"w:del" => Some(Kind::Del),
        b"w:rPrChange" => Some(Kind::RPrChange),
        b"w:pPrChange" => Some(Kind::PPrChange),
        b"w:moveFrom" => Some(Kind::MoveFrom),
        b"w:moveTo" => Some(Kind::MoveTo),
        _ => None,
    }
}

enum Op {
    Unwrap,
    Drop,
    /// Leave as-is (unsupported in this build: moves; format-changes on reject).
    Skip,
}

fn op_for(accept: bool, kind: Kind) -> Op {
    match (accept, kind) {
        (true, Kind::Ins) => Op::Unwrap,
        (true, Kind::Del | Kind::RPrChange | Kind::PPrChange) => Op::Drop,
        (false, Kind::Ins) => Op::Drop,
        (false, Kind::Del) => Op::Unwrap,
        _ => Op::Skip,
    }
}

fn attr_w_id(e: &quick_xml::events::BytesStart) -> Option<u64> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == b"w:id"
            && let Ok(s) = std::str::from_utf8(a.value.as_ref())
                && let Ok(n) = s.parse::<u64>() {
                    return Some(n);
                }
    }
    None
}

fn is_targeted(target: &Target, e: &quick_xml::events::BytesStart) -> bool {
    match target {
        Target::All => true,
        Target::Id(n) => attr_w_id(e) == Some(*n),
    }
}

/// Clone a start tag to an owned copy, renaming `w:delText` -> `w:t` when rejecting (so a restored
/// deletion becomes normal text).
fn emit_start(
    e: &quick_xml::events::BytesStart,
    accept: bool,
) -> quick_xml::events::BytesStart<'static> {
    if !accept && e.name().as_ref() == b"w:delText" {
        let mut n = quick_xml::events::BytesStart::new("w:t".to_owned());
        for a in e.attributes().flatten() {
            n.push_attribute((a.key.as_ref(), a.value.as_ref()));
        }
        n.into_owned()
    } else {
        e.clone().into_owned()
    }
}

fn emit_end(e: &quick_xml::events::BytesEnd, accept: bool) -> quick_xml::events::BytesEnd<'static> {
    if !accept && e.name().as_ref() == b"w:delText" {
        quick_xml::events::BytesEnd::new("w:t")
    } else {
        e.clone().into_owned()
    }
}

/// Stream `document.xml` through a revision-resolving transform. Returns the new XML plus
/// (resolved, skipped) counts. Untouched structure is re-serialized faithfully by quick-xml.
fn resolve_document(xml: &[u8], accept: bool, target: &Target) -> Result<(Vec<u8>, usize, usize)> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut writer = quick_xml::Writer::new(Vec::new());
    let mut buf = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut suppress: usize = 0;
    let mut resolved = 0usize;
    let mut skipped = 0usize;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(e) => {
                let action = if suppress > 0 {
                    None
                } else {
                    revision_kind(e.name().as_ref())
                        .filter(|_| is_targeted(target, &e))
                        .map(|k| op_for(accept, k))
                };
                match action {
                    None => {
                        if suppress > 0 {
                            stack.push(Frame::InsideDrop);
                        } else {
                            stack.push(Frame::Normal);
                            writer.write_event(Event::Start(emit_start(&e, accept)))?;
                        }
                    }
                    Some(Op::Drop) => {
                        stack.push(Frame::Drop);
                        suppress += 1;
                        resolved += 1;
                    }
                    Some(Op::Unwrap) => {
                        stack.push(Frame::Unwrap);
                        resolved += 1;
                    }
                    Some(Op::Skip) => {
                        skipped += 1;
                        stack.push(Frame::Normal);
                        writer.write_event(Event::Start(emit_start(&e, accept)))?;
                    }
                }
            }
            Event::End(e) => match stack.pop().unwrap_or(Frame::Normal) {
                Frame::Normal => writer.write_event(Event::End(emit_end(&e, accept)))?,
                Frame::Unwrap | Frame::InsideDrop => {}
                Frame::Drop => suppress = suppress.saturating_sub(1),
            },
            Event::Empty(e) => {
                if suppress > 0 {
                    // inside a dropped subtree: emit nothing
                } else if let Some(kind) =
                    revision_kind(e.name().as_ref()).filter(|_| is_targeted(target, &e))
                {
                    match op_for(accept, kind) {
                        Op::Drop | Op::Unwrap => resolved += 1,
                        Op::Skip => {
                            skipped += 1;
                            writer.write_event(Event::Empty(emit_start(&e, accept)))?;
                        }
                    }
                } else {
                    writer.write_event(Event::Empty(emit_start(&e, accept)))?;
                }
            }
            other => {
                if suppress == 0 {
                    writer.write_event(other)?;
                }
            }
        }
        buf.clear();
    }

    Ok((writer.into_inner(), resolved, skipped))
}
