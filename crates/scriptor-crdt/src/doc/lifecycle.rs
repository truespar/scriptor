//! Construction, history and collaboration sync.
//! 
//! Owns the `LoroDoc` itself: creating an empty document, undo and redo, and the
//! snapshot, merge and delta operations a peer syncs through. `trial_fork` clones the
//! whole thing so a caller can try an edit and throw the result away.

use crate::*;

impl CollabDoc {
    /// A fresh, empty document with its own (random) peer id.
    pub fn new() -> Self {
        let doc = LoroDoc::new();
        configure_marks(&doc);
        let mut undo = UndoManager::new(&doc);
        undo.set_merge_interval(400); // group rapid typing into one undo step (ms)
        let base = StyleTable::word_default();
        Self {
            doc,
            styles: std::cell::RefCell::new(base.clone()),
            styles_base: base,
            styles_dirty: std::cell::Cell::new(false),
            page: PageGeometry::default(),
            legacy_spacing: false,
            background: None,
            background_shown: false,
            sections: Vec::new(),
            hf_docs: std::collections::BTreeMap::new(),
            sections_hf: vec![SectionHf::default()],
            title_pg: false,
            textboxes: Vec::new(),
            source_parts: Vec::new(),
            imported_comments: Vec::new(),
            hf: Vec::new(),
            numbering: std::cell::RefCell::new(Numbering::default()),
            images: Vec::new(),
            pending_media: std::cell::RefCell::new(HashMap::new()),
            undo,
            rev_counter: std::cell::Cell::new(None),
        }
    }

    /// The document body in order (paragraph markers + tables), **derived** from the loro block tree:
    /// each top-level paragraph node is a `Paragraph` marker, each `type "table"` node is read from its
    /// hosted [`TableGrid`](crate::table_crdt::TableGrid) via [`model::grid_to_table`]. Table structure
    /// is now a loro citizen (tables-crdt T2.7), so there is no stored `Vec<BodyItem>`; this projection
    /// is what the renderer + the flat-index locator consume (its visible-cell flat walk lines up with
    /// [`paragraphs`](Self::paragraphs)). For a table-free document this is a flat list of `Paragraph`s.
    pub fn body(&self) -> Vec<model::BodyItem> {
        model::node_body(&self.doc)
    }

    /// Undo the last local edit. Returns whether anything changed (caller re-renders).
    pub fn undo(&mut self) -> Result<bool> {
        // An undo may revert a style-definition edit in STYLE_OVERRIDES; force the next styles() read
        // to rebuild the effective table from the (now-reverted) override set.
        self.styles_dirty.set(true);
        self.undo.undo().map_err(|e| anyhow::anyhow!("undo: {e}"))
    }

    /// Redo the last undone edit. Returns whether anything changed.
    pub fn redo(&mut self) -> Result<bool> {
        self.styles_dirty.set(true);
        self.undo.redo().map_err(|e| anyhow::anyhow!("redo: {e}"))
    }

    /// Whether there is anything to undo / redo.
    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }

    /// Drop the undo/redo history (used to baseline a freshly loaded document so the load itself
    /// isn't undoable).
    pub fn clear_undo(&self) {
        self.undo.clear();
    }

    /// Export a full, self-contained snapshot (history + state). Importing it into another
    /// `CollabDoc` merges the two histories. Production sync uses version-vector diffs
    /// (`ExportMode::Updates`) over the wire; a snapshot is the simplest correct merge unit.
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::Snapshot)
            .map_err(|e| anyhow::anyhow!("export snapshot: {e}"))
    }

    /// Merge another document's exported bytes (snapshot or updates) into this one.
    pub fn merge(&self, bytes: &[u8]) -> Result<()> {
        self.doc.import(bytes).map_err(|e| anyhow::anyhow!("import: {e}"))?;
        // The merged ops may include a peer's style-definition edits (STYLE_OVERRIDES); force the
        // next styles() read to fold them into the effective table.
        self.styles_dirty.set(true);
        Ok(())
    }

    /// The current version (all committed ops). Capture before a mutation and
    /// pair with [`Self::export_updates_since`] for the delta. See [`DocVersion`].
    pub fn version(&self) -> DocVersion {
        DocVersion(self.doc.oplog_vv())
    }

    /// Export just the ops committed since `from` as a loro update blob (an
    /// `ExportMode::Updates` delta). Pair with [`Self::version`] captured before
    /// the mutation; the result merges into any peer at-or-after `from`. This is
    /// the incremental wire unit for a collaborative relay - far smaller than a
    /// full [`Self::snapshot`] when only a few ops changed.
    ///
    /// All `CollabDoc` mutations commit their ops, so a delta taken right after a
    /// mutation reflects it. A no-op interval yields loro's minimal empty-update
    /// blob (merging it is a harmless idempotent import).
    pub fn export_updates_since(&self, from: &DocVersion) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::Updates { from: std::borrow::Cow::Borrowed(&from.0) })
            .map_err(|e| anyhow::anyhow!("export updates: {e}"))
    }

    /// Attach the original `.docx`'s parts as this document's `source_parts`.
    ///
    /// The loro snapshot ([`Self::snapshot`]) carries the modeled content +
    /// structure but **not** `source_parts` - the verbatim-passthrough parts
    /// (theme / settings / fontTable / customXml / docProps, ...) are a Rust
    /// field, not loro-backed. So a document reopened via [`Self::new`] +
    /// [`Self::merge`] (e.g. a server reloading from a persisted op-log) has no
    /// original parts, and [`Self::to_docx_bytes`] then rebuilds a minimal
    /// `.docx`, losing that passthrough. Reattaching the parts from the
    /// immutable origin `.docx` restores Word-perfect export. The modeled parts
    /// (document.xml / styles / numbering / headers / footers) are still
    /// re-rendered from the live model on save, so this only restores the
    /// verbatim remainder.
    pub fn set_source_parts(&mut self, parts: Vec<scriptor_ooxml::Part>) {
        self.source_parts = parts;
    }

    /// Convenience over [`Self::set_source_parts`]: read the parts from an origin
    /// `.docx`'s bytes and attach them.
    pub fn attach_source_parts_from_docx(&mut self, origin_docx: &[u8]) -> Result<()> {
        self.source_parts = scriptor_ooxml::read_parts_bytes(origin_docx)?;
        Ok(())
    }

    /// A snapshot-isolated copy of this document at its current state, for **trial application**: apply
    /// a batch of edits to the fork and, only if every one succeeds, apply it to the original (so a
    /// mid-batch apply-time failure never leaves the original partially edited). It shares this
    /// document's loro history, so an [`Anchor`] (a history-bound cursor) resolves identically in the
    /// fork - the proposal's anchors address the same content. The in-memory body (table structure) is
    /// cloned too, so container-boundary checks behave the same. Not a general-purpose clone: it carries
    /// no styles / numbering / header-footer / source parts (none of which an edit batch mutates) and
    /// has its own fresh peer id + undo history; never export the fork as the document.
    pub fn trial_fork(&self) -> Result<Self> {
        let me = Self::new();
        me.doc.import(&self.snapshot()?).map_err(|e| anyhow::anyhow!("trial fork import: {e}"))?;
        // Table structure rides in the imported snapshot (loro table nodes + grids), so the fork's
        // `body()` derives correctly - no separate clone needed.
        me.reconfigure_comment_marks(); // re-register cmt~/fld~/bkm~/lnk~ mark keys from the imported state
        me.clear_undo();
        Ok(me)
    }
}
