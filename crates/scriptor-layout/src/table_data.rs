//! Table structure handed to the layout pass.
//! 
//! A table arrives as a grid of cells that each carry their own blocks, plus the row
//! heights, spans, borders and shading needed to place and paint it.

use crate::*;

// ── tables ───────────────────────────────────────────────────────────────────

/// One item of the document flow: a paragraph (by index into the paragraph `blocks`) or a table.
/// Laying these out in order reproduces the document (e.g. a contract whose body is a table).
#[derive(Debug, Clone)]
pub enum Content {
    Para(usize),
    Table(TableData),
}

/// A table to lay out: grid column widths (px, as authored - positioned absolutely, never scaled
/// to fit the column) + rows.
#[derive(Debug, Clone)]
pub struct TableData {
    pub col_widths: Vec<f32>,
    /// Alignment within the text column (`w:jc`): 0 = left, 1 = center, 2 = right. Word positions
    /// the grid at its authored width - a narrower table sits by this, a wider one spills the
    /// margins (which is why a 15000-twip table eats into the right margin like Word shows it).
    pub justify: u8,
    pub rows: Vec<RowData>,
    /// Start the table at the top of a new page (a section break or manual page break preceded it).
    /// Mirrors [`Block::page_break_before`] for the table case - paragraph-only break propagation
    /// would otherwise miss a table that immediately follows a break-carrying paragraph.
    pub page_break_before: bool,
}

/// One table row: its cells + an optional explicit height (px). `exact` forces that height; else it
/// is a minimum (the row grows to fit content).
#[derive(Debug, Clone)]
pub struct RowData {
    pub cells: Vec<CellData>,
    pub min_height: f32,
    pub exact: bool,
    /// `w:trPr/w:cantSplit` - the row must paginate whole; without it Word (and this layout) splits
    /// a row whose content exceeds the space left on the page.
    pub cant_split: bool,
}

/// One resolved cell border edge: line width (px) + RGB. Absent edge = no line.
#[derive(Debug, Clone, Copy)]
pub struct CellBorder {
    pub width: f32,
    pub color: [u8; 3],
}

/// The four resolved border edges of a cell (px widths), in paint order top/left/bottom/right.
#[derive(Debug, Clone, Copy, Default)]
pub struct CellEdges {
    pub top: Option<CellBorder>,
    pub left: Option<CellBorder>,
    pub bottom: Option<CellBorder>,
    pub right: Option<CellBorder>,
}

/// A table cell: resolved render blocks, grid span, content margins (px, top/left/bottom/right from
/// `tblCellMar`/`tcMar`), vertical-merge state, and resolved border edges (px).
#[derive(Debug, Clone)]
pub struct CellData {
    pub blocks: Vec<Block>,
    /// Flat paragraph index of each block (parallel to `blocks`) - drives the cell's caret geometry.
    pub para_ids: Vec<usize>,
    pub grid_span: usize,
    pub vmerge_restart: bool,
    pub vmerge_continue: bool,
    pub margins: [f32; 4],
    pub borders: CellEdges,
    pub shading: Option<[u8; 3]>,
}

/// A placed table cell: its rect on a page (page-local px) + content margins + borders + the blocks
/// to paint inside it. Produced by [`Renderer::layout_doc`]; painted by [`Renderer::paint_page`].
#[derive(Debug, Clone)]
pub struct CellPlacement {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub margins: [f32; 4],
    pub borders: CellEdges,
    pub shading: Option<[u8; 3]>,
    pub blocks: Vec<Block>,
    /// Flat paragraph index of each block (parallel to `blocks`).
    pub para_ids: Vec<usize>,
}
