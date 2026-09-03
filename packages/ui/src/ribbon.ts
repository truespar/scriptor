import type {
  CompareResult,
  ScriptorView,
  StyleEdit,
  StyleGalleryItem,
  TrackDisplay,
} from '@truespar/scriptor-core';
import { type CompareSources, type CompareVersion, openCompareDialog } from './compare-dialog';
import {
  col,
  dropdown,
  group,
  h,
  icon,
  iconBtn,
  injectStyles,
  labeled,
  makeRibbonResponsive,
  openFlyout,
  row,
  sep,
  splitBtn,
  svgBtn,
  toggle,
} from './dom';

/** Measurement units: US (inch) or EU (metric mm). Affects the ruler + page presets. */
export type Units = 'us' | 'eu';

export interface RibbonOptions {
  /** Initial units. Default `'us'`. */
  units?: Units;
  /** Called when the user switches units (the ruler/host can react). */
  onUnits?: (units: Units) => void;
  /** Called when the user toggles the reviewing pane (Review tab). When omitted, no toggle is shown. */
  onReviewingPane?: () => void;
  /** Called with the redline + manifest after the user runs a comparison (Review tab → Compare). The
   *  host decides what to do with it - the [`Workspace`] opens the redline in its view. When omitted,
   *  no Compare button is shown. */
  onCompare?: (result: CompareResult, sources: CompareSources) => void;
  /** Toggle the side-by-side comparison view (original | revised, scroll-locked). Enabled by the host
   *  (via [`Ribbon.setSideBySide`]) only once a comparison has run. */
  onSideBySide?: () => void;
  /** Optional source of named documents (e.g. saved versions) the Compare dialog offers for either
   *  side, in addition to the current document and a file - the version-aware compare path. */
  compareVersions?: () => CompareVersion[] | Promise<CompareVersion[]>;
  /** Show the Quick-Access "Save" button. Default `true`. Set `false` when the
   *  host persists continuously (e.g. live collaboration), where a manual Save
   *  is misleading - `view.save()` there just downloads a `.docx` copy. */
  showSave?: boolean;
}

// Family names the user can pick. Each is an MS family the engine renders via a bundled
// metric-compatible clone (Calibri->Carlito, Cambria->Caladea, ...); the label is the MS name, not
// the clone. The legal serifs (Century Schoolbook / Book Antiqua / Palatino Linotype / Bookman Old
// Style) map to the GUST TeX Gyre trio (Schola/Pagella/Bonum) - see scriptor-fonts substitute_family.
const FONTS = [
  'Calibri',
  'Cambria',
  'Arial',
  'Times New Roman',
  'Georgia',
  'Century Schoolbook',
  'Book Antiqua',
  'Palatino Linotype',
  'Bookman Old Style',
  'Courier New',
  'Verdana',
  'Tahoma',
];

/** A font-dropdown item that previews its label in the family itself. scriptor-core registers each MS
 *  family as a DOM `@font-face` backed by its bundled clone, so `font-family:'Cambria'` renders in
 *  Caladea - the same face the canvas paints. `system-ui` is the graceful fallback for any unmapped
 *  name (e.g. Verdana/Tahoma, which have no bundled clone yet). */
const fontItem = (f: string): { value: string; label: string; font: string } => ({
  value: f,
  label: f,
  font: `'${f.replace(/'/g, '')}', system-ui, sans-serif`,
});
const SIZES = [8, 9, 10, 10.5, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 36, 48, 72];
const LINE_SPACINGS: [string, number][] = [
  ['1.0', 1],
  ['1.15', 1.15],
  ['1.5', 1.5],
  ['2.0', 2],
];
const PAGE_SIZES: Record<Units, [string, number, number][]> = {
  us: [
    ['Letter', 12240, 15840],
    ['Legal', 12240, 20160],
    ['A4', 11906, 16838],
  ],
  eu: [
    ['A4', 11906, 16838],
    ['A5', 8391, 11906],
    ['Letter', 12240, 15840],
  ],
};
const MARGINS: [string, number, number, number, number][] = [
  ['Normal', 1440, 1440, 1440, 1440],
  ['Narrow', 720, 720, 720, 720],
  ['Moderate', 1440, 1080, 1440, 1080],
  ['Wide', 1440, 2880, 1440, 2880],
];

/** Word's text-highlight palette: `[display name, OOXML w:highlight value, CSS swatch color]`. These
 *  are the only colors `w:highlight` allows (named, not arbitrary RGB), matching Word's highlighter. */
const HIGHLIGHT_COLORS: [string, string, string][] = [
  ['Yellow', 'yellow', '#ffff00'],
  ['Bright Green', 'green', '#00ff00'],
  ['Turquoise', 'cyan', '#00ffff'],
  ['Pink', 'magenta', '#ff00ff'],
  ['Blue', 'blue', '#0000ff'],
  ['Red', 'red', '#ff0000'],
  ['Dark Blue', 'darkBlue', '#000080'],
  ['Teal', 'darkCyan', '#008080'],
  ['Green', 'darkGreen', '#008000'],
  ['Violet', 'darkMagenta', '#800080'],
  ['Dark Red', 'darkRed', '#800000'],
  ['Dark Yellow', 'darkYellow', '#808000'],
  ['Gray 50%', 'darkGray', '#808080'],
  ['Gray 25%', 'lightGray', '#c0c0c0'],
  ['Black', 'black', '#000000'],
  ['White', 'white', '#ffffff'],
];

/** Two dimensions (twips) match within ~3mm - paper sizes from different apps vary by a few twips. */
function dimsNear(a: number, b: number): boolean {
  return Math.abs(a - b) <= 40;
}

/** Infer the measurement unit from the document's paper size: metric (A4 / A5) -> mm, else inch. So
 *  opening an A4 doc shows mm + an "A4" label instead of defaulting to US/Letter. */
export function inferUnits(view: ScriptorView): Units {
  const g = view.pageGeometry();
  const w = Math.min(g.width, g.height);
  const h = Math.max(g.width, g.height);
  const isA4 = dimsNear(w, 11906) && dimsNear(h, 16838);
  const isA5 = dimsNear(w, 8391) && dimsNear(h, 11906);
  return isA4 || isA5 ? 'eu' : 'us';
}

/**
 * The default Word-style chrome for Scriptor: a tabbed ribbon (Home + Layout) bound to a
 * [`ScriptorView`]. It only dispatches the view's commands and reflects its state - all document
 * logic stays in the core - so apps can use it as-is or skip it and drive the view themselves.
 */
