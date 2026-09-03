//! OOXML <-> Loro mapping - the document model.
//!
//! The collaborative document is a `loro::LoroTree` named [`BLOCKS`]: one node per block. Each
//! paragraph node's meta map carries `type` / an
//! optional `style`, plus a nested `LoroText` container holding the paragraph's run text. Run
//! formatting and tracked changes are **Peritext marks** over that text:
//!
//! - `b` / `i` (bool) - run bold / italic.
//! - `ins` / `del` (JSON string `{author,date,id}`) - a tracked insertion / deletion. Following
//!   OOXML's `w:delText`, a deletion **marks** the text rather than removing it, so accept/reject
//!   is a pure mark resolution.
//!
//! Import (`word/document.xml` -> CRDT) captures the modeled subset and reconstructs a valid,
//! Word-openable `document.xml` from it. This is distinct from `scriptor-ooxml`'s byte-lossless
//! passthrough, which is for documents resolved without entering the CRDT.
//!
//! Fields (`w:fldChar`/`w:instrText`, e.g. a TOC): the outermost field's instruction + cached-result
//! range are preserved (a `fld~{id}` mark + the `fields` map) and re-wrapped on export, so a TOC
//! survives editing and stays updatable in Word; nested fields (PAGEREF inside a TOC) flatten to text.
//! `PAGE`/`NUMPAGES` keep the computed-placeholder path. **Bookmarks** (`bkm~{id}` + the `bookmarks`
//! map) and **hyperlinks** (`lnk~{id}` + the `hyperlinks` map; internal `#anchor` / external URL via
//! the document rels) are likewise modeled + round-trip. v1 limits: a zero-width bookmark (a bare
//! insertion point with no text) is dropped, and a hyperlink is re-emitted per paragraph (it can't
//! cross a paragraph in OOXML anyway).

use std::ops::Range;

use anyhow::{anyhow, Result};
use loro::cursor::{Cursor, Side};
use loro::{Container, ContainerID, ContainerTrait, LoroDoc, LoroMap, LoroText, LoroValue, ToJson,
    TreeID, TreeParentId, ValueOrContainer};
use serde_json::Value as Json;

// The value types, and the leaf helpers everything sits on.
mod types;
mod xml;

// The CRDT side: containers and marks, the read and write paths over them, and the edit operations.
mod containers;
mod export;
mod format;
mod import;
mod read;
mod tracked;

// Part readers. Each owns one OOXML part, or one construct within a part, and depends only on the
// types and XML helpers above, so none of them carry CRDT state.
mod comments;
mod drawings;
mod numbering;
mod passthrough;
mod rels;
mod sections;
mod styles;
mod textboxes;

// Re-exported flat, so callers keep using `model::parse_numbering`, `model::Paragraph` and so on.
pub use comments::*;
pub use containers::*;
pub use drawings::*;
pub use export::*;
pub use format::*;
pub use import::*;
pub use numbering::*;
pub use passthrough::*;
pub use read::*;
pub use rels::*;
pub use sections::*;
pub use styles::*;
pub use textboxes::*;
pub use tracked::*;
pub use types::*;
// Crate-internal helpers the sibling modules share. Kept out of the public surface: `model` is a
// public module, so a plain `pub use` glob here would export the model's own plumbing.
pub(crate) use containers::{append_runs, comment_para_id, mark_fmt_change, mark_track};
pub(crate) use export::raw_attrs;
// Export internals the round-trip tests drive directly, comparing the grid codec against the
// legacy body walk. Not part of the model's surface outside those tests.
#[cfg(test)]
pub(crate) use export::{ExportSpans, IdAlloc, OptSpanGrid, SpanGrid, tbl_xml};
pub(crate) use format::write_para_props;
pub(crate) use read::{
    block_meta_at, meta_bool, meta_string, ordered_roots, raw_root_pos, read_para_props,
    set_or_del_i64, set_para_props_exact, write_para_mark, write_para_prop_change,
};
pub(crate) use styles::parse_border;
pub(crate) use tracked::nth_block_text;
pub(crate) use xml::*;

/// The tree container holding the block hierarchy.
pub const BLOCKS: &str = "blocks";

/// Private-use placeholder chars for computed fields. Import replaces a `PAGE` / `NUMPAGES` field's
/// cached result text with these; the renderer substitutes the live page number / page count per
/// page (so the footer says "3" on page 3, not the value Word happened to cache). Render-only: this
/// is a single text char, so it survives the loro round-trip without a Run-model change.
pub const FIELD_PAGE: char = '\u{E000}';
pub const FIELD_NUMPAGES: char = '\u{E001}';

/// Classify a field instruction (`PAGE`, `NUMPAGES`, ...) to its placeholder char, if computable.
fn field_placeholder(instr: &str) -> Option<char> {
    match instr.split_whitespace().next() {
        Some("PAGE") => Some(FIELD_PAGE),
        Some("NUMPAGES") => Some(FIELD_NUMPAGES),
        _ => None,
    }
}



#[cfg(test)]
mod tests;
