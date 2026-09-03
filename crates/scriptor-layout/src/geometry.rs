//! Laid-out geometry and the queries over it.
//! 
//! [`DocLayout`] is the output of a layout pass without rasterizing: visual lines with
//! their caret stops, block placements, page sizes and per-page fingerprints. Every
//! query the editor needs - hit-testing, caret rectangles, selection rectangles, which
//! page a paragraph is on - is answered from this alone, with no renderer involved.

use crate::*;

/// A caret position within a line: the byte offset (into the paragraph's text) and its x on the
/// canvas (device px). There is one stop before each glyph plus one at the line's end.
#[derive(Debug, Clone, Copy)]
pub struct CaretStop {
    pub byte: usize,
    pub x: f32,
}

/// One laid-out visual line: which paragraph it belongs to, its vertical band on the canvas, and
/// its caret stops (left-to-right). Even an empty paragraph yields one line with a single stop.
#[derive(Debug, Clone)]
pub struct LineBox {
    pub para: usize,
    pub y: f32,
    pub height: f32,
    pub stops: Vec<CaretStop>,
}

impl LineBox {
    pub(crate) fn left(&self) -> f32 {
        // A line with no caret stops (a degenerate empty line) has no x - return a finite 0 rather
        // than ±inf, which would otherwise leak into caret / geometry coordinates (invalid JSON).
        let l = self.stops.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        if l.is_finite() { l } else { 0.0 }
    }
    pub(crate) fn right(&self) -> f32 {
        let r = self.stops.iter().map(|s| s.x).fold(f32::NEG_INFINITY, f32::max);
        if r.is_finite() { r } else { 0.0 }
    }
    pub(crate) fn first_byte(&self) -> usize {
        self.stops.iter().map(|s| s.byte).min().unwrap_or(0)
    }
    pub(crate) fn last_byte(&self) -> usize {
        self.stops.iter().map(|s| s.byte).max().unwrap_or(0)
    }
    /// The x of the caret stop at `byte` (nearest by byte if not exact).
    pub(crate) fn x_at(&self, byte: usize) -> f32 {
        self.stops
            .iter()
            .min_by_key(|s| s.byte.abs_diff(byte))
            .map(|s| s.x)
            .unwrap_or_else(|| self.left())
    }
    /// Horizontal distance from `x` to this line's extent (0 if `x` is within it). Disambiguates
    /// side-by-side table cells that share a row's vertical band during hit-testing.
    pub(crate) fn h_dist(&self, x: f32) -> f32 {
        let (l, r) = (self.left(), self.right());
        if x < l {
            l - x
        } else if x > r {
            x - r
        } else {
            0.0
        }
    }
}

/// Where a block landed: its paragraph index, the page it's on, and its top y **within that page**
/// (px). The driver of incremental painting - [`Renderer::paint_page`] rasterizes only the blocks
/// whose `page` matches.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub block: usize,
    pub page: u32,
    pub y: f32,
}

/// One page's cache key: a fingerprint over everything painted on it (the blocks placed there + the
/// positions they land at). When an edit changes it, that page - and only that page - re-rasterizes.
#[derive(Debug, Clone, Copy)]
pub struct PageInfo {
    pub fingerprint: u64,
}

/// A margin change-bar segment: which page it's on + its page-local vertical band (px), plus the
/// paragraph it belongs to. One is emitted per changed *visual line* (body or table cell) in a
/// markup display mode; [`Renderer::paint_page`] paints each as a thin vertical line set into the
/// left margin. `para` lets the editor map a click on the bar back to its paragraph (click-to-expand
/// in Simple Markup) - it's the same namespaced index the caret geometry uses.
#[derive(Debug, Clone, Copy)]
pub struct ChangeBar {
    pub page: u32,
    pub y: f32,
    pub height: f32,
    pub para: usize,
}

/// A right-margin revision balloon (Word's "Show Revisions in Balloons"): tracked deletions pulled
/// out of the line into a bubble in the balloon band, connected to their paragraph by a thin elbow
/// line. The band is reserved by narrowing the body's content width (so the body shifts left). Built
/// by the wasm layer (it owns the model + colours); [`Renderer::paint_page`] draws the box, the
/// author-coloured struck content (`blocks`), and the connector.
#[derive(Debug, Clone)]
pub struct BalloonPlacement {
    /// Page the balloon (and its anchor) sit on.
    pub page: u32,
    /// The balloon box's top y on the page (after stacking to avoid overlap), page-local px.
    pub y: f32,
    /// The balloon box's total height (content + padding), px.
    pub height: f32,
    /// The y of the body line the balloon annotates (the connector's source), page-local px.
    pub anchor_y: f32,
    /// The balloon's content (the struck, author-coloured deleted text), painted inside the box.
    pub blocks: Vec<Block>,
}