export class Ribbon {
  readonly element: HTMLElement;
  private readonly view: ScriptorView;
  private readonly opts: Required<Pick<RibbonOptions, 'units'>> &
    Pick<
      RibbonOptions,
      'onUnits' | 'onReviewingPane' | 'onCompare' | 'onSideBySide' | 'compareVersions'
    >;
  /** The "Side by side" toggle button, once built - so [`setSideBySide`] can enable/press it. */
  private sideBySideBtn: HTMLButtonElement | null = null;
  private units: Units;
  /** Once the user picks a unit from the dropdown, stop auto-following the document's paper size. */
  private unitsLocked = false;
  private readonly unsub: () => void;
  private refreshUi: () => void = () => {};
  private refreshLayout: (() => void) | null = null;
  private refreshReview: (() => void) | null = null;
  private refreshInsert: (() => void) | null = null;

  constructor(container: HTMLElement, view: ScriptorView, options: RibbonOptions = {}) {
    this.view = view;
    // Default the measurement unit to the document's paper size (A4 -> mm) unless the host forces one.
    this.units = options.units ?? inferUnits(view);
    this.opts = {
      units: this.units,
      onUnits: options.onUnits,
      onReviewingPane: options.onReviewingPane,
      onCompare: options.onCompare,
      onSideBySide: options.onSideBySide,
      compareVersions: options.compareVersions,
    };
    injectStyles();

    this.element = h('div', { class: 'scr-ribbon' });
    const tabs = h('div', { class: 'scr-tabs' });
    const panels = h('div', { class: 'scr-panels' });
    this.element.append(tabs, panels);
    container.append(this.element);

    // Quick Access (Word's top-left): save, undo / redo, left of the tabs. The
    // Save button is omitted when the host persists continuously (showSave:false)
    // - there a manual Save is misleading (view.save() only downloads a copy).
    const qat = h('div', { class: 'scr-qat' });
    if (options.showSave !== false) {
      const saveBtn = svgBtn('save', 'Save (Ctrl+S)', '', () => view.save());
      qat.append(saveBtn);
    }
    const undoBtn = svgBtn('undo', 'Undo (Ctrl+Z)', '', () => view.undo());
    const redoBtn = svgBtn('redo', 'Redo (Ctrl+Y)', '', () => view.redo());
    qat.append(undoBtn, redoBtn);
    tabs.append(qat);

    const home = this.buildHome();
    const insert = this.buildInsert();
    const layout = this.buildLayout();
    const references = this.buildReferences();
    const review = this.buildReview();
    panels.append(home, insert, layout, references, review);

    const tabDefs: [string, HTMLElement | null][] = [
      ['Home', home],
      ['Insert', insert],
      ['Layout', layout],
      ['References', references],
      ['Review', review],
      ['View', null],
    ];
    const tabBtns: HTMLButtonElement[] = [];
    const show = (panel: HTMLElement | null, btn: HTMLButtonElement) => {
      if (!panel) return;
      for (const p of [home, insert, layout, references, review])
        p.classList.toggle('scr-hidden', p !== panel);
      for (const b of tabBtns) b.classList.toggle('active', b === btn);
    };
    for (const [name, panel] of tabDefs) {
      const btn = h('button', {
        class: `scr-tab${panel ? '' : ' scr-tab-disabled'}`,
        type: 'button',
        title: panel ? name : `${name} - coming soon`,
      }) as HTMLButtonElement;
      btn.textContent = name;
      if (panel) btn.addEventListener('click', () => show(panel, btn));
      else btn.disabled = true;
      tabBtns.push(btn);
      tabs.append(btn);
    }
    show(home, tabBtns[0]!);

    // Word-style responsive collapse: each panel tucks overflowing groups behind a "More" button when
    // the window is too narrow, so the (fixed-height) ribbon never scrolls or reflows vertically.
    for (const p of [home, insert, layout, references, review]) makeRibbonResponsive(p);

    const refreshAll = () => {
      this.refreshUi();
      this.refreshReview?.();
      this.refreshInsert?.();
      undoBtn.disabled = !view.canUndo();
      redoBtn.disabled = !view.canRedo();
    };
    this.unsub = view.addListener(refreshAll);
    refreshAll();
  }

  /** Tear down: unsubscribe + remove the ribbon DOM. */
  destroy(): void {
    this.unsub();
    this.element.remove();
  }

