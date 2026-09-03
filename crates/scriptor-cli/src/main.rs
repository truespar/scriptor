//! `scriptor` - the OOXML correctness test bench.
//!
//! `inspect` / `roundtrip` / `redline` / `accept` / `reject` all work. The test loop is: run a
//! command, open the output `.docx` in real Microsoft Word, confirm fidelity.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "scriptor", version, about = "Scriptor OOXML test bench")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Probe a .docx: list its parts and count tracked-change elements + comments.
    Inspect {
        /// Path to the .docx file.
        file: PathBuf,
    },
    /// Read a .docx and write it back, asserting a byte-stable round-trip. Point it at a
    /// directory to gate a whole corpus: every .docx underneath is round-tripped in memory,
    /// results are summarized (optionally as JSON), and the command fails if any document is
    /// not byte-stable.
    Roundtrip {
        /// A .docx file, or a directory to check recursively (batch mode).
        input: PathBuf,
        /// Where to write the round-tripped .docx (single-file mode only).
        output: Option<PathBuf>,
        /// Batch mode: write per-document results to this JSON file (the baseline ledger).
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// Open a .docx through the CRDT and save it straight back with no edit, asserting that every
    /// part the model does not own comes back byte-identical and nothing is dropped.
    ///
    /// This is the browser save path (`to_docx_bytes`), which neither `roundtrip` nor `remodel`
    /// exercises: the first is a container repack that never builds the model, and the second goes
    /// through `export_docx`, which replaces `word/document.xml` and copies the rest verbatim. Point
    /// it at a directory to gate a whole corpus.
    Resave {
        /// A .docx file, or a directory to check recursively (batch mode).
        input: PathBuf,
        /// Batch mode: write per-document results to this JSON file (the baseline ledger).
        #[arg(long)]
        json: Option<PathBuf>,
        /// Also compare the element histogram inside the regenerated `word/document.xml` and report
        /// anything that appears fewer times after the save.
        ///
        /// Off by default because it is a far stricter bar than the part-level check and most of the
        /// corpus does not meet it yet: the model drops what it does not represent, so footnote and
        /// endnote references, OMML math, `w:sym`, multi-column section properties and named
        /// bookmarks go missing. Turning it on is how that backlog is measured and, eventually,
        /// gated - see `docs/passthrough.md`.
        #[arg(long)]
        elements: bool,
    },
    /// Inject one tracked insertion attributed to an author.
    Redline {
        input: PathBuf,
        output: PathBuf,
        /// Author the injected tracked change is attributed to.
        #[arg(long, default_value = "Scriptor Agent")]
        author: String,
        /// ISO-8601 date stamped on the revision.
        #[arg(long, default_value = "2026-01-01T00:00:00Z")]
        date: String,
        /// Text of the inserted run.
        #[arg(long, default_value = "Inserted by Scriptor.")]
        text: String,
    },
    /// Accept tracked changes (insertions kept, deletions removed, change records cleared).
    Accept {
        input: PathBuf,
        output: PathBuf,
        /// Resolve only the revision with this w:id (default: all revisions).
        #[arg(long)]
        id: Option<u64>,
    },
    /// Reject tracked changes (insertions removed, deletions restored).
    Reject {
        input: PathBuf,
        output: PathBuf,
        /// Resolve only the revision with this w:id (default: all revisions).
        #[arg(long)]
        id: Option<u64>,
    },
    /// Compare two .docx documents and produce a redline: `original` with every difference as an
    /// author-attributed tracked change (the model-based blacklining path). Verify with `--check`,
    /// which asserts accept-all reproduces the revised doc and reject-all the original.
    Compare {
        /// The original ("before") document.
        original: PathBuf,
        /// The revised ("after") document.
        revised: PathBuf,
        /// Where to write the redlined .docx (omit with `--check`).
        output: Option<PathBuf>,
        /// Author every emitted revision is attributed to.
        #[arg(long, default_value = "Compare")]
        author: String,
        /// ISO-8601 date stamped on every revision.
        #[arg(long, default_value = "2026-01-01T00:00:00Z")]
        date: String,
        /// Also write the machine-readable change manifest (JSON) to this path.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Run the oracle (accept-all == revised, reject-all == original) and report; no output doc.
        #[arg(long)]
        check: bool,
    },
    /// Import a .docx into the CRDT model and print its paragraphs (style, runs, tracked changes).
    Model {
        /// Path to the .docx file.
        file: PathBuf,
    },
    /// Round-trip a .docx through the CRDT model: import then re-serialize `word/document.xml`
    /// (preserving every other part of the input). Open the output in Word to verify fidelity.
    Remodel { input: PathBuf, output: PathBuf },
    /// Render every page of a .docx to PNG via the real engine (the same pipeline as the browser
    /// canvas). The visual-diff harness compares these against Word / LibreOffice.
    Render {
        /// Path to the .docx file.
        input: PathBuf,
        /// Output directory for `page-NNN.png` (created if missing).
        out_dir: PathBuf,
        /// Render scale (1.0 = 96 px/in, matching a PDF rasterized at -density 96).
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Tracked-change display: `all` (markup shown), `simple`/`none` (deletions hidden, the
        /// Final view), or `original` (insertions hidden). Match the reference renderer's view.
        #[arg(long, default_value = "all")]
        track: String,
    },
    /// Dump the engine's per-paragraph layout geometry (page + on-page position in points + the
    /// computed list marker) as JSON - the Scriptor half of the geometry oracle. Diff against the
    /// Word-COM reference (`scripts/word-geometry.ps1`) to localize layout divergence exactly.
    Geometry {
        /// Path to the .docx file.
        input: PathBuf,
        /// Output JSON path (default: stdout).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Layout scale (1.0 = 96 px/in). The dump is in points regardless, so this rarely matters.
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Tracked-change display the geometry is measured in (match the Word reference's view).
        #[arg(long, default_value = "all")]
        track: String,
    },
    /// Scan a .docx (or a folder of them) and rank the OOXML elements we do NOT yet model, by how
    /// many files use them - a data-driven backlog for implementing the standard. Looks at the body
    /// content parts (document.xml + headers/footers).
    Coverage {
        /// A .docx file or a directory to scan recursively.
        path: PathBuf,
        /// Also list the elements we already model.
        #[arg(long)]
        all: bool,
        /// Max unsupported elements to list (0 = no limit).
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Inspect { file } => inspect(&file),
        Command::Roundtrip { input, output, json } => {
            if input.is_dir() {
                roundtrip_corpus(&input, json.as_deref())
            } else {
                let output = output.ok_or_else(|| {
                    anyhow::anyhow!("an output path is required (or pass a directory for batch mode)")
                })?;
                roundtrip(&input, &output)
            }
        }
        Command::Resave { input, json, elements } => {
            if input.is_dir() {
                resave_corpus(&input, json.as_deref(), elements)
            } else {
                resave_one(&input, elements)
            }
        }
        Command::Redline { input, output, author, date, text } => {
            redline(&input, &output, &author, &date, &text)
        }
        Command::Accept { input, output, id } => resolve(&input, &output, true, id),
        Command::Reject { input, output, id } => resolve(&input, &output, false, id),
        Command::Compare { original, revised, output, author, date, manifest, check } => {
            compare(&original, &revised, output.as_deref(), &author, &date, manifest.as_deref(), check)
        }
        Command::Model { file } => model(&file),
        Command::Remodel { input, output } => remodel(&input, &output),
        Command::Render { input, out_dir, scale, track } => {
            render_pages(&input, &out_dir, scale, &track)
        }
        Command::Geometry { input, out, scale, track } => geometry(&input, out.as_deref(), scale, &track),
        Command::Coverage { path, all, limit } => coverage(&path, all, limit),
    }
}

fn geometry(input: &Path, out: Option<&Path>, scale: f32, track: &str) -> Result<()> {
    let bytes = std::fs::read(input)?;
    let json = scriptor_wasm::dump_geometry(&bytes, scale, track)?;
    match out {
        Some(path) => {
            std::fs::write(path, &json)?;
            eprintln!("geometry -> {}", path.display());
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn render_pages(input: &Path, out_dir: &Path, scale: f32, track: &str) -> Result<()> {
    let bytes = std::fs::read(input)?;
    let pages = scriptor_wasm::render_all_pages(&bytes, scale, track)?;
    std::fs::create_dir_all(out_dir)?;
    for (i, p) in pages.iter().enumerate() {
        let path = out_dir.join(format!("page-{:03}.png", i + 1));
        let img = image::RgbaImage::from_raw(p.width, p.height, p.rgba.clone())
            .ok_or_else(|| anyhow::anyhow!("invalid pixel buffer for page {}", i + 1))?;
        img.save(&path)?;
        println!("  {}  ({}x{})", path.display(), p.width, p.height);
    }
    println!(
        "rendered {} page(s) from {} at scale {scale} (track={track})",
        pages.len(),
        input.display()
    );
    Ok(())
}

fn inspect(file: &Path) -> Result<()> {
    let summary = scriptor_ooxml::inspect(file)?;

    println!("{}", file.display());
    println!("  parts: {}", summary.parts.len());
    for p in &summary.parts {
        println!("    {:>9}  {}", p.size, p.name);
    }

    let r = summary.revisions;
    println!("  revisions ({} total):", r.total());
    println!(
        "    w:ins={}  w:del={}  w:rPrChange={}  w:pPrChange={}  w:moveFrom={}  w:moveTo={}",
        r.ins, r.del, r.r_pr_change, r.p_pr_change, r.move_from, r.move_to
    );
    println!("  comments: {}", summary.comment_count);
    Ok(())
}

fn roundtrip(input: &Path, output: &Path) -> Result<()> {
    let r = scriptor_ooxml::roundtrip(input, output)?;
    println!("round-trip {} -> {}", input.display(), output.display());
    println!("  parts: {}", r.parts);
    if r.stable {
        println!("  byte-stable: yes (every part identical)");
        Ok(())
    } else {
        println!(
            "  byte-stable: NO (first difference: {})",
            r.first_diff.unwrap_or_default()
        );
        anyhow::bail!("round-trip was not byte-stable")
    }
}

/// Parts `to_docx_bytes` is entitled to rewrite on a save with no edit.
///
/// `word/document.xml` is regenerated from the model. `word/styles.xml` is merged into: modeled
/// props are patched in place and canonical quick styles the document lacked are appended, so it
/// grows even when nothing changed. The content-type and relationship parts are patched so the
/// rebuilt document's synthesized rel ids resolve.
///
/// Everything else must come back byte-identical, and the list is deliberately short. Headers and
/// footers are absent because they are re-rendered only when edited. The comment parts are absent
/// for the same reason - a comment body is modeled as plain text, so re-emitting one discards run
/// formatting and any table inside it, and an untouched comment set is passed through instead.
/// `word/numbering.xml` is absent because it is only ever appended to, and only when a list was
/// synthesized this session; a resave synthesizes nothing.
///
/// Keeping a part out of this list is what makes the gate able to see a regression in it. Adding a
/// name here silences that, so it should happen only when the save path genuinely must rewrite the
/// part on every save.
const REWRITABLE: &[&str] = &[
    "[Content_Types].xml",
    "word/_rels/document.xml.rels",
    "word/document.xml",
    "word/styles.xml",
];

/// Elements whose disappearance is not loss: Word regenerates them on open, and every save
/// legitimately drops them. Counting them would bury the real signal - `w:proofErr` alone fires on
/// hundreds of corpus documents.
const REGENERATED_BY_WORD: &[&str] = &[
    "w:proofErr",            // spell/grammar-check markers
    "w:lastRenderedPageBreak", // a layout cache from the last renderer to touch the file
    "w:bookmarkEnd",         // paired with bookmarkStart by id; the start is the name-bearing one
];

/// Count every element name in an XML part, skipping what Word regenerates.
///
/// The point is to need no whitelist of *interesting* elements. A part-level check cannot see
/// inside `word/document.xml`, which is regenerated on every save and so must be in [`REWRITABLE`] -
/// and that is precisely where content the model does not represent goes missing. Comparing the
/// element histogram before and after catches anything that vanished, including elements nobody has
/// thought of yet.
///
/// `w:bookmarkStart` is filtered by name rather than skipped wholesale: `_GoBack` is Word's
/// internal "where was I" marker, rewritten on every open, and `_Toc*` entries are regenerated when
/// a table of contents is refreshed. A *named* bookmark going missing is real - it is what a
/// cross-reference points at.
fn element_counts(xml: &[u8]) -> std::collections::BTreeMap<String, usize> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut counts = std::collections::BTreeMap::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if REGENERATED_BY_WORD.contains(&name.as_str()) {
                    buf.clear();
                    continue;
                }
                if name == "w:bookmarkStart" {
                    let transient = e.attributes().flatten().any(|a| {
                        a.key.as_ref() == b"w:name"
                            && {
                                let v = String::from_utf8_lossy(&a.value).into_owned();
                                v == "_GoBack" || v.starts_with("_Toc")
                            }
                    });
                    if transient {
                        buf.clear();
                        continue;
                    }
                }
                *counts.entry(name).or_insert(0) += 1;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    counts
}

/// Visible characters in a part: everything inside `w:t` and `w:delText`, wherever it sits -
/// including text nested in a table or a text box.
///
/// This is the one unambiguous measure of loss inside `word/document.xml`. The element histogram
/// over-reports badly, because merging two adjacent runs with identical formatting removes a `w:rPr`
/// and a `w:b` without changing a single character; 189 of the 219 corpus documents that "lose" a
/// `w:b` keep every bold character. Text either survives or it does not.
fn visible_text_len(xml: &[u8]) -> usize {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    // Whether a `<w:r>` is open. A `w:tab` is a character only as a run child - the same element in
    // `<w:pPr><w:tabs>` is a tab STOP definition, and counting those reported 115 documents as losing
    // text purely because the model normalizes tab stops.
    let mut in_run = false;
    let mut n = 0usize;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if matches!(e.name().as_ref(), b"w:t" | b"w:delText") => depth += 1,
            Ok(Event::End(e)) if matches!(e.name().as_ref(), b"w:t" | b"w:delText") => {
                depth = depth.saturating_sub(1)
            }
            Ok(Event::Start(e)) if e.name().as_ref() == b"w:r" => in_run = true,
            Ok(Event::End(e)) if e.name().as_ref() == b"w:r" => in_run = false,
            Ok(Event::Text(t)) if depth > 0 => {
                if let Ok(s) = t.decode() {
                    n += s.chars().count();
                }
            }
            // An entity reference is one character of text, and quick-xml reports it separately from
            // the text around it. Counting only `Text` made an apostrophe look like a lost character
            // whenever the original wrote `'` and the export wrote `&apos;` - which reported 38
            // corpus documents as losing text when they had lost nothing at all.
            Ok(Event::GeneralRef(_)) if depth > 0 => n += 1,
            // A tab is one character of run text - the importer maps `w:tab` to `\t` - and it can
            // arrive either as the element or as a literal tab inside `w:t`. Counting only the
            // literal form made the canonical rewrite (which is what Word itself writes) read as a
            // lost character.
            Ok(Event::Empty(e)) if in_run && e.name().as_ref() == b"w:tab" => n += 1,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    n
}

/// What a no-op open/save did to one document.
struct ResaveReport {
    parts: usize,
    /// Parts present at open and missing at save. Always a defect.
    dropped: Vec<String>,
    /// Parts that changed which the save path has no business touching.
    changed: Vec<String>,
    /// Visible characters in `word/document.xml` before and after, when the count fell.
    ///
    /// Checked by default rather than behind `--elements`: a part-level check cannot see inside the
    /// one part that must be rewritten, and losing text there is the least arguable defect there is.
    lost_text: Option<(usize, usize)>,
    /// Elements that appear fewer times in the regenerated `word/document.xml` than they did in the
    /// original, as `(name, before, after)`. This is the loss a part-level check cannot see.
    lost_elements: Vec<(String, usize, usize)>,
}

impl ResaveReport {
    fn ok(&self) -> bool {
        self.dropped.is_empty()
            && self.changed.is_empty()
            && self.lost_text.is_none()
            && self.lost_elements.is_empty()
    }

    /// The first offending item, for the ledger's detail column. Text loss outranks everything else.
    fn detail(&self) -> String {
        if let Some((b, a)) = self.lost_text {
            return format!("lost text ({b} -> {a} chars)");
        }
        if let Some(d) = self.dropped.first() {
            return format!("dropped {d}");
        }
        if let Some(c) = self.changed.first() {
            return format!("changed {c}");
        }
        match self.lost_elements.first() {
            Some((n, b, a)) => format!("lost {n} ({b} -> {a})"),
            None => String::new(),
        }
    }
}

/// Open `bytes` through the CRDT, save straight back, and diff the two part sets.
fn resave_bytes(bytes: &[u8], elements: bool) -> Result<ResaveReport> {
    let before = scriptor_ooxml::read_parts_bytes(bytes)?;
    let doc = scriptor_crdt::CollabDoc::from_docx_bytes(bytes)?;
    let after = scriptor_ooxml::read_parts_bytes(&doc.to_docx_bytes()?)?;

    let saved: std::collections::HashMap<&str, &Vec<u8>> =
        after.iter().map(|p| (p.name.as_str(), &p.data)).collect();

    let mut dropped = Vec::new();
    let mut changed = Vec::new();
    for p in &before {
        match saved.get(p.name.as_str()) {
            None => dropped.push(p.name.clone()),
            Some(d) if **d != p.data && !REWRITABLE.contains(&p.name.as_str()) => {
                changed.push(p.name.clone())
            }
            Some(_) => {}
        }
    }

    // Inside the one part that must be rewritten: did anything disappear?
    let doc_before = before.iter().find(|p| p.name == "word/document.xml").map(|p| &p.data);
    let doc_after = saved.get("word/document.xml");

    let mut lost_text = None;
    if let (Some(b), Some(a)) = (doc_before, doc_after) {
        let (tb, ta) = (visible_text_len(b), visible_text_len(a));
        if ta < tb {
            lost_text = Some((tb, ta));
        }
    }

    let mut lost_elements = Vec::new();
    if elements && let (Some(b), Some(a)) = (doc_before, doc_after) {
        let (cb, ca) = (element_counts(b), element_counts(a));
        for (name, n) in cb {
            let after = ca.get(&name).copied().unwrap_or(0);
            if after < n {
                lost_elements.push((name, n, after));
            }
        }
    }

    Ok(ResaveReport { parts: before.len(), dropped, changed, lost_text, lost_elements })
}

fn resave_one(input: &Path, elements: bool) -> Result<()> {
    let r = resave_bytes(&std::fs::read(input)?, elements)?;
    println!("resave {}", input.display());
    println!("  parts: {}", r.parts);
    if r.ok() {
        println!("  lossless: yes (no part dropped or rewritten, no text lost)");
        return Ok(());
    }
    for d in &r.dropped {
        println!("  DROPPED  {d}");
    }
    for c in &r.changed {
        println!("  CHANGED  {c}");
    }
    if let Some((b, a)) = r.lost_text {
        println!("  LOST TEXT  {b} -> {a} characters in word/document.xml");
    }
    for (n, b, a) in &r.lost_elements {
        println!("  LOST     {n}  ({b} -> {a}) in word/document.xml");
    }
    anyhow::bail!("resave was not lossless")
}

/// The corpus gate for the browser save path. Every `.docx` under `dir` is opened through the CRDT
/// and saved straight back with no edit; any part that vanishes, or that changes without being in
/// [`REWRITABLE`], is a regression. Unreadable files are reported but do not fail the run, matching
/// [`roundtrip_corpus`] - the invariant is scoped to documents we can open.
fn resave_corpus(dir: &Path, json: Option<&Path>, elements: bool) -> Result<()> {
    let files = collect_docx(dir);
    if files.is_empty() {
        anyhow::bail!("no .docx files found at {}", dir.display());
    }

    struct Row {
        file: String,
        parts: usize,
        status: &'static str,
        detail: String,
    }
    let mut rows: Vec<Row> = Vec::with_capacity(files.len());
    let (mut lossless, mut lossy, mut errors) = (0u32, 0u32, 0u32);
    for f in &files {
        let rel = f.strip_prefix(dir).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let result = std::fs::read(f).map_err(anyhow::Error::from).and_then(|b| resave_bytes(&b, elements));
        match result {
            Ok(r) if r.ok() => {
                lossless += 1;
                rows.push(Row { file: rel, parts: r.parts, status: "lossless", detail: String::new() });
            }
            Ok(r) => {
                lossy += 1;
                let detail = r.detail();
                println!("LOSSY       {rel}  ({detail})");
                rows.push(Row { file: rel, parts: r.parts, status: "lossy", detail });
            }
            Err(e) => {
                errors += 1;
                println!("UNREADABLE  {rel}  ({e:#})");
                rows.push(Row { file: rel, parts: 0, status: "error", detail: format!("{e:#}") });
            }
        }
    }

    println!("resave corpus {}", dir.display());
    println!(
        "  scanned: {}   lossless: {lossless}   lossy: {lossy}   unreadable: {errors}",
        files.len()
    );

    if let Some(path) = json {
        let mut out = String::from("{\n");
        out.push_str(&format!(
            "  \"scanned\": {},\n  \"lossless\": {lossless},\n  \"lossy\": {lossy},\n  \"unreadable\": {errors},\n  \"docs\": [\n",
            files.len()
        ));
        for (i, r) in rows.iter().enumerate() {
            let detail = match r.status {
                "lossy" => format!(", \"loss\": \"{}\"", json_escape(&r.detail)),
                "error" => format!(", \"error\": \"{}\"", json_escape(&r.detail)),
                _ => String::new(),
            };
            let comma = if i + 1 < rows.len() { "," } else { "" };
            out.push_str(&format!(
                "    {{\"file\": \"{}\", \"status\": \"{}\", \"parts\": {}{detail}}}{comma}\n",
                json_escape(&r.file),
                r.status,
                r.parts
            ));
        }
        out.push_str("  ]\n}\n");
        std::fs::write(path, out)?;
        println!("  results: {}", path.display());
    }

    if lossy > 0 {
        anyhow::bail!("{lossy} document(s) lost content on save");
    }
    Ok(())
}

/// The corpus round-trip gate promised in `docs/passthrough.md`: every `.docx` under `dir`
/// round-tripped in memory, byte-stability asserted per document. Unreadable files (not a zip,
/// truncated) are reported but do not fail the gate - the invariant is scoped to documents we
/// can open. Any unstable document fails the run.
fn roundtrip_corpus(dir: &Path, json: Option<&Path>) -> Result<()> {
    let files = collect_docx(dir);
    if files.is_empty() {
        anyhow::bail!("no .docx files found at {}", dir.display());
    }

    struct Row {
        file: String,
        parts: usize,
        status: &'static str,
        /// `first_diff` for unstable rows, the error text for error rows, empty for stable.
        detail: String,
    }
    let mut rows: Vec<Row> = Vec::with_capacity(files.len());
    let (mut stable, mut unstable, mut errors) = (0u32, 0u32, 0u32);
    for f in &files {
        let rel = f.strip_prefix(dir).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let result = std::fs::read(f)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| scriptor_ooxml::roundtrip_bytes(&bytes));
        match result {
            Ok(r) if r.stable => {
                stable += 1;
                rows.push(Row { file: rel, parts: r.parts, status: "stable", detail: String::new() });
            }
            Ok(r) => {
                unstable += 1;
                let diff = r.first_diff.unwrap_or_default();
                println!("UNSTABLE    {rel}  (first difference: {diff})");
                rows.push(Row { file: rel, parts: r.parts, status: "unstable", detail: diff });
            }
            Err(e) => {
                errors += 1;
                println!("UNREADABLE  {rel}  ({e:#})");
                rows.push(Row { file: rel, parts: 0, status: "error", detail: format!("{e:#}") });
            }
        }
    }

    println!("round-trip corpus {}", dir.display());
    println!(
        "  scanned: {}   stable: {stable}   unstable: {unstable}   unreadable: {errors}",
        files.len()
    );

    if let Some(path) = json {
        let mut out = String::from("{\n");
        out.push_str(&format!(
            "  \"scanned\": {},\n  \"stable\": {stable},\n  \"unstable\": {unstable},\n  \"unreadable\": {errors},\n  \"docs\": [\n",
            files.len()
        ));
        for (i, r) in rows.iter().enumerate() {
            let detail = match r.status {
                "unstable" => format!(", \"firstDiff\": \"{}\"", json_escape(&r.detail)),
                "error" => format!(", \"error\": \"{}\"", json_escape(&r.detail)),
                _ => String::new(),
            };
            let comma = if i + 1 < rows.len() { "," } else { "" };
            out.push_str(&format!(
                "    {{\"file\": \"{}\", \"status\": \"{}\", \"parts\": {}{detail}}}{comma}\n",
                json_escape(&r.file),
                r.status,
                r.parts
            ));
        }
        out.push_str("  ]\n}\n");
        std::fs::write(path, out)?;
        println!("  results: {}", path.display());
    }

    if unstable > 0 {
        anyhow::bail!("{unstable} document(s) did not round-trip byte-stable");
    }
    Ok(())
}

/// Minimal JSON string escaping for the hand-rolled results ledger (paths + error messages).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn redline(input: &Path, output: &Path, author: &str, date: &str, text: &str) -> Result<()> {
    let r = scriptor_ooxml::redline(input, output, author, date, text)?;
    println!("redline {} -> {}", input.display(), output.display());
    println!("  injected w:ins id={} author=\"{}\"", r.injected_id, r.author);
    println!("  parts: {} (only word/document.xml changed)", r.parts);
    Ok(())
}

fn model(file: &Path) -> Result<()> {
    let doc = scriptor_crdt::CollabDoc::import_docx(file)?;
    let paragraphs = doc.paragraphs()?;
    println!("{} -> {} paragraph(s) modeled", file.display(), paragraphs.len());
    for (i, p) in paragraphs.iter().enumerate() {
        let style = p.style.as_deref().unwrap_or("-");
        println!("  [{i}] style={style} runs={}", p.runs.len());
        for r in &p.runs {
            let mut fmt = String::new();
            if r.bold {
                fmt.push('b');
            }
            if r.italic {
                fmt.push('i');
            }
            let track = match &r.track {
                Some(t) => format!(
                    "  <{:?} id={} author=\"{}\">",
                    t.kind, t.id, t.author
                ),
                None => String::new(),
            };
            println!("      [{fmt:<2}] {:?}{track}", r.text);
        }
    }
    Ok(())
}

fn remodel(input: &Path, output: &Path) -> Result<()> {
    let doc = scriptor_crdt::CollabDoc::import_docx(input)?;
    doc.export_docx(input, output)?;
    let paragraphs = doc.paragraphs()?;
    println!("remodel {} -> {}", input.display(), output.display());
    println!("  {} paragraph(s) round-tripped through the CRDT", paragraphs.len());
    println!("  word/document.xml re-serialized; every other part preserved verbatim");
    println!("  open {} in Word to verify fidelity", output.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compare(
    original: &Path,
    revised: &Path,
    output: Option<&Path>,
    author: &str,
    date: &str,
    manifest: Option<&Path>,
    check: bool,
) -> Result<()> {
    let original_bytes = std::fs::read(original)?;
    let revised_bytes = std::fs::read(revised)?;
    let opts = scriptor_compare::CompareOptions {
        author: author.to_string(),
        date: date.to_string(),
        ..Default::default()
    };

    if check {
        let r = scriptor_compare::check(&original_bytes, &revised_bytes, &opts)?;
        println!("compare --check {} -> {}", original.display(), revised.display());
        println!("  changes:   {}", r.changes);
        println!("  accept==revised: {}", if r.accept_ok { "OK" } else { "FAIL" });
        println!("  reject==original: {}", if r.reject_ok { "OK" } else { "FAIL" });
        if let Some((i, want, got)) = &r.accept_mismatch {
            println!("  first accept mismatch at ¶{i}: expected {want:?}, got {got:?}");
        }
        if let Some((i, want, got)) = &r.reject_mismatch {
            println!("  first reject mismatch at ¶{i}: expected {want:?}, got {got:?}");
        }
        if !r.ok() {
            anyhow::bail!("oracle failed - the redline does not reproduce both documents");
        }
        println!("  oracle: PASS");
        return Ok(());
    }

    let result = scriptor_compare::compare(&original_bytes, &revised_bytes, &opts)?;
    let out = output.ok_or_else(|| anyhow::anyhow!("an output path is required (or pass --check)"))?;
    std::fs::write(out, &result.redline)?;
    println!("compare {} -> {}", original.display(), revised.display());
    println!("  redline: {}", out.display());
    println!("  {}", result.manifest.summary());
    if let Some(path) = manifest {
        std::fs::write(path, result.manifest.to_json())?;
        println!("  manifest: {}", path.display());
    }
    println!("  open {} in Word to verify the redline", out.display());
    Ok(())
}

fn resolve(input: &Path, output: &Path, accept: bool, id: Option<u64>) -> Result<()> {
    let target = match id {
        Some(n) => scriptor_ooxml::Target::Id(n),
        None => scriptor_ooxml::Target::All,
    };
    let r = scriptor_ooxml::resolve(input, output, accept, target)?;
    let verb = if accept { "accept" } else { "reject" };
    println!("{verb} {} -> {}", input.display(), output.display());
    println!("  resolved: {}", r.resolved);
    if r.skipped > 0 {
        println!(
            "  skipped: {} (not yet supported: moves; format/paragraph-property changes on reject)",
            r.skipped
        );
    }
    Ok(())
}

// ── coverage scanner ─────────────────────────────────────────────────────────

/// OOXML elements the importer/layout currently models. Used to flag everything else as a gap.
/// Keep this in sync as support grows - it drives the `coverage` backlog. (Body-content elements
/// only; styles.xml / numbering.xml elements are resolved separately and not scanned here.)
const MODELED: &[&str] = &[
    // structure
    "w:document", "w:body", "w:p", "w:r", "w:sectPr", "w:pgSz", "w:pgMar",
    "w:headerReference", "w:footerReference", "w:hdr", "w:ftr",
    // tables
    "w:tbl", "w:tr", "w:tc", "w:tblPr", "w:tblGrid", "w:gridCol", "w:trPr", "w:tcPr",
    "w:gridSpan", "w:vMerge", "w:tblStyle", "w:tcW", "w:tblBorders", "w:tcBorders",
    "w:tblCellMar", "w:tcMar", "w:top", "w:left", "w:bottom", "w:right", "w:insideH", "w:insideV",
    "w:trHeight", "w:shd",
    // paragraph properties
    "w:pPr", "w:pStyle", "w:jc", "w:spacing", "w:ind", "w:numPr", "w:ilvl", "w:numId",
    "w:tabs", "w:tab",
    // run properties + text
    "w:rPr", "w:rFonts", "w:b", "w:i", "w:u", "w:strike", "w:sz", "w:color", "w:t", "w:delText",
    "w:highlight",
    // tracked changes + fields
    "w:ins", "w:del", "w:fldChar", "w:instrText", "w:fldSimple",
    // drawing (images)
    "w:drawing", "wp:anchor", "wp:inline", "wp:extent", "wp:positionH", "wp:positionV",
    "wp:posOffset", "wp:align", "a:blip",
    // embedded objects + non-picture drawings: pictures render via the image path, everything else
    // (OLE/ActiveX, charts, SmartArt, shapes, VML lines, text boxes) round-trips byte-stable via a
    // verbatim `raw~{id}` passthrough placeholder - see docs/passthrough.md
    "w:object", "w:control", "w:pict", "mc:AlternateContent",
    // block-level content controls / custom-XML: the `<w:sdtPr>` control definition round-trips as a
    // verbatim wrapper while the inner blocks stay modeled + editable (docs/passthrough.md P2)
    "w:sdt", "w:sdtPr", "w:sdtContent", "w:customXml", "w:customXmlPr",
];

/// Elements we deliberately do NOT model because they carry no layout/visual effect for rendering
/// (revision/proofing/bookmark markers) or just mirror a Latin property for complex scripts. Kept
/// out of the backlog so it surfaces only features that actually change the rendered page.
const IGNORABLE: &[&str] = &[
    "w:bookmarkStart", "w:bookmarkEnd", "w:proofErr", "w:lastRenderedPageBreak", "w:noProof",
    "w:lang", "w:szCs", "w:bCs", "w:iCs", "w:rsid", "w:commentRangeStart", "w:commentRangeEnd",
];

fn coverage(path: &Path, show_all: bool, limit: usize) -> Result<()> {
    let files = collect_docx(path);
    if files.is_empty() {
        anyhow::bail!("no .docx files found at {}", path.display());
    }
    let modeled: HashSet<&str> = MODELED.iter().copied().collect();
    let ignorable: HashSet<&str> = IGNORABLE.iter().copied().collect();

    // element -> (total occurrences, number of files containing it)
    let mut cov: HashMap<String, (u64, u32)> = HashMap::new();
    let mut scanned = 0u32;
    for f in &files {
        let Ok(bytes) = std::fs::read(f) else { continue };
        let parts = match scriptor_ooxml::read_parts_bytes(&bytes) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skip {}: {e}", f.display());
                continue;
            }
        };
        let mut seen: HashSet<String> = HashSet::new();
        for part in &parts {
            if is_body_part(&part.name) {
                scan_elements(&part.data, &mut cov, &mut seen);
            }
        }
        for e in seen {
            cov.entry(e).or_default().1 += 1;
        }
        scanned += 1;
    }

    // Category per element: 0 = real gap (implement), 1 = ignorable (no visual effect), 2 = modeled.
    let mut rows: Vec<(String, u64, u32, u8)> = cov
        .into_iter()
        .map(|(name, (occ, fc))| {
            let cat = if modeled.contains(name.as_str()) {
                2
            } else if ignorable.contains(name.as_str()) {
                1
            } else {
                0
            };
            (name, occ, fc, cat)
        })
        .collect();
    // Gaps first, then by files desc, then occurrences desc.
    rows.sort_by(|a, b| a.3.cmp(&b.3).then(b.2.cmp(&a.2)).then(b.1.cmp(&a.1)));

    let gaps: Vec<_> = rows.iter().filter(|r| r.3 == 0).collect();
    let ignored = rows.iter().filter(|r| r.3 == 1).count();
    let modeled_count = rows.iter().filter(|r| r.3 == 2).count();
    println!(
        "scanned {scanned} file(s)  -  {} distinct elements: {} modeled, {} ignorable, {} GAPS",
        rows.len(),
        modeled_count,
        ignored,
        gaps.len()
    );
    println!("\nGAPS - unsupported features with a visual effect (ranked by files):");
    println!("  {:>5}  {:>7}  element", "files", "occ");
    let show = if limit == 0 { gaps.len() } else { limit.min(gaps.len()) };
    for r in &gaps[..show] {
        println!("  {:>5}  {:>7}  {}", r.2, r.1, r.0);
    }
    if show < gaps.len() {
        println!("  ... and {} more (use --limit 0 to show all)", gaps.len() - show);
    }
    if show_all {
        println!("\nIGNORABLE (no visual effect):");
        for r in rows.iter().filter(|r| r.3 == 1) {
            println!("  {:>5}  {:>7}  {}", r.2, r.1, r.0);
        }
        println!("\nMODELED:");
        for r in rows.iter().filter(|r| r.3 == 2) {
            println!("  {:>5}  {:>7}  {}", r.2, r.1, r.0);
        }
    }
    Ok(())
}

/// Body content parts whose elements reflect layout features worth tracking.
fn is_body_part(name: &str) -> bool {
    name == "word/document.xml"
        || (name.starts_with("word/header") && name.ends_with(".xml"))
        || (name.starts_with("word/footer") && name.ends_with(".xml"))
}

/// Count each distinct element (by qualified name) in `xml`: total occurrences into `cov.0`, and
/// record presence in `seen` (the caller bumps the per-file count once).
fn scan_elements(xml: &[u8], cov: &mut HashMap<String, (u64, u32)>, seen: &mut HashSet<String>) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                cov.entry(name.clone()).or_default().0 += 1;
                seen.insert(name);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

/// Collect `.docx` files: the path itself if it's a file, else every `.docx` under it (recursive).
fn collect_docx(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if path.is_dir() {
        collect_docx_dir(path, &mut out);
    } else {
        out.push(path.to_path_buf());
    }
    out.retain(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("docx")));
    // Word owner-lock artifacts ("~$name.docx") are not documents - left behind by open/crashed
    // Word sessions, they would show up as unreadable noise in corpus stats.
    out.retain(|p| {
        !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("~$"))
    });
    out
}

fn collect_docx_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_docx_dir(&p, out);
        } else {
            out.push(p);
        }
    }
}
