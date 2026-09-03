//! `comments.xml` and `commentsExtended.xml`.
//! 
//! Reads the comment bodies and the thread parent/resolved state Word keeps in the
//! separate extended part, and writes both back out.

use super::*;

/// Parse `word/comments.xml` into comments (body flattened to plain text, `\n` per paragraph). Parent
/// + resolved state come from `commentsExtended` (see [`apply_comments_extended`]).
pub fn parse_comments(xml: &[u8]) -> Vec<ParsedComment> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out: Vec<ParsedComment> = Vec::new();

    let mut cur: Option<Comment> = None;
    let mut para_ids: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut in_para = false;
    let mut capturing = false; // inside a w:t
    let mut cur_text = String::new();

    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                b"w:comment" => {
                    cur = Some(Comment {
                        id: attr(&e, b"w:id").and_then(|s| s.parse().ok()).unwrap_or(0),
                        author: attr(&e, b"w:author").unwrap_or_default(),
                        initials: attr(&e, b"w:initials").unwrap_or_default(),
                        date: attr(&e, b"w:date").unwrap_or_default(),
                        parent: None,
                        resolved: false,
                        text: String::new(),
                    });
                    para_ids.clear();
                    lines.clear();
                }
                b"w:p" if cur.is_some() => {
                    in_para = true;
                    line.clear();
                    if let Some(p) = attr(&e, b"w14:paraId") {
                        para_ids.push(p);
                    }
                }
                b"w:t" if in_para => {
                    capturing = true;
                    cur_text.clear();
                }
                _ => {}
            },
            Event::Text(t) if capturing => {
                if let Ok(s) = t.decode() {
                    cur_text.push_str(&s);
                }
            }
            Event::GeneralRef(r) if capturing => {
                if let Ok(s) = resolve_reference(&r) {
                    cur_text.push_str(&s);
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:t" => {
                    line.push_str(&std::mem::take(&mut cur_text));
                    capturing = false;
                }
                b"w:p" if in_para => {
                    lines.push(std::mem::take(&mut line));
                    in_para = false;
                }
                b"w:comment" => {
                    if let Some(mut c) = cur.take() {
                        c.text = lines.join("\n");
                        if c.initials.is_empty() {
                            c.initials = initials_of(&c.author);
                        }
                        out.push(ParsedComment { comment: c, para_ids: std::mem::take(&mut para_ids) });
                    }
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Derive 1-2 uppercase initials from an author display name (fallback when `w:initials` is absent).
pub fn initials_of(author: &str) -> String {
    author
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// Resolve threading + resolved state from `word/commentsExtended.xml` onto already-parsed comments,
/// matching `w15:paraId` / `w15:paraIdParent` against each comment's body paraIds.
pub fn apply_comments_extended(xml: &[u8], parsed: &mut [ParsedComment]) {
    use quick_xml::events::Event;
    // Map every body paraId -> its comment id (a comment may have several).
    let mut para_to_id: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for p in parsed.iter() {
        for pid in &p.para_ids {
            para_to_id.insert(pid.clone(), p.comment.id);
        }
    }
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        let done_for = |e: &quick_xml::events::BytesStart| {
            let para = attr(e, b"w15:paraId");
            let parent = attr(e, b"w15:paraIdParent");
            let done = matches!(attr(e, b"w15:done").as_deref(), Some("1") | Some("true"));
            (para, parent, done)
        };
        let (para, parent, done) = match ev {
            Event::Eof => break,
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w15:commentEx" => {
                done_for(&e)
            }
            _ => {
                buf.clear();
                continue;
            }
        };
        if let Some(cid) = para.as_deref().and_then(|p| para_to_id.get(p)).copied()
            && let Some(pc) = parsed.iter_mut().find(|pc| pc.comment.id == cid) {
                pc.comment.resolved = done;
                pc.comment.parent = parent.as_deref().and_then(|p| para_to_id.get(p)).copied();
            }
        buf.clear();
    }
}

// ── comments serialization (CRDT -> comments.xml / commentsExtended.xml) ──────

const COMMENTS_HEAD: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n",
    "<w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" ",
    "xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">"
);

/// Serialize the comment list to a valid `word/comments.xml`. Each comment's plain-text body becomes
/// one `<w:p>` per line, with a deterministic `w14:paraId` (line 0 is the comment's canonical id).
pub fn export_comments_xml(comments: &[Comment]) -> String {
    let mut out = String::from(COMMENTS_HEAD);
    for c in comments {
        out.push_str(&format!(
            "<w:comment w:id=\"{id}\" w:author=\"{author}\" w:date=\"{date}\" w:initials=\"{ini}\">",
            id = c.id,
            author = xml_escape(&c.author),
            date = xml_escape(&c.date),
            ini = xml_escape(&c.initials),
        ));
        let lines: Vec<&str> = if c.text.is_empty() { vec![""] } else { c.text.split('\n').collect() };
        for (j, line) in lines.iter().enumerate() {
            out.push_str(&format!(
                "<w:p w14:paraId=\"{pid}\"><w:r><w:t xml:space=\"preserve\">{t}</w:t></w:r></w:p>",
                pid = comment_para_id(c.id, j as u64),
                t = xml_escape(line),
            ));
        }
        out.push_str("</w:comment>");
    }
    out.push_str("</w:comments>");
    out
}

/// Serialize the thread state to a valid `word/commentsExtended.xml` (`w15:commentEx` per comment:
/// its canonical paraId, optional parent paraId, and the resolved flag).
pub fn export_comments_extended_xml(comments: &[Comment]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
<w15:commentsEx xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\">",
    );
    for c in comments {
        out.push_str(&format!("<w15:commentEx w15:paraId=\"{}\"", comment_para_id(c.id, 0)));
        if let Some(p) = c.parent {
            out.push_str(&format!(" w15:paraIdParent=\"{}\"", comment_para_id(p, 0)));
        }
        out.push_str(&format!(" w15:done=\"{}\"/>", if c.resolved { 1 } else { 0 }));
    }
    out.push_str("</w15:commentsEx>");
    out
}