  private buildHome(): HTMLElement {
    const panel = h('div', { class: 'scr-panel' });

    // Clipboard group (Word's leftmost group): Paste, then Cut / Copy. Plain text for now.
    const paste = svgBtn(
      'paste',
      'Paste',
      'scr-paste',
      () => {
        navigator.clipboard
          .readText()
          .then((t) => this.view.replaceSelection(t))
          .catch(() => {});
      },
      { size: 22, label: 'Paste' },
    );
    const cut = svgBtn('cut', 'Cut', '', () => {
      const t = this.view.selectionText();
      if (!t) return;
      navigator.clipboard.writeText(t).catch(() => {});
      this.view.replaceSelection('');
    });
    const copy = svgBtn('copy', 'Copy', '', () => {
      const t = this.view.selectionText();
      if (t) navigator.clipboard.writeText(t).catch(() => {});
    });
    // Word's Clipboard layout: a tall Paste on the left, Cut over Copy stacked to its right.
    const clipboardGroup = group('Clipboard', [row([paste, col([cut, copy])])]);

    // Font group. Family + size are styled dropdowns (size is a typable combo, like Word).
    const family = dropdown({
      items: FONTS.map(fontItem),
      value: FONTS[0],
      title: 'Font',
      className: 'scr-family',
      onChange: (v) => this.view.setFontFamily(v),
    });
    const size = dropdown({
      items: SIZES.map((sz) => ({ value: String(sz), label: String(sz) })),
      title: 'Font size',
      className: 'scr-size',
      editable: true,
      onChange: (v) => {
        const n = Number(v);
        if (n > 0) this.view.setFontSize(n);
      },
    });

    const b = iconBtn('B', 'Bold (Ctrl+B)', 'scr-b', () => this.view.toggleBold());
    const i = iconBtn('I', 'Italic (Ctrl+I)', 'scr-i', () => this.view.toggleItalic());
    const u = iconBtn('U', 'Underline (Ctrl+U)', 'scr-u', () => this.view.toggleUnderline());
    const s = iconBtn('S', 'Strikethrough', 'scr-s', () => this.view.toggleStrike());

    const color = h('input', {
      class: 'scr-color',
      type: 'color',
      title: 'Text color',
      value: '#1a1a1a',
    }) as HTMLInputElement;
    color.addEventListener('input', () => this.view.setColor(color.value));

    // Grow / Shrink font (Word's A▲ / A▼) - step up/down the size ladder.
    const grow = svgBtn('plus', 'Grow font', '', () => this.view.growFont());
    const shrink = svgBtn('minus', 'Shrink font', '', () => this.view.shrinkFont());
    const clearFmt = svgBtn('eraser', 'Clear all formatting', '', () =>
      this.view.clearFormatting(),
    );

    // Super / subscript (the rest of Word's Font group).
    const superscript = svgBtn('superscript', 'Superscript', 'scr-sup', () =>
      this.view.toggleSuperscript(),
    );
    const subscript = svgBtn('subscript', 'Subscript', 'scr-sub', () =>
      this.view.toggleSubscript(),
    );

    // Text highlight: a Word-style split button. The main button applies the last-used color; the
    // caret drops a swatch palette (+ No Color). Picking a swatch both applies it and remembers it.
    let lastHl = 'yellow';
    const highlight = svgBtn('highlight', 'Text highlight color', 'scr-hl scr-split-main', () =>
      this.view.setHighlight(lastHl),
    );
    const setHlSwatch = (name: string): void => {
      const c = HIGHLIGHT_COLORS.find(([, v]) => v === name)?.[2] ?? '#ffff00';
      highlight.style.borderBottom = `3px solid ${c}`; // Word's color bar under the highlighter
    };
    setHlSwatch(lastHl);
    const hlCaret = h('button', {
      class: 'scr-btn scr-split-caret',
      type: 'button',
      title: 'Highlight color',
    }) as HTMLButtonElement;
    hlCaret.append(icon('caretDown', 12));
    let hlFlyoutClose: (() => void) | null = null;
    hlCaret.addEventListener('click', (e) => {
      e.stopPropagation();
      if (hlFlyoutClose) {
        hlFlyoutClose();
        return;
      }
      const menu = h('div', { class: 'scr-menu scr-hl-menu' });
      const grid = h('div', { class: 'scr-hl-grid' });
      for (const [label, name, css] of HIGHLIGHT_COLORS) {
        const sw = h('button', {
          class: 'scr-hl-swatch',
          type: 'button',
          title: label,
        }) as HTMLButtonElement;
        sw.style.background = css;
        if (name === lastHl) sw.classList.add('active');
        sw.addEventListener('click', () => {
          hlFlyoutClose?.();
          lastHl = name;
          setHlSwatch(name);
          this.view.setHighlight(name);
        });
        grid.append(sw);
      }
      const none = h('button', {
        class: 'scr-hl-none scr-menu-item',
        type: 'button',
      }) as HTMLButtonElement;
      none.textContent = 'No Color';
      none.addEventListener('click', () => {
        hlFlyoutClose?.();
        this.view.setHighlight('none');
      });
      menu.append(grid, none);
      hlFlyoutClose = openFlyout(hlCaret, menu, () => {
        hlFlyoutClose = null;
      });
    });
    const highlightSplit = h('div', { class: 'scr-split' });
    highlightSplit.append(highlight, hlCaret);

    const fontGroup = group('Font', [
      row([family.el, size.el, grow, shrink, clearFmt]),
      row([b, i, u, s, superscript, subscript, highlightSplit, color]),
    ]);

    // Paragraph group.
    const alignL = svgBtn('alignLeft', 'Align left', '', () => this.view.setAlignment('left'));
    const alignC = svgBtn('alignCenter', 'Center', '', () => this.view.setAlignment('center'));
    const alignR = svgBtn('alignRight', 'Align right', '', () => this.view.setAlignment('right'));
    const alignJ = svgBtn('alignJustify', 'Justify', '', () => this.view.setAlignment('justify'));

    const spacing = dropdown({
      items: LINE_SPACINGS.map(([label, mult]) => ({ value: String(mult), label })),
      value: '1',
      title: 'Line spacing',
      width: '64px',
      onChange: (v) => this.view.setLineSpacing(Number(v)),
    });

    const outdent = svgBtn('outdent', 'Decrease indent', '', () => this.view.indent(-720));
    const indent = svgBtn('indent', 'Increase indent', '', () => this.view.indent(720));

    // Lists: Bullets (icon) toggles on/off. Numbering is a split control - the "1." glyph toggles a
    // decimal list; its caret drops a format picker (1. / a. / A. / i. / I.) like Word's Numbering gallery.
    const bullets = svgBtn('list', 'Bullets', 'scr-bullets', () => this.view.toggleBullets());
    const numbering = iconBtn('1.', 'Numbering', 'scr-numbering scr-split-main', () =>
      this.view.toggleNumbering(),
    );
    const NUM_FORMATS: [string, string][] = [
      ['decimal', '1.  2.  3.'],
      ['lowerLetter', 'a.  b.  c.'],
      ['upperLetter', 'A.  B.  C.'],
      ['lowerRoman', 'i.  ii.  iii.'],
      ['upperRoman', 'I.  II.  III.'],
    ];
    const numCaret = h('button', {
      class: 'scr-btn scr-split-caret',
      type: 'button',
      title: 'Numbering format',
    }) as HTMLButtonElement;
    numCaret.append(icon('caretDown', 12));
    let numFlyoutClose: (() => void) | null = null;
    numCaret.addEventListener('click', (e) => {
      e.stopPropagation();
      if (numFlyoutClose) {
        numFlyoutClose();
        return;
      }
      const active = this.view.currentListFormat();
      const menu = h('div', { class: 'scr-menu' });
      for (const [numFmt, sample] of NUM_FORMATS) {
        const item = h('button', { class: 'scr-menu-item', type: 'button' }) as HTMLButtonElement;
        const s = h('span', { class: 'scr-num-sample' });
        s.textContent = sample;
        item.append(s);
        if (numFmt === active) item.classList.add('active');
        item.addEventListener('click', () => {
          numFlyoutClose?.();
          this.view.applyListFormat(numFmt);
        });
        menu.append(item);
      }
      numFlyoutClose = openFlyout(numCaret, menu, () => {
        numFlyoutClose = null;
      });
    });
    const numberingSplit = h('div', { class: 'scr-split' });
    numberingSplit.append(numbering, numCaret);

    const paraGroup = group('Paragraph', [
      row([bullets, numberingSplit, outdent, indent]),
      row([alignL, alignC, alignR, alignJ, spacing.el]),
    ]);

    // Styles gallery: a button that drops a flyout of live-preview cards - each quick style's name
    // (Title / Heading / Subtitle / Normal / ...) rendered in its own look (font / size / weight /
    // colour). Clicking a card applies it to the caret paragraph (tracked as a `w:pPrChange` when
    // Track-Changes is on). The trigger's label reflects the caret paragraph's current style.
    const styleBtn = h('button', {
      class: 'scr-btn scr-style-trigger',
      type: 'button',
      title: 'Paragraph style',
    }) as HTMLButtonElement;
    const styleBtnLabel = h('span', { class: 'scr-btn-label' });
    styleBtnLabel.textContent = 'Styles';
    styleBtn.append(styleBtnLabel, icon('caretDown', 14));

    // A gallery entry's resolved props -> inline CSS for its preview card. Point sizes are clamped to a
    // compact range so the popup stays tidy while still conveying the hierarchy (Title big, body small).
    const previewCss = (s: StyleGalleryItem): string => {
      const css: string[] = [];
      if (s.size) css.push(`font-size:${Math.max(12, Math.min(22, s.size / 2))}px`);
      if (s.bold) css.push('font-weight:700');
      if (s.italic) css.push('font-style:italic');
      if (s.color) css.push(`color:#${s.color}`);
      if (s.font) css.push(`font-family:'${s.font.replace(/'/g, '')}', system-ui, sans-serif`);
      return css.join(';');
    };

    let styleFlyoutClose: (() => void) | null = null;
    const openStyleGallery = (): void => {
      if (styleFlyoutClose) {
        styleFlyoutClose(); // re-click toggles it shut (onClose clears the handle)
        return;
      }
      const gallery = this.view.styleGallery();
      if (!gallery.length) return;
      const cur = this.view.currentStyle() || 'Normal';
      const panel = h('div', { class: 'scr-style-gallery' });
      for (const s of gallery) {
        const card = h('button', {
          class: 'scr-style-card',
          type: 'button',
          title: `${s.name} - right-click to modify`,
        }) as HTMLButtonElement;
        if (s.id === cur) card.classList.add('active');
        const nm = h('span', { class: 'scr-style-card-name' });
        nm.textContent = s.name;
        nm.setAttribute('style', previewCss(s));
        card.append(nm);
        card.addEventListener('click', () => {
          styleFlyoutClose?.();
          this.view.setParagraphStyle(s.id);
        });
        // Right-click a card to edit its *definition* (Word's Modify-Style: change what the style is,
        // so every paragraph using it updates), rather than applying it to the caret paragraph.
        card.addEventListener('contextmenu', (e) => {
          e.preventDefault();
          styleFlyoutClose?.();
          this.openModifyStyle(s.id, s.name);
        });
        panel.append(card);
      }
      // A discoverable footer for the Modify + Update-to-Match actions on the caret paragraph's
      // current style (Word's right-click "Modify…" / "Update … to Match Selection").
      const curName = gallery.find((s) => s.id === cur)?.name ?? cur;
      const modify = h('button', {
        class: 'scr-style-modify',
        type: 'button',
      }) as HTMLButtonElement;
      modify.textContent = `Modify “${curName}”…`;
      modify.addEventListener('click', () => {
        styleFlyoutClose?.();
        this.openModifyStyle(cur, curName);
      });
      panel.append(modify);
      const update = h('button', {
        class: 'scr-style-modify',
        type: 'button',
      }) as HTMLButtonElement;
      update.textContent = `Update “${curName}” to Match Selection`;
      update.title = "Make this style adopt the caret paragraph's current formatting";
      update.addEventListener('click', () => {
        styleFlyoutClose?.();
        this.view.updateStyleToMatchSelection();
      });
      panel.append(update);
      const create = h('button', {
        class: 'scr-style-modify',
        type: 'button',
      }) as HTMLButtonElement;
      create.textContent = 'New Style from Selection…';
      create.title = "Save the selection's formatting as a new paragraph style";
      create.addEventListener('click', () => {
        styleFlyoutClose?.();
        this.openNewStyle();
      });
      panel.append(create);
      styleFlyoutClose = openFlyout(styleBtn, panel, () => {
        styleFlyoutClose = null;
      });
    };
    styleBtn.addEventListener('click', openStyleGallery);
    const stylesGroup = group('Styles', [row([styleBtn])]);

    // Word's Home order, left to right: Clipboard | Font | Paragraph | Styles. (Hyperlink + Bookmark
    // moved to Insert, Table of Contents to References - matching Word's tab layout.)
    panel.append(clipboardGroup, sep(), fontGroup, sep(), paraGroup, sep(), stylesGroup);

    this.refreshUi = () => {
      panel.classList.toggle('scr-readonly', this.view.mode === 'read');
      const f = this.view.selectionFormat();
      toggle(b, f?.bold === true);
      toggle(i, f?.italic === true);
      toggle(u, f?.underline === true);
      toggle(s, f?.strike === true);
      toggle(superscript, f?.vertAlign === 'superscript');
      toggle(subscript, f?.vertAlign === 'subscript');
      toggle(highlight, !!f?.highlight);
      // Font / size now resolve through the paragraph style + doc default (see `selection_format`), so
      // a normal (un-overridden) selection still reports its rendered font + size - reflect them. The
      // styled dropdown just shows whatever value the doc reports (preset or not), no option list to seed.
      family.setValue(f?.font ?? '');
      size.setValue(f?.size ? String(f.size / 2) : '');
      const p = this.view.paragraphFormat();
      toggle(alignL, p?.align === 'left' || (!!p && !p.align));
      toggle(alignC, p?.align === 'center');
      toggle(alignR, p?.align === 'right');
      toggle(alignJ, p?.align === 'justify');
      if (p?.lineSpacing) spacing.setValue(String(p.lineSpacing / 240));
      // List toggle state (Bullets / Numbering) reflects the caret paragraph's list kind.
      const listKind = this.view.currentListKind();
      toggle(bullets, listKind === 'bullet');
      toggle(numbering, listKind === 'number');
      // Styles trigger label reflects the caret paragraph's current style (its display name); the gallery
      // itself (with the active card) is built fresh each time it opens.
      const curStyle = this.view.currentStyle() || 'Normal';
      styleBtnLabel.textContent =
        this.view.styleGallery().find((s) => s.id === curStyle)?.name ?? curStyle;
      this.refreshLayout?.();
    };

    return panel;
  }