/// The queryable geometry of a laid-out document: visual lines (caret geometry) in document order,
/// block placements (for incremental paint), per-page fingerprints, and page dimensions. Drives
/// hit-testing, caret / selection rectangles, and which pages need repainting. Produced by
/// [`Renderer::layout_doc`] WITHOUT rasterizing - the cheap pass run on every keystroke.
#[derive(Debug, Clone, Default)]
pub struct DocLayout {
    pub lines: Vec<LineBox>,
    pub placements: Vec<Placement>,
    /// Placed table cells (page-local rects + their blocks), painted alongside the paragraph blocks.
    pub cells: Vec<CellPlacement>,
    pub pages: Vec<PageInfo>,
    pub page_width: u32,
    pub page_height: u32,
    pub gap: u32,
    pub total_height: u32,
    /// Left margin + content width (px) - retained so [`Renderer::paint_page`] can place text
    /// without re-deriving the geometry.
    pub margin_left: f32,
    pub content_width: f32,
    /// Margin change-bar segments (one per changed paragraph) + the bar's x / width (device px).
    /// `paint_page` draws them in the left margin. Empty when no tracked changes are shown.
    pub change_bars: Vec<ChangeBar>,
    pub change_bar_x: f32,
    pub change_bar_w: f32,
    /// Width (px) of the right-margin revision-balloon band, reserved by narrowing `content_width`.
    /// Zero when balloons are off. The band occupies `[margin_left + content_width, page_width - mr]`.
    pub balloon_band: f32,
    /// Inline-picture placements (device px, page-local y, page-tagged) reserved in the flow by
    /// [`Renderer::layout_doc`]. `paint_page` composites them over the text (foreground). Empty when
    /// the document has no inline pictures.
    pub inline_images: Vec<PageImage>,
    /// Passthrough placeholder boxes (device px, page-tagged) reserved in the flow, painted by
    /// `paint_page`. Empty when the document has no unmodeled objects.
    pub placeholders: Vec<PagePlaceholder>,
}

impl DocLayout {
    /// The 1-based page number paragraph `para` (flat index) starts on, from the topmost line it
    /// produced. `None` if it laid out no line. Drives table-of-contents page numbers. Line `y` is an
    /// absolute canvas coordinate (pages stacked at `page_index * (page_height + gap)`), so the page is
    /// `floor(y / stride) + 1`.
    pub fn page_of_para(&self, para: usize) -> Option<usize> {
        let line = self.lines.iter().filter(|l| l.para == para).min_by(|a, b| a.y.total_cmp(&b.y))?;
        let stride = self.page_height as f32 + self.gap as f32;
        if stride <= 0.0 {
            return Some(1);
        }
        Some((line.y / stride).floor().max(0.0) as usize + 1)
    }

