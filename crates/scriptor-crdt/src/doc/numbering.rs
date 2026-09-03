//! Lists: definitions, and binding paragraphs to them.
//! 
//! A document that never had a list needs one synthesized before a paragraph can join
//! it, which is what `ensure_list` is for.

use crate::*;

impl CollabDoc {
    /// The resolved list numbering definitions (imported `numbering.xml` defs + runtime-synthesized
    /// defs). Reconciles the runtime-synthesized population in from the loro [`model::NUM_SYNTH`] map
    /// first, so a list created earlier this session, restored from the op-log on reopen, or just
    /// arrived from a peer over `merge` is reflected here before any read - `level()`, the marker
    /// computation, and `synth_xml()` all see it without the caller having to remember a refresh step.
    /// Returns a `Ref` guard (the field is interior-mutable); deref for `&Numbering`.
    pub fn numbering(&self) -> std::cell::Ref<'_, Numbering> {
        // Reconcile the loro-backed synth defs into the in-memory view. Use
        // try_borrow_mut: this accessor is called all over the render path
        // (computing list markers), and a NESTED call - while a Ref from an
        // outer numbering() is still alive - would make borrow_mut() PANIC and
        // trap the wasm (frozen editor, the reported "blinking marker on row 1").
        // When the mutable borrow is unavailable an outer call already
        // reconciled this read, and reconcile_synth is idempotent, so skipping is
        // correct. The final borrow() is a shared Ref (any number may coexist).
        if let Ok(mut num) = self.numbering.try_borrow_mut() {
            num.reconcile_synth(&model::read_num_synth(&self.doc));
        }
        self.numbering.borrow()
    }

    /// The `numId` for a list of `fmt` (bullet / decimal / a picked number format) - reusing an existing
    /// definition of that kind (imported or already-synthesized) or synthesizing a fresh one. A
    /// synthesized def's identity (its `numId` + level-0 format) is written into the loro
    /// [`model::NUM_SYNTH`] map, so it persists in the op-log, syncs to peers, and rebuilds on reopen -
    /// the `numbering.xml` patch on save ([`Self::write_numbering_parts`]) is now just one consumer of
    /// that loro-backed state, not its only home. Takes `&self` (interior mutability) so it works on the
    /// live `&CollabDoc` edit path (the agent's `add_list`, the editor's Bullets / Numbering buttons).
    /// Body-level (lists in headers/footers aren't modeled).
    pub fn ensure_list(&self, fmt: model::ListFormat) -> i32 {
        // Reconcile first so we reuse a def created earlier this session / by a peer.
        let synth_map = model::read_num_synth(&self.doc);
        let mut num = self.numbering.borrow_mut();
        num.reconcile_synth(&synth_map);
        if let Some(existing) = num.reusable_num_id(fmt) {
            return existing;
        }
        // Synthesize: assign a fresh high-base id, persist its identity in loro, mirror it in-memory.
        let num_id = num.next_synth_num_id();
        num.insert_synth(num_id, fmt);
        drop(num); // release the borrow before touching loro (no aliasing, but keep it tight)
        let _ = model::write_num_synth(&self.doc, num_id, fmt.level0_numfmt());
        self.doc.commit();
        num_id
    }

    /// Like [`Self::ensure_list`], but ALWAYS mints a FRESH list definition (its
    /// own `numId` + abstract) instead of reusing the document's existing def of
    /// that kind. Because the marker counter is keyed by abstract id, each fresh
    /// def has an INDEPENDENT counter - so two lists authored with `new_list`
    /// each restart at 1, rather than continuing one shared sequence. Use this
    /// for distinct authored lists; `ensure_list` stays the editor's list-toggle
    /// path (which must reuse so repeated toggles don't pile up definitions).
    pub fn new_list(&self, fmt: model::ListFormat) -> i32 {
        // Reconcile so the fresh id lands above any def created earlier this
        // session / by a peer, but DON'T reuse - we want a new instance.
        let synth_map = model::read_num_synth(&self.doc);
        let mut num = self.numbering.borrow_mut();
        num.reconcile_synth(&synth_map);
        let num_id = num.next_synth_num_id();
        num.insert_synth(num_id, fmt);
        drop(num);
        let _ = model::write_num_synth(&self.doc, num_id, fmt.level0_numfmt());
        self.doc.commit();
        num_id
    }

    /// The kind of list paragraph `para` is in: `Some(true)` = bullet, `Some(false)` = numbered,
    /// `None` = not in a list. Lets the toolbar toggle Bullets / Numbering like Word (re-clicking the
    /// active kind clears it; the other kind switches).
    pub fn paragraph_list_kind(&self, para: usize) -> Option<bool> {
        let num_id = self.paragraph_format(para).ok()?.num_id?;
        let l0 = self.numbering().level(num_id, 0).map(|l| l.fmt.clone())?;
        Some(l0 == "bullet")
    }

    /// Paragraph `para`'s list level-0 number format (`"decimal"` / `"lowerRoman"` / `"bullet"` / ...),
    /// or `None` when it isn't in a list - lets the Numbering format picker check the active format.
    pub fn paragraph_list_format(&self, para: usize) -> Option<String> {
        let num_id = self.paragraph_format(para).ok()?.num_id?;
        self.numbering().level(num_id, 0).map(|l| l.fmt.clone())
    }

    /// Set / clear paragraph `para`'s numbering (`w:numPr`) **directly** (no revision). `num_id = None`
    /// removes it from any list.
    pub fn set_numbering(
        &self,
        para: usize,
        num_id: Option<i32>,
        ilvl: Option<i32>,
        audit: &str,
    ) -> Result<()> {
        model::set_numbering(&self.doc, para, num_id, ilvl)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(())
    }

    /// Suggest a numbering change: set / clear paragraph `para`'s list (`w:numPr`) as a tracked
    /// `w:pPrChange` attributed to `author`/`date` (the old style + props are recorded for reject).
    /// Returns the allocated revision id.
    pub fn suggest_numbering(
        &self,
        para: usize,
        num_id: Option<i32>,
        ilvl: Option<i32>,
        author: &str,
        date: &str,
        audit: &str,
    ) -> Result<u64> {
        let id = self.next_revision_id()?;
        model::suggest_numbering(&self.doc, para, num_id, ilvl, author, date, id)?;
        self.doc.set_next_commit_message(audit);
        self.doc.commit();
        Ok(id)
    }

    /// Inject any list definitions synthesized this session (the Bullets / Numbering buttons on a doc
    /// that lacked a suitable list) into `word/numbering.xml`: appended before `</w:numbering>` if the
    /// part exists (imported definitions preserved verbatim), else a fresh part is created + registered
    /// in `[Content_Types].xml` + `document.xml.rels`. A no-op when nothing was synthesized.
    pub(crate) fn write_numbering_parts(&self, parts: &mut Vec<scriptor_ooxml::Part>) {
        // Reconcile the loro-backed synth defs in first (via `numbering()`), so a runtime list reaches
        // the exported `numbering.xml` whether it was created this session OR restored from the op-log.
        let num = self.numbering();
        if !num.has_synth() {
            return;
        }
        let synth = num.synth_xml();
        if let Some(p) = parts.iter_mut().find(|p| p.name == "word/numbering.xml") {
            let mut s = String::from_utf8_lossy(&p.data).into_owned();
            if let Some(pos) = s.rfind("</w:numbering>") {
                s.insert_str(pos, &synth);
                p.data = s.into_bytes();
            }
            return;
        }
        let ns = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
<w:numbering xmlns:w=\"{ns}\">{synth}</w:numbering>"
        );
        set_part(parts, "word/numbering.xml", xml.into_bytes());
        patch_content_types(
            parts,
            "/word/numbering.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
        );
        patch_doc_rels(
            parts,
            "numbering.xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering",
        );
    }
}