  /** Insert tab: Links group (Hyperlink + Bookmark - Word's Insert > Links). */
  private buildInsert(): HTMLElement {
    const panel = h('div', { class: 'scr-panel scr-hidden' });
    const picture = svgBtn(
      'image',
      'Insert a picture from this device',
      '',
      () => void this.view.insertPicture(),
      {
        size: 22,
        label: 'Pictures',
      },
    );
    // Crop is contextual: enabled only when a picture is selected, pressed while crop mode is active.
    const crop = svgBtn('crop', 'Crop the selected picture', '', () => this.view.toggleCrop(), {
      label: 'Crop',
    });
    // Wrap Text: how the selected picture sits relative to the text (Word's wrapping menu).
    const wrap = dropdown({
      items: [
        { value: 'inline', label: 'In Line with Text' },
        { value: 'square', label: 'Square' },
        { value: 'tight', label: 'Tight' },
        { value: 'through', label: 'Through' },
        { value: 'topAndBottom', label: 'Top and Bottom' },
        { value: 'behind', label: 'Behind Text' },
        { value: 'front', label: 'In Front of Text' },
      ],
      value: 'inline',
      title: 'Wrap Text',
      width: '150px',
      onChange: (v) => this.view.setImageWrap(v),
    });
    this.refreshInsert = () => {
      const selected = this.view.selectedImageId !== null;
      crop.disabled = !selected;
      crop.classList.toggle('active', this.view.cropActive);
      wrap.el.style.opacity = selected ? '1' : '0.5';
      wrap.el.style.pointerEvents = selected ? '' : 'none';
      wrap.setValue(this.view.selectedImageWrap ?? 'inline');
    };
    const link = svgBtn('link', 'Hyperlink (Ctrl+K)', '', () => void this.view.addHyperlink(), {
      label: 'Link',
    });
    const bookmark = svgBtn('bookmark', 'Bookmark', '', () => void this.view.addBookmark(), {
      label: 'Bookmark',
    });
    // Header & Footer: enter the region for inline editing, creating an empty one if the doc has none.
    const header = svgBtn('header', 'Edit the page header', '', () => this.view.editHeader(), {
      label: 'Header',
    });
    const footer = svgBtn('footer', 'Edit the page footer', '', () => this.view.editFooter(), {
      label: 'Footer',
    });
    panel.append(
      group('Illustrations', [row([picture])]),
      group('Picture', [row([crop, wrap.el])]),
      group('Links', [row([link, bookmark])]),
      group('Header & Footer', [row([header, footer])]),
    );
    return panel;
  }