    /// The line index nearest a canvas y (clamped to the document).
    pub(crate) fn line_at_y(&self, y: f32) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }
        // A line whose vertical band contains y wins; otherwise the vertically-nearest line.
        if let Some(i) = self.lines.iter().position(|l| y >= l.y && y < l.y + l.height) {
            return Some(i);
        }
        let mut best = 0usize;
        let mut best_d = f32::INFINITY;
        for (i, l) in self.lines.iter().enumerate() {
            let center = l.y + l.height / 2.0;
            let d = (center - y).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        Some(best)
    }

    /// Map a canvas point to a caret position `(paragraph, byte)` - the nearest caret stop. Lines
    /// whose vertical band contains `y` are candidates (so a click inside a table row considers every
    /// cell on that row); the candidate nearest `x` horizontally wins, then its nearest stop. With no
    /// band hit (e.g. a click in the gutter below the text) it falls back to the vertically-nearest.
    pub fn hit_test(&self, x: f32, y: f32) -> (usize, usize) {
        if self.lines.is_empty() {
            return (0, 0);
        }
        let mut candidates: Vec<usize> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| y >= l.y && y < l.y + l.height)
            .map(|(i, _)| i)
            .collect();
        if candidates.is_empty() {
            match self.line_at_y(y) {
                Some(i) => candidates.push(i),
                None => return (0, 0),
            }
        }
        let best = *candidates
            .iter()
            .min_by(|&&a, &&b| self.lines[a].h_dist(x).total_cmp(&self.lines[b].h_dist(x)))
            .unwrap();
        let line = &self.lines[best];
        let stop = line
            .stops
            .iter()
            .min_by(|a, b| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
            .copied()
            .unwrap_or(CaretStop { byte: 0, x: line.left() });
        (line.para, stop.byte)
    }

    /// The caret one visual line above (`!down`) or below (`down`) the caret at `(para, byte)`,
    /// keeping the goal column `x` - Word's ArrowUp/Down. Candidates are the lines on the nearest
    /// visual row beyond the current line's midpoint (absolute y, so it crosses paragraph spacing
    /// and page boundaries); rows with several lines (side-by-side table cells) resolve by
    /// horizontal distance to `x`. `None` at the document's first/last line. (A 1px hit-test probe
    /// cannot do this: a probe inside inter-paragraph spacing snaps back to the vertically-nearest
    /// line - the one the caret is already on.)
    pub fn line_step(
        &self,
        para: usize,
        byte: usize,
        x: f32,
        down: bool,
        page_hint: Option<u32>,
    ) -> Option<(usize, usize)> {
        let (cur, _) = self.locate(para, byte, page_hint)?;
        let cm = self.lines[cur].y + self.lines[cur].height / 2.0;
        let mut best: Option<usize> = None;
        let mut best_m = 0.0_f32;
        for (i, l) in self.lines.iter().enumerate() {
            let m = l.y + l.height / 2.0;
            if if down { m <= cm + 0.5 } else { m >= cm - 0.5 } {
                continue;
            }
            let take = match best {
                None => true,
                Some(b) => {
                    let closer = if down { m < best_m - 0.5 } else { m > best_m + 0.5 };
                    let same_row = (m - best_m).abs() <= 0.5;
                    closer || (same_row && l.h_dist(x) < self.lines[b].h_dist(x))
                }
            };
            if take {
                best = Some(i);
                best_m = m;
            }
        }
        let line = &self.lines[best?];
        let stop = line
            .stops
            .iter()
            .min_by(|a, b| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
            .copied()
            .unwrap_or(CaretStop { byte: 0, x: line.left() });
        Some((line.para, stop.byte))
    }

    /// The 0-based page a line sits on (from its absolute y). `y = page * (page_height + gap) +
    /// page-local`, and page-local `< page_height < page_height + gap`, so the floor recovers the page.
    pub(crate) fn line_page(&self, y: f32) -> u32 {
        let denom = (self.page_height + self.gap).max(1) as f32;
        (y / denom).max(0.0) as u32
    }

    /// Locate the line index + x for a caret at `(para, byte)`. `page_hint` disambiguates a paragraph
    /// painted on every page (a header / footer story): when set, a line on that page wins, so the
    /// caret resolves to the instance the user is editing instead of always page 0.
    pub(crate) fn locate(&self, para: usize, byte: usize, page_hint: Option<u32>) -> Option<(usize, f32)> {
        let mut fallback: Option<usize> = None; // first line of this paragraph, any page
        let mut hint_line: Option<usize> = None; // first line of this paragraph on the hinted page
        for (i, l) in self.lines.iter().enumerate() {
            if l.para != para {
                continue;
            }
            fallback.get_or_insert(i);
            let on_hint = page_hint.is_some_and(|ph| self.line_page(l.y) == ph);
            if on_hint {
                hint_line.get_or_insert(i);
            }
            // A byte match wins - but only on the hinted page when a hint is given (so an off-page
            // instance of the same header line can't steal the caret).
            if byte >= l.first_byte() && byte <= l.last_byte() && (on_hint || page_hint.is_none()) {
                return Some((i, l.x_at(byte)));
            }
        }
        // No byte match on the right page: prefer any line on the hinted page, else the first line.
        let pick = hint_line.or(fallback)?;
        Some((pick, self.lines[pick].x_at(byte)))
    }

    /// The caret rectangle `(x, y, height)` for `(para, byte)` (device px). `page_hint` picks the page
    /// instance for a repeated header/footer paragraph (see [`Self::locate`]).
    pub fn caret_rect(&self, para: usize, byte: usize, page_hint: Option<u32>) -> (f32, f32, f32) {
        match self.locate(para, byte, page_hint) {
            Some((i, x)) => (x, self.lines[i].y, self.lines[i].height),
            None => (0.0, 0.0, 0.0),
        }
    }

    /// Selection rectangles `[x, y, w, h, ...]` between two caret positions (order-independent).
    /// `page_hint` picks the page instance for a repeated header/footer paragraph.
    pub fn selection_rects(
        &self,
        p1: usize,
        b1: usize,
        p2: usize,
        b2: usize,
        page_hint: Option<u32>,
    ) -> Vec<f32> {
        let Some((mut sl, mut sx)) = self.locate(p1, b1, page_hint) else { return Vec::new() };
        let Some((mut el, mut ex)) = self.locate(p2, b2, page_hint) else { return Vec::new() };
        if (sl, sx) == (el, ex) {
            return Vec::new(); // collapsed selection -> caret only
        }
        // Order by line, then x.
        if sl > el || (sl == el && sx > ex) {
            std::mem::swap(&mut sl, &mut el);
            std::mem::swap(&mut sx, &mut ex);
        }
        let mut out = Vec::new();
        for i in sl..=el {
            let line = &self.lines[i];
            let x0 = if i == sl { sx } else { line.left() };
            let x1 = if i == el { ex } else { line.right() };
            let w = (x1 - x0).max(1.0);
            out.extend_from_slice(&[x0, line.y, w, line.height]);
        }
        out
    }
}
