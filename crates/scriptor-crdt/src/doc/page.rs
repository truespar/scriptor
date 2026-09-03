//! Page setup and document-level section metadata.
//! 
//! Size, margins, orientation, background and the column layout. None of it is in the
//! op log: this is static metadata read from the section properties and written back
//! out on save.

use crate::*;

impl CollabDoc {
    /// Page size + margins (`w:sectPr`), used to render at the document's real page geometry.
    pub fn page_geometry(&self) -> PageGeometry {
        self.page
    }

    /// Legacy paragraph-spacing mode (`word/settings.xml`): `true` when adjacent space-after +
    /// space-before SUM (Word 2003-and-older / `w:doNotUseHTMLParagraphAutoSpacing`) instead of
    /// consolidating to the max like modern Word. Stamped onto the layout blocks in `relayout`.
    pub fn legacy_para_spacing(&self) -> bool {
        self.legacy_spacing
    }

    /// The page-background fill (`<w:background w:color>`, hex) - `Some` whenever the document
    /// carries one, independent of whether it is displayed.
    pub fn page_background(&self) -> Option<&str> {
        self.background.as_deref()
    }

    /// Whether Word displays the page background (`w:displayBackgroundShape` in settings.xml).
    pub fn page_background_shown(&self) -> bool {
        self.background_shown
    }

    /// Per-section newspaper-column geometry (`w:sectPr/w:cols`), in document order. Empty for a
    /// from-scratch document; populated at import. Drives multi-column page flow in the layout.
    pub fn sections(&self) -> &[SectionCols] {
        &self.sections
    }

    /// Set the page size (twips). Orientation is implied by which dimension is larger.
    pub fn set_page_size(&mut self, width: u32, height: u32) {
        self.page.width = width;
        self.page.height = height;
    }

    /// Set the page margins (twips). The page-setup UI only sets non-negative margins; the model field
    /// is signed (a negative page margin can only arrive via import - see [`PageGeometry`]).
    pub fn set_margins(&mut self, top: u32, right: u32, bottom: u32, left: u32) {
        self.page.margin_top = top as i32;
        self.page.margin_right = right as i32;
        self.page.margin_bottom = bottom as i32;
        self.page.margin_left = left as i32;
    }

    /// Set orientation by ensuring width &lt; height (portrait) or width &gt; height (landscape),
    /// swapping the page dimensions if needed.
    pub fn set_landscape(&mut self, landscape: bool) {
        let (a, b) = (self.page.width, self.page.height);
        let (w, h) = (a.min(b), a.max(b)); // normalize to portrait
        self.page.width = if landscape { h } else { w };
        self.page.height = if landscape { w } else { h };
    }
}