  /** References tab: Table of Contents group (insert + update the TOC field). */
  private buildReferences(): HTMLElement {
    const panel = h('div', { class: 'scr-panel scr-hidden' });
    const toc = svgBtn('toc', 'Insert table of contents', '', () => this.view.insertToc(), {
      label: 'Table of Contents',
    });
    const update = svgBtn('refresh', 'Update table (F9)', '', () => this.view.updateToc(), {
      label: 'Update Table',
    });
    panel.append(group('Table of Contents', [row([toc, update])]));
    return panel;
  }

  private buildLayout(): HTMLElement {
    const panel = h('div', { class: 'scr-panel scr-hidden' });

    const pageItems = (): { value: string; label: string }[] =>
      PAGE_SIZES[this.units].map(([label, w, hgt]) => ({ value: `${w}x${hgt}`, label }));
    const pageSize = dropdown({
      items: pageItems(),
      title: 'Page size',
      width: '96px',
      onChange: (v) => {
        const [w, hgt] = v.split('x').map(Number);
        this.view.setPageSize({ width: w!, height: hgt! });
      },
    });
    const fillPageSizes = () => pageSize.setItems(pageItems());

    const portrait = svgBtn('page', 'Portrait', '', () => this.view.setLandscape(false));
    const landscape = svgBtn('page', 'Landscape', '', () => this.view.setLandscape(true), {
      rotate: 90,
    });

    const m0 = MARGINS[0]!;
    const margins = dropdown({
      items: MARGINS.map(([label, t, r, btm, l]) => ({ value: `${t},${r},${btm},${l}`, label })),
      value: `${m0[1]},${m0[2]},${m0[3]},${m0[4]}`,
      title: 'Margins',
      width: '110px',
      onChange: (v) => {
        const [t, r, btm, l] = v.split(',').map(Number);
        this.view.setMargins(t!, r!, btm!, l!);
      },
    });

    const unitsSel = dropdown({
      items: [
        { value: 'us', label: 'US (inch)' },
        { value: 'eu', label: 'EU (mm)' },
      ],
      value: this.units,
      title: 'Units',
      width: '110px',
      onChange: (v) => {
        this.unitsLocked = true; // an explicit choice wins over the document's inferred unit
        this.units = v as Units;
        fillPageSizes();
        this.opts.onUnits?.(this.units);
      },
    });

    // Word's Page Setup dialog launcher (the little corner button): exact margins, orientation, paper.
    const dialogBtn = svgBtn('page', 'Page Setup', '', () => this.openPageSetup(), {
      label: 'Page Setup',
    });

    panel.append(
      group('Page Setup', [
        row([labeled('Size', pageSize.el)]),
        row([portrait, landscape, dialogBtn]),
      ]),
      sep(),
      group('Margins', [
        row([labeled('Margins', margins.el)]),
        row([labeled('Units', unitsSel.el)]),
      ]),
    );

    this.refreshLayout = () => {
      // Auto-follow the document's paper size: opening an A4 doc flips to mm, a Letter doc to inch -
      // until the user picks a unit by hand. (Docs load at runtime, after construction, so this is
      // where the switch must happen.) Switching here also re-fills the size list + tells the rulers.
      if (!this.unitsLocked) {
        const want = inferUnits(this.view);
        if (want !== this.units) {
          this.units = want;
          unitsSel.setValue(want);
          fillPageSizes();
          this.opts.onUnits?.(want);
        }
      }
      const g = this.view.pageGeometry();
      // Snap the displayed size to the nearest named preset (in either orientation), so a doc whose
      // A4 is 11910 twips - not exactly 11906 - still shows "A4" instead of the raw "11910x16838".
      const match = PAGE_SIZES[this.units].find(
        ([, w, hgt]) =>
          (dimsNear(g.width, w) && dimsNear(g.height, hgt)) ||
          (dimsNear(g.width, hgt) && dimsNear(g.height, w)),
      );
      pageSize.setValue(match ? `${match[1]}x${match[2]}` : `${g.width}x${g.height}`);
      toggle(portrait, g.width <= g.height);
      toggle(landscape, g.width > g.height);
    };

    return panel;
  }

  /** A labelled field (stacked label over control) for the Page Setup dialog. */
  private modalField(label: string, control: HTMLElement): HTMLElement {
    const wrap = h('label', { class: 'scr-modal-field' });
    const span = h('span', { class: 'scr-modal-label' });
    span.textContent = label;
    wrap.append(span, control);
    return wrap;
  }

  /** Word's Page Setup dialog: orientation, paper size, and exact margins in the active unit (in / mm).
   *  A centered modal (backdrop click / Esc / Cancel dismiss); OK applies size + orientation + margins. */
  private openPageSetup(): void {
    const g = this.view.pageGeometry();
    const metric = this.units === 'eu';
    const unit = metric ? 'mm' : 'in';
    const toUnit = (tw: number): number =>
      metric ? Math.round((tw / 1440) * 25.4 * 10) / 10 : Math.round((tw / 1440) * 100) / 100;
    const toTwips = (v: number): number => Math.round(metric ? (v / 25.4) * 1440 : v * 1440);

    const backdrop = h('div', { class: 'scr-modal-backdrop' });
    const card = h('div', { class: 'scr-modal' });
    backdrop.append(card);
    const title = h('div', { class: 'scr-modal-title' });
    title.textContent = 'Page Setup';
    card.append(title);

    // Orientation segmented control.
    let landscape = g.width > g.height;
    const portraitBtn = h('button', {
      class: 'scr-modal-seg',
      type: 'button',
    }) as HTMLButtonElement;
    portraitBtn.textContent = 'Portrait';
    const landscapeBtn = h('button', {
      class: 'scr-modal-seg',
      type: 'button',
    }) as HTMLButtonElement;
    landscapeBtn.textContent = 'Landscape';
    const syncOrient = (): void => {
      portraitBtn.classList.toggle('active', !landscape);
      landscapeBtn.classList.toggle('active', landscape);
    };
    portraitBtn.addEventListener('click', () => {
      landscape = false;
      syncOrient();
    });
    landscapeBtn.addEventListener('click', () => {
      landscape = true;
      syncOrient();
    });
    syncOrient();
    const orient = h('div', { class: 'scr-modal-seg-group' });
    orient.append(portraitBtn, landscapeBtn);
    card.append(this.modalField('Orientation', orient));

    // Paper size (portrait dimensions in twips).
    const SIZES: [string, number, number][] = [
      ['Letter', 12240, 15840],
      ['Legal', 12240, 20160],
      ['A4', 11906, 16838],
      ['A5', 8391, 11906],
    ];
    const pw = Math.min(g.width, g.height);
    const ph = Math.max(g.width, g.height);
    // The closest named size by total dimension difference (so an ~11910-twip A4 reads as "A4", not
    // a Letter fallback).
    let paper = SIZES.reduce(
      (best, [label, w, hh]) => {
        const d = Math.abs(w - pw) + Math.abs(hh - ph);
        return d < best.d ? { label, d } : best;
      },
      { label: 'Letter', d: Number.POSITIVE_INFINITY },
    ).label;
    const sizeSel = dropdown({
      items: SIZES.map(([label]) => ({ value: label, label })),
      value: paper,
      title: 'Paper size',
      width: '100%',
      onChange: (v) => {
        paper = v;
      },
    });
    card.append(this.modalField('Paper size', sizeSel.el));

    // Margins (in the active unit).
    const numInput = (tw: number): HTMLInputElement => {
      const i = h('input', {
        class: 'scr-modal-num',
        type: 'number',
        step: metric ? '1' : '0.1',
      }) as HTMLInputElement;
      i.value = String(toUnit(tw));
      return i;
    };
    const mt = numInput(g.marginTop);
    const mb = numInput(g.marginBottom);
    const ml = numInput(g.marginLeft);
    const mr = numInput(g.marginRight);
    const grid = h('div', { class: 'scr-modal-margins' });
    grid.append(
      this.modalField(`Top (${unit})`, mt),
      this.modalField(`Bottom (${unit})`, mb),
      this.modalField(`Left (${unit})`, ml),
      this.modalField(`Right (${unit})`, mr),
    );
    card.append(grid);

    const actions = h('div', { class: 'scr-modal-actions' });
    const cancel = h('button', { class: 'scr-modal-btn', type: 'button' }) as HTMLButtonElement;
    cancel.textContent = 'Cancel';
    const ok = h('button', {
      class: 'scr-modal-btn scr-modal-primary',
      type: 'button',
    }) as HTMLButtonElement;
    ok.textContent = 'OK';
    actions.append(cancel, ok);
    card.append(actions);

    const close = (): void => {
      backdrop.remove();
      document.removeEventListener('keydown', onKey, true);
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') {
        e.preventDefault();
        close();
      }
    };
    backdrop.addEventListener('mousedown', (e) => {
      if (e.target === backdrop) close();
    });
    document.addEventListener('keydown', onKey, true);
    cancel.addEventListener('click', close);
    ok.addEventListener('click', () => {
      const [, w, hh] = SIZES.find(([l]) => l === paper) ?? SIZES[0]!;
      this.view.setPageSize({ width: w, height: hh });
      this.view.setLandscape(landscape);
      // setMargins takes (top, right, bottom, left).
      this.view.setMargins(
        toTwips(+mt.value || 0),
        toTwips(+mr.value || 0),
        toTwips(+mb.value || 0),
        toTwips(+ml.value || 0),
      );
      close();
      this.refreshLayout?.();
    });

    document.body.append(backdrop);
    setTimeout(() => mt.focus(), 0);
  }

  /** Word's Modify-Style dialog (minimal): edit a paragraph style's *definition* - font, size, bold,
   *  italic, colour, line spacing, space before/after - prefilled from what the style currently renders
   *  at. OK writes only the fields the user actually changed (per-field merge), so every paragraph using
   *  the style re-renders. A centered modal (backdrop / Esc / Cancel dismiss). */
  private openModifyStyle(styleId: string, styleName: string): void {
    const r = this.view.resolveStyleProps(styleId);

    const backdrop = h('div', { class: 'scr-modal-backdrop' });
    const card = h('div', { class: 'scr-modal' });
    backdrop.append(card);
    const title = h('div', { class: 'scr-modal-title' });
    title.textContent = `Modify Style: ${styleName}`;
    card.append(title);

    // Font family - include the resolved font even when it's not one of the presets, so it round-trips.
    const fontItems = !r.font || FONTS.includes(r.font) ? FONTS : [r.font, ...FONTS];
    const initFont = r.font || FONTS[0]!;
    let font = initFont;
    const fontSel = dropdown({
      items: fontItems.map(fontItem),
      value: initFont,
      title: 'Font',
      width: '100%',
      onChange: (v) => {
        font = v;
      },
    });
    card.append(this.modalField('Font', fontSel.el));

    // Size (points) + Bold / Italic segmented toggles.
    const sizeInput = h('input', {
      class: 'scr-modal-num',
      type: 'number',
      step: '1',
      min: '1',
    }) as HTMLInputElement;
    const initSizePt = r.size ? r.size / 2 : 0;
    sizeInput.value = initSizePt ? String(initSizePt) : '';
    let bold = r.bold;
    let italic = r.italic;
    const boldBtn = h('button', { class: 'scr-modal-seg', type: 'button' }) as HTMLButtonElement;
    boldBtn.textContent = 'B';
    boldBtn.style.fontWeight = '700';
    const italicBtn = h('button', { class: 'scr-modal-seg', type: 'button' }) as HTMLButtonElement;
    italicBtn.textContent = 'I';
    italicBtn.style.fontStyle = 'italic';
    const syncBI = (): void => {
      boldBtn.classList.toggle('active', bold);
      italicBtn.classList.toggle('active', italic);
    };
    boldBtn.addEventListener('click', () => {
      bold = !bold;
      syncBI();
    });
    italicBtn.addEventListener('click', () => {
      italic = !italic;
      syncBI();
    });
    syncBI();
    const biGroup = h('div', { class: 'scr-modal-seg-group' });
    biGroup.append(boldBtn, italicBtn);
    const sizeRow = h('div', { class: 'scr-modal-margins' });
    sizeRow.append(this.modalField('Size (pt)', sizeInput), this.modalField('Weight', biGroup));
    card.append(sizeRow);

    // Text colour - native picker; OOXML stores hex without the leading '#'.
    const colorInput = h('input', { class: 'scr-modal-color', type: 'color' }) as HTMLInputElement;
    const initColor = r.color ? `#${r.color}` : '#000000';
    colorInput.value = initColor;
    card.append(this.modalField('Text colour', colorInput));

    // Alignment - a segmented Left / Center / Right / Justify control.
    const initAlign = r.align || 'left';
    let align = initAlign;
    const alignGroup = h('div', { class: 'scr-modal-seg-group' });
    const alignBtns = (['left', 'center', 'right', 'justify'] as const).map((a) => {
      const btn = h('button', {
        class: 'scr-modal-seg',
        type: 'button',
        title: a,
      }) as HTMLButtonElement;
      btn.textContent = { left: 'Left', center: 'Center', right: 'Right', justify: 'Justify' }[a];
      btn.addEventListener('click', () => {
        align = a;
        for (const b of alignBtns) b.classList.toggle('active', b === btn);
      });
      return btn;
    });
    for (const b of alignBtns) {
      if (b.title === initAlign) b.classList.add('active');
      alignGroup.append(b);
    }
    card.append(this.modalField('Alignment', alignGroup));

    // Line spacing - the common multiples (auto rule, 240ths) plus "Exactly" (absolute pt). The rule
    // dropdown picks the kind; the "Exactly" value box is enabled only for that rule.
    const SPACINGS: [string, number][] = [
      ['Single', 240],
      ['1.5 lines', 360],
      ['Double', 480],
    ];
    const isExact = r.lineRule === 'exact';
    let lineKind = isExact
      ? 'exact'
      : String(
          SPACINGS.reduce(
            (b, [, v]) =>
              Math.abs(v - (r.lineSpacing || 240)) < Math.abs(b - (r.lineSpacing || 240)) ? v : b,
            240,
          ),
        );
    const exactInput = h('input', {
      class: 'scr-modal-num',
      type: 'number',
      step: '0.5',
      min: '1',
    }) as HTMLInputElement;
    exactInput.value =
      isExact && r.lineSpacing > 0 ? String(Math.round((r.lineSpacing / 20) * 10) / 10) : '';
    exactInput.disabled = !isExact;
    const lineSel = dropdown({
      items: [
        ...SPACINGS.map(([label, v]) => ({ value: String(v), label })),
        { value: 'exact', label: 'Exactly' },
      ],
      value: lineKind,
      title: 'Line spacing',
      width: '100%',
      onChange: (v) => {
        lineKind = v;
        exactInput.disabled = v !== 'exact';
      },
    });
    const lineRow = h('div', { class: 'scr-modal-margins' });
    lineRow.append(
      this.modalField('Line spacing', lineSel.el),
      this.modalField('Exactly (pt)', exactInput),
    );
    card.append(lineRow);

    // Space before / after (points; stored as twips = pt × 20).
    const beforeInput = h('input', {
      class: 'scr-modal-num',
      type: 'number',
      step: '1',
      min: '0',
    }) as HTMLInputElement;
    const afterInput = h('input', {
      class: 'scr-modal-num',
      type: 'number',
      step: '1',
      min: '0',
    }) as HTMLInputElement;
    beforeInput.value = r.spaceBefore >= 0 ? String(Math.round(r.spaceBefore / 20)) : '';
    afterInput.value = r.spaceAfter >= 0 ? String(Math.round(r.spaceAfter / 20)) : '';
    const spRow = h('div', { class: 'scr-modal-margins' });
    spRow.append(
      this.modalField('Before (pt)', beforeInput),
      this.modalField('After (pt)', afterInput),
    );
    card.append(spRow);

    const actions = h('div', { class: 'scr-modal-actions' });
    const cancel = h('button', { class: 'scr-modal-btn', type: 'button' }) as HTMLButtonElement;
    cancel.textContent = 'Cancel';
    const ok = h('button', {
      class: 'scr-modal-btn scr-modal-primary',
      type: 'button',
    }) as HTMLButtonElement;
    ok.textContent = 'OK';
    actions.append(cancel, ok);
    card.append(actions);

    const close = (): void => {
      backdrop.remove();
      document.removeEventListener('keydown', onKey, true);
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') {
        e.preventDefault();
        close();
      }
    };
    backdrop.addEventListener('mousedown', (e) => {
      if (e.target === backdrop) close();
    });
    document.addEventListener('keydown', onKey, true);
    cancel.addEventListener('click', close);
    ok.addEventListener('click', () => {
      // Only the fields the user actually changed (per-field merge keeps the rest inheriting).
      const edit: StyleEdit = {};
      if (font !== initFont) edit.font = font;
      const sizePt = +sizeInput.value || 0;
      const sizeHp = Math.round(sizePt * 2);
      if (sizePt > 0 && sizeHp !== r.size) edit.size = sizeHp;
      if (bold !== r.bold) edit.bold = bold;
      if (italic !== r.italic) edit.italic = italic;
      if (colorInput.value.toLowerCase() !== initColor.toLowerCase()) {
        edit.color = colorInput.value.replace(/^#/, '').toUpperCase();
      }
      // Line spacing: "Exactly" -> twips + exact rule; a preset -> 240ths + auto rule.
      let newLine: number | null = null;
      let newRule = 'auto';
      if (lineKind === 'exact') {
        const pt = +exactInput.value || 0;
        if (pt > 0) {
          newLine = Math.round(pt * 20);
          newRule = 'exact';
        }
      } else {
        newLine = +lineKind;
      }
      if (newLine !== null && (newLine !== r.lineSpacing || newRule !== r.lineRule)) {
        edit.lineSpacing = newLine;
        edit.lineRule = newRule;
      }
      if (beforeInput.value !== '') {
        const tw = Math.round((+beforeInput.value || 0) * 20);
        if (tw !== r.spaceBefore) edit.spaceBefore = tw;
      }
      if (afterInput.value !== '') {
        const tw = Math.round((+afterInput.value || 0) * 20);
        if (tw !== r.spaceAfter) edit.spaceAfter = tw;
      }
      if (align !== initAlign) edit.align = align;
      if (Object.keys(edit).length > 0) this.view.modifyStyle(styleId, edit);
      close();
      this.refreshUi();
    });

    document.body.append(backdrop);
    setTimeout(() => sizeInput.focus(), 0);
  }

  /** Word's "Create a Style" (minimal): name a new paragraph style that captures the caret
   *  selection's formatting and apply it to the selected paragraph(s). A centered modal with a single
   *  name field (backdrop / Esc / Cancel dismiss; Enter or OK creates). */
  private openNewStyle(): void {
    const backdrop = h('div', { class: 'scr-modal-backdrop' });
    const card = h('div', { class: 'scr-modal' });
    backdrop.append(card);
    const title = h('div', { class: 'scr-modal-title' });
    title.textContent = 'New Style from Selection';
    card.append(title);

    const nameInput = h('input', {
      class: 'scr-modal-text',
      type: 'text',
      placeholder: 'Style name',
    }) as HTMLInputElement;
    card.append(this.modalField('Name', nameInput));

    const actions = h('div', { class: 'scr-modal-actions' });
    const cancel = h('button', { class: 'scr-modal-btn', type: 'button' }) as HTMLButtonElement;
    cancel.textContent = 'Cancel';
    const ok = h('button', {
      class: 'scr-modal-btn scr-modal-primary',
      type: 'button',
    }) as HTMLButtonElement;
    ok.textContent = 'Create';
    actions.append(cancel, ok);
    card.append(actions);

    const close = (): void => {
      backdrop.remove();
      document.removeEventListener('keydown', onKey, true);
    };
    const submit = (): void => {
      const name = nameInput.value.trim();
      if (name) this.view.createStyleFromSelection(name);
      close();
      this.refreshUi();
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') {
        e.preventDefault();
        close();
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        submit();
      }
    };
    backdrop.addEventListener('mousedown', (e) => {
      if (e.target === backdrop) close();
    });
    document.addEventListener('keydown', onKey, true);
    cancel.addEventListener('click', close);
    ok.addEventListener('click', submit);

    document.body.append(backdrop);
    setTimeout(() => nameInput.focus(), 0);
  }

  private buildReview(): HTMLElement {
    const panel = h('div', { class: 'scr-panel scr-hidden' });

    // Tracking group: the Track-Changes toggle + Display-for-Review dropdown.
    const track = svgBtn(
      'trackChanges',
      'Track Changes (suggesting mode)',
      'scr-track-toggle',
      () => this.view.setTrackChanges(!this.view.trackChangesOn),
      { label: 'Track Changes' },
    );
    const DISPLAYS: [TrackDisplay, string][] = [
      ['all', 'All Markup'],
      ['simple', 'Simple Markup'],
      ['none', 'No Markup'],
      ['original', 'Original'],
    ];
    const display = dropdown({
      items: DISPLAYS.map(([val, label]) => ({ value: val, label })),
      value: 'all',
      title: 'Display for Review',
      width: '132px',
      onChange: (v) => this.view.setTrackDisplay(v as TrackDisplay),
    });
    // Lock Tracking: forces Track-Changes on (it can't be turned off until unlocked).
    const lock = svgBtn(
      'lock',
      'Lock Tracking (forces Track Changes on)',
      'scr-track-lock',
      () => this.view.setTrackLocked(!this.view.trackLocked),
      { label: 'Lock' },
    );
    // Balloons: show tracked deletions in right-margin bubbles (the body narrows to make room).
    const balloons = svgBtn(
      'comment',
      'Show revisions in balloons (deletions in the right margin)',
      'scr-balloons',
      () => this.view.setBalloons(!this.view.balloonsOn),
      { label: 'Balloons' },
    );
    // Two even rows: the toggles on top, the Display dropdown below - no nested column (which made a
    // lopsided tall sub-stack next to Track Changes).
    const trackingGroup = group('Tracking', [
      row([track, lock, balloons]),
      row([labeled('Display', display.el)]),
    ]);

    // Changes group: Word-style split buttons (Accept / Reject, with the "All" variants in the
    // dropdown) + Previous / Next navigation.
    const accept = splitBtn(
      'accept',
      'Accept',
      'Accept this change',
      () => this.view.acceptChange(),
      [
        { label: 'Accept Change', icon: 'accept', onClick: () => this.view.acceptChange() },
        { label: 'Accept All Changes', icon: 'acceptAll', onClick: () => this.view.acceptAll() },
      ],
    );
    const reject = splitBtn(
      'reject',
      'Reject',
      'Reject this change',
      () => this.view.rejectChange(),
      [
        { label: 'Reject Change', icon: 'reject', onClick: () => this.view.rejectChange() },
        { label: 'Reject All Changes', icon: 'reject', onClick: () => this.view.rejectAll() },
      ],
    );
    const prev = svgBtn('prev', 'Previous change', '', () => this.view.prevChange(), {
      label: 'Previous',
    });
    const next = svgBtn('next', 'Next change', '', () => this.view.nextChange(), { label: 'Next' });
    const changesGroup = group('Changes', [row([accept, reject]), row([prev, next])]);

    // Comments group: New (on the selection) + Delete (at the caret) + Previous / Next comment.
    const newComment = svgBtn(
      'comment',
      'New comment (on the selection)',
      'scr-comment-new scr-large',
      () => this.view.newComment(),
      { size: 22, label: 'Comment' },
    );
    const delComment = svgBtn(
      'trash',
      'Delete comment',
      '',
      () => this.view.deleteCommentAtCaret(),
      { label: 'Delete' },
    );
    const prevComment = svgBtn('prev', 'Previous comment', '', () => this.view.prevComment(), {
      label: 'Previous',
    });
    const nextComment = svgBtn('next', 'Next comment', '', () => this.view.nextComment(), {
      label: 'Next',
    });
    const commentsGroup = group('Comments', [
      row([newComment, delComment]),
      row([prevComment, nextComment]),
    ]);

    panel.append(trackingGroup, sep(), changesGroup, sep(), commentsGroup);

    // Compare group (only when the host wired a handler, e.g. the Workspace): opens the Compare
    // Documents dialog, and hands the redline + manifest back to the host.
    if (this.opts.onCompare) {
      const compareBtn = svgBtn(
        'compare',
        'Compare this document with another (redline)',
        'scr-large',
        () =>
          openCompareDialog(
            this.view,
            (result, sources) => this.opts.onCompare?.(result, sources),
            {
              versions: this.opts.compareVersions,
            },
          ),
        { size: 22, label: 'Compare' },
      );
      const compareRow = [compareBtn];
      if (this.opts.onSideBySide) {
        const sbs = svgBtn(
          'sideBySide',
          'Side-by-side view of the two compared documents (scroll-locked)',
          'scr-large',
          () => this.opts.onSideBySide?.(),
          { size: 22, label: 'Side by Side' },
        );
        sbs.disabled = true; // enabled once a comparison has run (setSideBySide)
        this.sideBySideBtn = sbs;
        compareRow.push(sbs);
      }
      panel.append(sep(), group('Compare', [row(compareRow)]));
    }

    // Reviewing-pane toggle (only when the host wired a handler, e.g. the Workspace).
    if (this.opts.onReviewingPane) {
      const paneBtn = svgBtn(
        'list',
        'Toggle the reviewing pane',
        'scr-paste',
        () => this.opts.onReviewingPane?.(),
        { size: 22, label: 'Reviewing Pane' },
      );
      panel.append(sep(), group('Panes', [row([paneBtn])]));
    }

    this.refreshReview = () => {
      panel.classList.toggle('scr-readonly', this.view.mode === 'read');
      toggle(track, this.view.trackChangesOn);
      toggle(lock, this.view.trackLocked);
      toggle(balloons, this.view.balloonsOn);
      display.setValue(this.view.trackDisplayMode);
      newComment.disabled = !this.view.canAddComment();
    };

    return panel;
  }

  /** Enable/press the "Side by side" toggle. The host calls this after a comparison runs (`available`)
   *  and whenever the side-by-side view is shown/hidden (`active`). */
  setSideBySide(available: boolean, active: boolean): void {
    if (!this.sideBySideBtn) return;
    this.sideBySideBtn.disabled = !available;
    toggle(this.sideBySideBtn, active);
  }
}
