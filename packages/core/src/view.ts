import { ScriptorDoc } from '@truespar/scriptor-wasm';

import { compareDocuments } from './compare';
import type { CompareOptions, CompareResult } from './compare';
import {
  BODY_PARA_LIMIT,
  changeAccentColor,
  changeBandColor,
  FONT_SIZE_LADDER,
  GUTTER_DEFAULT,
  regionOf,
  sanitizeBookmarkName,
  TRACK_VERB,
} from './helpers';
import type {
  CaretPos,
  ChangeMarker,
  CommentJson,
  RemoteCursor,
  ResolvedStyleProps,
  ReviewItem,
  ScriptorContextMenuContext,
  ScriptorContextMenuItem,
  ScriptorMode,
  ScriptorOptions,
  Selection,
  StyleEdit,
  StyleGalleryItem,
  TrackDisplay,
} from './types';
import { ImageController } from './images';
import { PopupLayer } from './popups';
import { PresenceLayer } from './presence';
import { ensureWasm } from './wasm';




/**
 * A headless Scriptor document view: it renders an OOXML document onto its own `<canvas>` (via the
 * Rust/WASM engine), and - in edit mode - owns the caret, selection, and keyboard, routing every
 * edit through the shared `scriptor_edit::apply` path (the same one the agent uses). It mounts into
 * a container element and brings no chrome of its own (toolbars etc. belong to the host app), which
 * is what makes it reusable across apps and across read-only vs edit surfaces.
 *
 * Construct with [`ScriptorView.create`] (it awaits the one-time WASM init).
 */
export class ScriptorView {
  private readonly container: HTMLElement;
  private readonly sheet: HTMLDivElement;
  /** Behind the canvas: one drop-shadowed white rect per page (Word-style per-page frames). */
  private readonly framesLayer: HTMLDivElement;
  private framesCount = 0;
  private readonly page: HTMLCanvasElement;
  private readonly overlay: HTMLCanvasElement;
  private readonly keys: HTMLTextAreaElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly octx: CanvasRenderingContext2D;

  private doc: ScriptorDoc;
  /** The raw `.docx` bytes as last loaded via [`loadDocx`], kept so [`compareWith`] can diff the
   *  document *as opened* rather than a re-serialization of it. `null` for a from-scratch / snapshot
   *  document. Cleared and re-set by [`replaceDoc`] / [`loadDocx`]. */
  private loadedBytes: Uint8Array | null = null;
  /** Whether any content edit happened since the document was loaded. When `false`, [`compareWith`]
   *  can use [`loadedBytes`] verbatim (a lossless file-to-file diff); once the user edits, it must
   *  fall back to serializing the live model. */
  private editedSinceLoad = false;
  /** Monotonic id of the loaded document, bumped every time the whole document is replaced (open /
   *  new / snapshot / compare redline). Lets chrome (e.g. the reviewing pane's comparison scope) tell
   *  "the document was swapped" from "the document was edited", which content-change listeners can't. */
  private docEpoch = 0;
  private opts: Required<
    Omit<
      ScriptorOptions,
      'onChange' | 'onSelectionChange' | 'onReady' | 'onSave' | 'author' | 'onContextMenu'
    >
  > &
    Pick<
      ScriptorOptions,
      'onChange' | 'onSelectionChange' | 'onReady' | 'onSave' | 'onContextMenu'
    >;
  private dpr: number;

  private sel: Selection | null = null;
  private dragging = false;
  private caretOn = true;
  /** Which story the caret is in (0 = body, 1 = header, 2 = footer) - drives Word's dimming of the
   *  inactive region(s); a change forces a full repaint. */
  private activeRegion = 0;
  private blinkTimer: ReturnType<typeof setInterval> | null = null;
  private renderScheduled = false;
  private lastFingerprints: bigint[] = [];
  private destroyed = false;
  private zoom = 1;
  // ── virtualized canvas window ─────────────────────────────────────────────────
  // The wrapper (`sheet`) spans the WHOLE document (CSS height = total/dpr) so scrolling, events,
  // and client<->device math work over the full extent - but the two canvases only back a
  // page-snapped WINDOW around the viewport. One full-document canvas hits hard browser limits
  // (Safari caps canvas area at ~16.8M px - a dozen pages at 100% zoom, and it fails BLANK) and
  // costs O(document) memory; the window keeps both bounded and paint O(visible).
  /** Full-document device dimensions from the last render (the logical canvas space every rect,
   *  caret, and hit test is expressed in - unchanged by the windowing). */
  private deviceW = 0;
  private deviceTotalH = 0;
  /** First page of the painted window and its device-y origin; drawing subtracts this. */
  private windowFirst = 0;
  private windowCount = 0;
  private windowTopDev = 0;
  /** Per page: the fingerprint last rasterized into the window canvas (`null` = not painted). */
  private paintedAt: (bigint | null)[] = [];
  private onScrollOrResize: (() => void) | null = null;
  private windowUpdateScheduled = false;
  // Page band geometry (device px) from the last render, so a click can be mapped to a page-local
  // y (change-bar hit-testing for Simple-Markup click-to-expand). One page `i` paints at canvas
  // `y = i * (pageH + pageGap)`.
  private pageH = 0;
  private pageGap = 0;
  // Set when a mousedown handled a change-bar click (Simple-Markup expand/collapse), so the trailing
  // `click` event doesn't also open an accept/reject popup at the gutter position.
  private suppressNextClick = false;
  private trackDisplay: TrackDisplay = 'all';
  private trackChanges = false;
  private author: { id: string; name: string };
  private listeners = new Set<() => void>();
  private lastHoverKey = '';
  private commentRectsCache: number[] = [];
  // A move in progress (A1): a tracked cut marks the source (`w:moveFrom`, allocating this id) and
  // holds the cut text; the next matching paste relocates it as the destination (`w:moveTo`) under the
  // same id. Cleared by any other edit (so a stale source can't be paired) and on track-off / doc swap.
  private pendingMove: { id: number; text: string } | null = null;
  // Reviewers (`w:author` names) whose markup is filtered out of the display (R3). The authoritative
  // set lives in the engine; this mirror lets `reviewItems()` drop their rows without a wasm round-trip.
  private hiddenReviewers = new Set<string>();

  /** Create + mount a view. Awaits the one-time WASM init, then renders an empty document. */
  static async create(
    container: HTMLElement,
    options: ScriptorOptions = {},
  ): Promise<ScriptorView> {
    await ensureWasm();
    return new ScriptorView(container, options);
  }

  /** The picture subsystem: it owns the selected-image state and the view only routes to it. */
  private readonly img: ImageController;

  /** Menus, tooltips, the comment popover and the input dialog. Owns the elements it mounts. */
  private readonly pop: PopupLayer;

  /** Remote carets, their name chips, and comparison change markers. */
  private readonly presence: PresenceLayer;

  private constructor(container: HTMLElement, options: ScriptorOptions) {
    // The adapter is built here rather than handing over `this`, so nothing on the view has to
    // become public just to let pictures work. Every member is an accessor, so it reads whatever
    // the view holds at call time - `doc` in particular is replaced wholesale by `replaceDoc`.
    // One adapter, shared by all three controllers. Each of their Host interfaces is a subset of
    // it, so structural typing does the matching and every member is defined exactly once. It is
    // built here rather than handing over `this` so nothing on the view has to become public, and
    // every member is an accessor so it reads what the view holds at call time - `doc` in
    // particular is replaced wholesale by `replaceDoc`.
    const v = this;
    const host = {
      get doc() {
        return v.doc;
      },
      get sheet() {
        return v.sheet;
      },
      get octx() {
        return v.octx;
      },
      get keys() {
        return v.keys;
      },
      get opts() {
        return v.opts;
      },
      get dpr() {
        return v.dpr;
      },
      get zoom() {
        return v.zoom;
      },
      get pageH() {
        return v.pageH;
      },
      get pageGap() {
        return v.pageGap;
      },
      get deviceW() {
        return v.deviceW;
      },
      get deviceTotalH() {
        return v.deviceTotalH;
      },
      get trackChanges() {
        return v.trackChanges;
      },
      get destroyed() {
        return v.destroyed;
      },
      get authorAvatars() {
        return v.presence.authorAvatars;
      },
      get sel() {
        return v.sel;
      },
      set sel(x) {
        v.sel = x;
      },
      get caretOn() {
        return v.caretOn;
      },
      set caretOn(x) {
        v.caretOn = x;
      },
      get dragging() {
        return v.dragging;
      },
      set dragging(x) {
        v.dragging = x;
      },
      get lastHoverKey() {
        return v.lastHoverKey;
      },
      set lastHoverKey(x) {
        v.lastHoverKey = x;
      },
      bodyCaret: () => v.bodyCaret(),
      hit: (e: MouseEvent) => v.hit(e),
      toCanvas: (e: MouseEvent) => v.toCanvas(e),
      setSelection: (focus: CaretPos, anchor?: CaretPos) => v.setSelection(focus, anchor),
      drawOverlay: () => v.drawOverlay(),
      fullRender: () => v.fullRender(),
      requestRender: () => v.requestRender(),
      fireChange: () => v.fireChange(),
      notify: () => v.notify(),
      displayAuthor: (a: string) => v.presence.displayAuthor(a),
      caretClientXY: (para: number, off: number) => v.caretClientXY(para, off),
      afterResolve: () => v.afterResolve(),
      resolveRevision: (para: number, id: number, accept: boolean) =>
        v.resolveRevision(para, id, accept),
    };
    this.img = new ImageController(host);
    this.presence = new PresenceLayer(host);
    this.pop = new PopupLayer(host);
    this.container = container;
    this.opts = {
      mode: options.mode ?? 'read',
      scale: options.scale ?? Math.max(1, Math.floor(globalThis.devicePixelRatio || 1)),
      gutter: options.gutter ?? GUTTER_DEFAULT,
      selectable: options.selectable ?? true,
      tableMenu: options.tableMenu ?? true,
      onChange: options.onChange,
      onSelectionChange: options.onSelectionChange,
      onReady: options.onReady,
      onSave: options.onSave,
      onContextMenu: options.onContextMenu,
    };
    this.author = options.author ?? { id: 'local', name: 'You' };
    this.dpr = this.opts.scale;

    // DOM: a positioned sheet wrapper spanning the WHOLE document (it owns scrolling extent, mouse
    // events, and the client<->device mapping), holding a windowed content canvas + overlay that
    // only back the pages around the viewport (see the virtualized-window fields), plus an
    // off-screen textarea that holds keyboard focus while editing.
    this.sheet = document.createElement('div');
    this.sheet.className = 'scriptor-sheet';
    // No `overflow:hidden`: the per-page frames' drop shadows extend slightly past the page edges
    // and must not be clipped. The windowed canvas is page-width and positioned, so it doesn't
    // overflow anyway.
    this.sheet.style.cssText = 'position:relative;display:inline-block;';
    // Per-page frames: one white drop-shadowed rect behind the canvas per page, so each page reads
    // as its own sheet (Word) instead of one long block under a single container shadow. The canvas
    // paints page content opaquely over each frame and leaves the inter-page gutters transparent, so
    // the frames' shadows + the host's page-stage backdrop show through the gaps.
    this.framesLayer = document.createElement('div');
    this.framesLayer.className = 'scriptor-frames';
    this.framesLayer.style.cssText = 'position:absolute;top:0;left:0;pointer-events:none;';
    this.page = document.createElement('canvas');
    this.page.className = 'scriptor-page';
    this.page.style.cssText = 'position:absolute;top:0;left:0;display:block;';
    this.overlay = document.createElement('canvas');
    this.overlay.className = 'scriptor-overlay';
    this.overlay.style.cssText = 'position:absolute;top:0;left:0;';
    this.keys = document.createElement('textarea');
    // This off-screen textarea HOLDS keyboard focus while editing, so it must not
    // be aria-hidden: hiding a focused element from assistive tech is an a11y
    // violation that browsers now block ("Blocked aria-hidden ... because its
    // descendant retained focus"). Label it instead so screen readers announce
    // the editing surface; tabIndex -1 keeps it out of the tab order (focus is
    // programmatic only).
    this.keys.setAttribute('aria-label', 'Document editor');
    this.keys.tabIndex = -1;
    this.keys.style.cssText =
      'position:absolute;top:0;left:-9999px;width:1px;height:1px;opacity:0;';
    this.sheet.append(this.framesLayer, this.page, this.overlay);
    this.container.append(this.sheet, this.keys);

    const ctx = this.page.getContext('2d');
    const octx = this.overlay.getContext('2d');
    if (!ctx || !octx) throw new Error('scriptor: 2d canvas context unavailable');
    this.ctx = ctx;
    this.octx = octx;

    this.doc = new ScriptorDoc();
    this.syncDocState();
    this.applyMode();
    this.bindPointer();
    this.bindKeyboard();
    this.bindClipboard();
    this.bindZoom();
    this.bindContextMenu();
    this.bindTrackInteraction();
    // Any scroll (the stage, the page, a nested container - capture catches them all) or viewport
    // resize can move the viewport across the painted canvas window: re-derive it, coalesced to one
    // update per frame. Page-snapping inside updateWindow makes the common small scroll a no-op.
    this.onScrollOrResize = () => {
      if (this.windowUpdateScheduled || this.destroyed) return;
      this.windowUpdateScheduled = true;
      requestAnimationFrame(() => {
        this.windowUpdateScheduled = false;
        if (!this.destroyed) this.updateWindow(false);
      });
    };
    window.addEventListener('scroll', this.onScrollOrResize, true);
    window.addEventListener('resize', this.onScrollOrResize);
    this.fullRender();
    this.opts.onReady?.();
  }

  // ── public API ──────────────────────────────────────────────────────────────

  /** Load a `.docx` (raw OPC zip bytes) and render it. */
  loadDocx(bytes: Uint8Array): void {
    this.replaceDoc(ScriptorDoc.openDocx(bytes));
    // Keep the exact bytes as opened so `compareWith` can diff the file itself, not a re-serialization
    // (which, for a document whose embedded objects / drawings the editable model doesn't yet
    // round-trip, would otherwise surface those losses as phantom changes). Copied so a later mutation
    // of the caller's buffer can't corrupt it.
    this.loadedBytes = bytes.slice();
  }

  /** Replace the content with a fresh empty document. */
  newDocument(): void {
    this.replaceDoc(new ScriptorDoc());
  }

  // ── presence ────────────────────────────────────────────────────────────────
  // Implemented by `PresenceLayer`; these keep the view's own API intact.

  /** Set the remote collaborators' carets to render. */
  setRemoteCursors(cursors: RemoteCursor[]): void {
    this.presence.setRemoteCursors(cursors);
  }

  /** Set the comparison change markers to band on the page. */
  setChangeMarkers(markers: ChangeMarker[]): void {
    this.presence.setChangeMarkers(markers);
  }

  /** Map author ids to avatar image URLs, for presence chips and comment threads. */
  setAuthorAvatars(map: Record<string, string>): void {
    this.presence.setAuthorAvatars(map);
  }

  /** Map author ids to display names. */
  setAuthorNames(map: Record<string, string>): void {
    this.presence.setAuthorNames(map);
  }

  // ── pictures ────────────────────────────────────────────────────────────────
  // The implementation lives in `ImageController`; these keep the view's own API intact.

  /** Prompt for an image file and insert it at the caret. */
  async insertPicture(): Promise<void> {
    return this.img.insertPicture();
  }

  /** The selected picture's id, or `null` when the selection is text. */
  get selectedImageId(): bigint | null {
    return this.img.selectedImageId;
  }

  /** Whether the selected picture is showing crop handles. */
  get cropActive(): boolean {
    return this.img.cropActive;
  }

  /** The selected picture's wrap mode (`inline`, `square`, `behind`, ...), or `null`. */
  get selectedImageWrap(): string | null {
    return this.img.selectedImageWrap;
  }

  /** Set the selected picture's text wrapping. */
  setImageWrap(token: string): void {
    this.img.setImageWrap(token);
  }

  /** Clear any crop on the selected picture, restoring the full image. */
  resetCrop(): void {
    this.img.resetCrop();
  }

  /** Resize the selected picture back to its natural aspect ratio. */
  async resetAspect(): Promise<void> {
    return this.img.resetAspect();
  }

  /** Enter or leave crop mode on the selected picture. */
  toggleCrop(): void {
    this.img.toggleCrop();
  }

  /** Load a collaboration join snapshot (the authoritative server state) as the
   *  document, with no seeded paragraph. The collab provider calls this once on
   *  first connect; later server messages are merged via `applyRemoteUpdate`. */
  loadSnapshot(bytes: Uint8Array): void {
    this.replaceDoc(ScriptorDoc.fromSnapshot(bytes));
  }

  /** The current document body serialized to OOXML `word/document.xml`. */
  toDocumentXml(): string {
    return this.doc.toDocumentXml();
  }

  /** Save the whole document to `.docx` bytes (re-zips the source package with the edits, or builds
   *  a minimal package for a from-scratch document). */
  toDocx(): Uint8Array {
    return this.doc.toDocx();
  }

  /** Compare this view's current document (as the *original*) against a `revised` `.docx` and produce
   *  a redline + change manifest, attributed to the current review author (override via `options`).
   *  The redline is an ordinary tracked-changes document: open it with [`loadDocx`](Self.loadDocx) to
   *  review it - its changes then populate a reviewing pane bound to this view - or hand it to the
   *  host. Does not mutate this view.
   *
   *  When the document is unedited since it was opened, this diffs the **original bytes** (a true
   *  file-to-file comparison); it only serializes the live model once the user has made edits. This
   *  matters for documents whose embedded objects / drawings the editable model does not yet
   *  round-trip: re-serializing would drop them and report the losses as phantom changes. */
  compareWith(revised: Uint8Array, options: CompareOptions = {}): Promise<CompareResult> {
    return compareDocuments(this.comparisonSource(), revised, {
      author: options.author ?? this.author.name,
      date: options.date,
    });
  }

  /** The bytes this view uses as the *original* in [`compareWith`]: the file as loaded when unedited,
   *  else the serialized live model. Exposed so a side-by-side comparison can render the exact document
   *  that was diffed (same paragraph indices as the comparison's alignment). */
  comparisonSource(): Uint8Array {
    return !this.editedSinceLoad && this.loadedBytes ? this.loadedBytes : this.toDocx();
  }

  /** Invoke Save (the Quick Access Toolbar button / Ctrl+S): hand the serialized `.docx` bytes to the
   *  host's `onSave` callback, or - when none was provided - trigger a `.docx` download in the browser
   *  so standalone use just works. */
  save(): void {
    const bytes = this.doc.toDocx();
    if (this.opts.onSave) {
      this.opts.onSave(bytes);
      return;
    }
    if (typeof document === 'undefined') return;
    const blob = new Blob([bytes as BlobPart], {
      type: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'document.docx';
    a.click();
    URL.revokeObjectURL(url);
  }

  // ── live collaboration ────────────────────────────────────────────────────
  // The CRDT wire surface a collab provider drives (see the collab websocket
  // provider in collab.ts). The provider owns the socket; the view owns the document + caret.

  /** A full loro snapshot of the document - what a joining client ships first. */
  snapshot(): Uint8Array {
    return this.doc.snapshot();
  }

  /** The current oplog version, encoded. Hold it, then `exportUpdatesSince` to
   *  ship only the ops added since (the incremental wire delta). */
  oplogVersion(): Uint8Array {
    return this.doc.oplogVersion();
  }

  /** Export the ops committed since `version` (from `oplogVersion`) as a loro
   *  update delta to broadcast to peers. */
  exportUpdatesSince(version: Uint8Array): Uint8Array {
    return this.doc.exportUpdatesSince(version);
  }

  /** Merge a remote loro blob (snapshot or delta) and repaint, keeping the local
   *  caret on the same character: capture it as an edit-stable anchor before the
   *  merge, resolve it back after (a concurrent insert/delete before the caret
   *  would otherwise jump it). */
  applyRemoteUpdate(bytes: Uint8Array): void {
    const focus = this.sel?.focus ?? null;
    let anchor: Uint8Array | null = null;
    if (focus) {
      try {
        anchor = this.doc.caretAnchor(focus.para, focus.off);
      } catch {
        anchor = null; // caret not in the body story; remap is best-effort
      }
    }
    this.doc.merge(bytes);
    if (focus && anchor) {
      const pos = this.doc.resolveAnchor(anchor); // [para, off] | undefined
      if (pos && pos.length === 2) {
        const p = { para: pos[0]!, off: pos[1]! };
        this.sel = { anchor: { ...p }, focus: { ...p } };
      }
    }
    this.fullRender();
  }

  /** The current selection (collapsed = caret), for presence. */
  get selection(): Selection | null {
    return this.sel;
  }

  /** Encode the local caret as an edit-stable anchor to broadcast as presence,
   *  or null when there is no body caret. */
  localCaretAnchor(): Uint8Array | null {
    const focus = this.sel?.focus;
    if (!focus) return null;
    try {
      return this.doc.caretAnchor(focus.para, focus.off);
    } catch {
      return null;
    }
  }

  /** Encode the current selection as an edit-stable anchored range (for the
   *  inline select->ask: the agent edits exactly that span). Null when there is
   *  no non-empty selection. */
  selectionAnchor(): Uint8Array | null {
    const s = this.sel;
    if (!s) return null;
    const { anchor, focus } = s;
    if (anchor.para === focus.para && anchor.off === focus.off) return null;
    // Order the endpoints (a selection can run backwards).
    const fwd = anchor.para < focus.para || (anchor.para === focus.para && anchor.off <= focus.off);
    const [a, b] = fwd ? [anchor, focus] : [focus, anchor];
    try {
      return this.doc.anchorRange(a.para, a.off, b.para, b.off);
    } catch {
      return null;
    }
  }

  /** Resolve an anchored range (from `selectionAnchor`) to current
   *  `[para1, off1, para2, off2]`, or null. */
  resolveRange(range: Uint8Array): number[] | null {
    const r = this.doc.resolveRange(range);
    return r ? Array.from(r) : null;
  }




  /** The body insertion point: the live caret when it's in the body story, else the document start
   *  (pictures live on the body only - the engine rejects a header/footer anchor). */
  private bodyCaret(): CaretPos {
    const f = this.sel?.focus;
    if (f && f.para < BODY_PARA_LIMIT) return { para: f.para, off: f.off };
    return { para: 0, off: 0 };
  }

  // ── formatting commands (operate on the current selection) ───────────────────

  /** The resolved formatting of the current selection (drives toolbar state). Null if no selection
   *  or not over a single paragraph yet. Booleans are tri-state: `undefined` = the selection mixes
   *  both. */
  selectionFormat(): {
    bold?: boolean;
    italic?: boolean;
    underline?: boolean;
    strike?: boolean;
    size?: number;
    color?: string;
    font?: string;
    highlight?: string;
    vertAlign?: string;
  } | null {
    // Reflect the selection's format, or - when the caret is just placed with no range - the run at
    // the caret (like Word), so the Font / Size boxes always show the current text's values. (Edit
    // commands still require a real range; this fallback is query-only.)
    const s =
      this.formatSpan() ??
      (this.sel
        ? { para: this.sel.focus.para, s: this.sel.focus.off, e: this.sel.focus.off }
        : null);
    if (!s) return null;
    const f = this.doc.selectionFormat(s.para, s.s, s.e);
    return {
      bold: f.boldMixed ? undefined : f.bold,
      italic: f.italicMixed ? undefined : f.italic,
      underline: f.underlineMixed ? undefined : f.underline,
      strike: f.strikeMixed ? undefined : f.strike,
      size: f.size || undefined,
      color: f.color || undefined,
      font: f.font || undefined,
      highlight: f.highlight || undefined,
      vertAlign: f.vertAlign || undefined,
    };
  }

  /** Toggle bold on the selection (off if all-bold, else on). No-op without a selection. */
  toggleBold(): void {
    this.toggleRun('bold', (p, s, e, on) => this.doc.formatBold(p, s, e, on));
  }
  toggleItalic(): void {
    this.toggleRun('italic', (p, s, e, on) => this.doc.formatItalic(p, s, e, on));
  }
  toggleUnderline(): void {
    this.toggleRun('underline', (p, s, e, on) => this.doc.formatUnderline(p, s, e, on));
  }
  toggleStrike(): void {
    this.toggleRun('strike', (p, s, e, on) => this.doc.formatStrike(p, s, e, on));
  }

  /** Set the font size (points) on the selection. */
  setFontSize(points: number): void {
    this.applyFmt((p, s, e) => this.doc.formatSize(p, s, e, Math.round(points * 2)));
  }
  /** Bump the selection's font size up one step on Word's size ladder (Grow Font). */
  growFont(): void {
    this.stepFont(1);
  }
  /** Drop the selection's font size down one step (Shrink Font). */
  shrinkFont(): void {
    this.stepFont(-1);
  }
  private stepFont(dir: 1 | -1): void {
    const cur = (this.selectionFormat()?.size ?? 22) / 2; // half-points -> points
    let next = cur;
    if (dir > 0)
      next =
        FONT_SIZE_LADDER.find((s) => s > cur) ?? FONT_SIZE_LADDER[FONT_SIZE_LADDER.length - 1]!;
    else {
      const below = FONT_SIZE_LADDER.filter((s) => s < cur);
      next = below.length ? below[below.length - 1]! : FONT_SIZE_LADDER[0]!;
    }
    if (next !== cur) this.setFontSize(next);
  }
  /** Toggle a text highlight on the selection (Word's default yellow); off if already highlighted. */
  toggleHighlight(color = 'yellow'): void {
    const on = this.selectionFormat()?.highlight;
    this.applyFmt((p, s, e) => this.doc.formatHighlight(p, s, e, on ? '' : color));
  }
  /** Apply a specific highlight color to the selection (an OOXML highlight name; `''`/`'none'` removes
   *  it). Drives the highlighter palette - the main button reuses the last picked color. */
  setHighlight(color: string): void {
    const val = color === 'none' ? '' : color;
    this.applyFmt((p, s, e) => this.doc.formatHighlight(p, s, e, val));
  }
  /** Toggle superscript on the selection (off if already superscript). */
  toggleSuperscript(): void {
    const cur = this.selectionFormat()?.vertAlign;
    this.applyFmt((p, s, e) =>
      this.doc.formatVertAlign(p, s, e, cur === 'superscript' ? '' : 'superscript'),
    );
  }
  /** Toggle subscript on the selection (off if already subscript). */
  toggleSubscript(): void {
    const cur = this.selectionFormat()?.vertAlign;
    this.applyFmt((p, s, e) =>
      this.doc.formatVertAlign(p, s, e, cur === 'subscript' ? '' : 'subscript'),
    );
  }
  /** Clear all inline run formatting on the selection (the Clear Formatting eraser). */
  clearFormatting(): void {
    this.applyFmt((p, s, e) => this.doc.clearFormatting(p, s, e));
  }
  /** Set the text color (`RRGGBB` hex, with or without `#`) on the selection. */
  setColor(hex: string): void {
    const v = hex.replace(/^#/, '');
    this.applyFmt((p, s, e) => this.doc.formatColor(p, s, e, v));
  }
  /** Set the font family on the selection. */
  setFontFamily(family: string): void {
    this.applyFmt((p, s, e) => this.doc.formatFont(p, s, e, family));
  }

  // ── clipboard (plain text) ───────────────────────────────────────────────────

  /** The selected text, across as many paragraphs as the selection spans (joined with newlines);
   *  empty when collapsed.
   *
   *  This deliberately does NOT go through [`selSpan`]. That helper answers a different question -
   *  "is this a selection I can EDIT in place?" - and single-paragraph is a real constraint on the
   *  edit path. Reading the selection has no such limit: `selectionRects` already paints a
   *  cross-paragraph selection, so gating the text on `selSpan` meant a user could select three
   *  paragraphs, watch them highlight, press Ctrl+C, and get nothing at all - silently, since the
   *  empty return leaves the browser's default to copy the (invisible) canvas. */
  selectionText(): string {
    if (!this.sel) return '';
    // Selection order is anchor->focus, which is backwards when dragged up the page.
    const { anchor, focus } = this.sel;
    const fwd =
      anchor.para < focus.para || (anchor.para === focus.para && anchor.off <= focus.off);
    const a = fwd ? anchor : focus;
    const b = fwd ? focus : anchor;
    // Code points, not UTF-16 units: offsets are code-point indices everywhere in the view.
    if (a.para === b.para) {
      if (a.off === b.off) return '';
      return [...this.doc.paragraphText(a.para)].slice(a.off, b.off).join('');
    }
    const out: string[] = [[...this.doc.paragraphText(a.para)].slice(a.off).join('')];
    for (let p = a.para + 1; p < b.para; p++) out.push(this.doc.paragraphText(p));
    out.push([...this.doc.paragraphText(b.para)].slice(0, b.off).join(''));
    return out.join('\n');
  }

  /** Replace the current selection (or insert at the caret) with `text`. Newlines collapse to
   *  spaces for now - multi-paragraph paste is a follow-up. */
  replaceSelection(text: string): void {
    const clean = text.replace(/\r\n|\r|\n/g, ' ');
    const span = this.selSpan();
    const at = span ? { para: span.para, off: span.s } : this.sel?.focus;
    if (!at) return;
    this.edit(
      () => {
        if (span) this.doc.deleteRange(span.para, span.s, span.e);
        if (clean) this.doc.insertText(at.para, at.off, clean);
      },
      { para: at.para, off: at.off + [...clean].length },
    );
  }

  // ── paragraph formatting (apply to the caret's paragraph) ────────────────────

  /** Set the alignment of every selected paragraph. */
  setAlignment(align: 'left' | 'center' | 'right' | 'justify'): void {
    const paras = this.selectedParagraphs();
    if (!paras.length) return;
    this.formatEdit(() => {
      for (const para of paras) this.doc.setAlignment(para, align);
    });
  }

  /** Set line spacing of every selected paragraph (multiplier: 1 = single, 1.5, 2 = double). */
  setLineSpacing(multiplier: number): void {
    const paras = this.selectedParagraphs();
    if (!paras.length) return;
    const x240 = Math.round(multiplier * 240);
    this.formatEdit(() => {
      for (const para of paras) this.doc.setLineSpacing(para, x240);
    });
  }

  /** Change every selected paragraph's left indent by `deltaTwips` (each clamped at 0, from its own
   *  current indent). 1 inch = 1440. */
  indent(deltaTwips: number): void {
    const paras = this.selectedParagraphs();
    if (!paras.length) return;
    this.formatEdit(() => {
      for (const para of paras) {
        const cur = this.doc.paragraphFormat(para).indentLeft;
        this.doc.setIndentLeft(para, Math.max(0, Math.round(cur + deltaTwips)));
      }
    });
  }

  /** Set every selected paragraph's left indent (twips). */
  setIndentLeft(twips: number): void {
    const paras = this.selectedParagraphs();
    if (!paras.length) return;
    const v = Math.round(twips);
    this.formatEdit(() => {
      for (const para of paras) this.doc.setIndentLeft(para, v);
    });
  }

  /** Set every selected paragraph's right indent (twips). */
  setIndentRight(twips: number): void {
    const paras = this.selectedParagraphs();
    if (!paras.length) return;
    const v = Math.round(twips);
    this.formatEdit(() => {
      for (const para of paras) this.doc.setIndentRight(para, v);
    });
  }

  /** Set every selected paragraph's first-line indent (twips; negative = hanging). */
  setIndentFirst(twips: number): void {
    const paras = this.selectedParagraphs();
    if (!paras.length) return;
    const v = Math.round(twips);
    this.formatEdit(() => {
      for (const para of paras) this.doc.setIndentFirst(para, v);
    });
  }

  /** Put the current paragraph into list `numId` at `level` (default 0) - recorded as a tracked
   *  `w:pPrChange` when Track-Changes is on (a numbering change is a paragraph-property change), and
   *  surfaced in the reviewing pane. `numId` must reference a list defined in the document's
   *  numbering. */
  setNumbering(numId: number, level = 0): void {
    if (!this.sel) return;
    const para = this.sel.focus.para;
    this.formatEdit(() => this.doc.setNumbering(para, numId, level));
  }

  /** Remove the current paragraph from any list - tracked when Track-Changes is on. */
  clearNumbering(): void {
    if (!this.sel) return;
    const para = this.sel.focus.para;
    this.formatEdit(() => this.doc.setNumbering(para, -1, -1));
  }

  /** The current paragraph's list id (`w:numPr/w:numId`), or -1 when it isn't in a list. */
  currentNumId(): number {
    if (!this.sel) return -1;
    return this.doc.paragraphNumId(this.sel.focus.para);
  }

  /** Toggle the current paragraph between a bullet list and no list (re-clicking clears; a numbered
   *  paragraph switches to bullets) - tracked when Track-Changes is on. */
  toggleBullets(): void {
    if (!this.sel) return;
    const para = this.sel.focus.para;
    if (this.doc.paragraphListKind(para) === 'bullet') this.clearNumbering();
    else this.formatEdit(() => this.doc.applyList(para, true));
  }

  /** Toggle the current paragraph between a numbered list and no list (re-clicking clears; a bulleted
   *  paragraph switches to numbers) - tracked when Track-Changes is on. */
  toggleNumbering(): void {
    if (!this.sel) return;
    const para = this.sel.focus.para;
    if (this.doc.paragraphListKind(para) === 'number') this.clearNumbering();
    else this.formatEdit(() => this.doc.applyList(para, false));
  }

  /** The current paragraph's list kind: 'bullet', 'number', or '' (not a list) - toolbar toggle state. */
  currentListKind(): string {
    if (!this.sel) return '';
    return this.doc.paragraphListKind(this.sel.focus.para);
  }

  /** Apply a numbered list with a specific number format to the caret paragraph - `numFmt` is an OOXML
   *  token ('decimal' | 'lowerLetter' | 'upperLetter' | 'lowerRoman' | 'upperRoman'). The Numbering
   *  format picker. Tracked when Track-Changes is on. */
  applyListFormat(numFmt: string): void {
    if (!this.sel) return;
    const para = this.sel.focus.para;
    this.formatEdit(() => this.doc.applyListFormat(para, numFmt));
  }

  /** The caret paragraph's list level-0 number format ('decimal'/'lowerRoman'/'bullet'/...), or '' when
   *  it isn't in a list - lets the Numbering picker check the active format. */
  currentListFormat(): string {
    if (!this.sel) return '';
    return this.doc.paragraphListFormat(this.sel.focus.para);
  }

  /** Insert a table of contents at the caret, built from the document's Heading 1-9 paragraphs (one
   *  line each, "{heading}\t{page}"), wrapped as a real Word TOC field. Returns whether a TOC was
   *  inserted (false when the document has no headings). */
  insertToc(): boolean {
    if (!this.sel) return false;
    const para = this.sel.focus.para;
    let inserted = false;
    this.formatEdit(() => {
      inserted = this.doc.insertToc(para);
    });
    return inserted;
  }

  /** Update (regenerate) the document's table of contents in place from the current headings + page
   *  numbers (Word's F9). When there's no TOC yet, inserts one at the caret. Returns whether a TOC was
   *  written. */
  updateToc(): boolean {
    if (!this.sel) return false;
    const para = this.sel.focus.para;
    let ok = false;
    this.formatEdit(() => {
      ok = this.doc.updateToc(para);
    });
    return ok;
  }

  // ── bookmarks ──────────────────────────────────────────────────────────────

  /** Add a named bookmark over the current selection (References > Bookmark). Prompts for a name,
   *  sanitized to Word's rules (letters/digits/underscore, letter-initial, <= 40 chars). Needs selected
   *  text to anchor (v1 - a zero-width bookmark can't carry a Peritext mark). */
  async addBookmark(): Promise<void> {
    if (this.opts.mode !== 'edit' || !this.sel) return;
    const span = this.selSpan();
    if (!span) return;
    const raw = await this.pop.promptInput({
      title: 'Insert bookmark',
      placeholder: 'Bookmark name',
      hint: 'Letters, digits and underscores only; must begin with a letter.',
      okLabel: 'Add',
    });
    if (raw == null) return; // cancelled
    const name = sanitizeBookmarkName(raw);
    if (!name) return;
    this.formatEdit(() => this.doc.addBookmark(span.para, span.s, span.e, name));
  }

  // ── hyperlinks ───────────────────────────────────────────────────────────────

  /** Add or edit a hyperlink on the current selection (Ctrl+K). Prompts for the address, prefilled
   *  with the caret's current link; a blank address removes the link. A new link needs selected text. */
  async addHyperlink(): Promise<void> {
    if (this.opts.mode !== 'edit' || !this.sel) return;
    const existing = this.linkAt();
    const url = await this.pop.promptInput({
      title: existing ? 'Edit hyperlink' : 'Insert hyperlink',
      value: existing || 'https://',
      placeholder: 'https://example.com or #BookmarkName',
      hint: 'Link to the web (https://…) or within this document (#BookmarkName). Leave blank to remove.',
      okLabel: existing ? 'Update' : 'Insert',
    });
    if (url == null) return; // cancelled
    const target = url.trim();
    if (!target) {
      this.removeHyperlink();
      return;
    }
    const span = this.selSpan();
    if (!span) return; // v1: creating a link needs selected text to wrap
    this.formatEdit(() => this.doc.addHyperlink(span.para, span.s, span.e, target));
  }

  /** Remove the hyperlink at the caret. */
  removeHyperlink(): void {
    if (this.opts.mode !== 'edit' || !this.sel) return;
    const f = this.sel.focus;
    this.formatEdit(() => this.doc.removeHyperlink(f.para, f.off));
  }

  /** The hyperlink target at the caret ('' = none) - an external URL or an internal `#bookmark`. */
  linkAt(): string {
    if (!this.sel) return '';
    const f = this.sel.focus;
    return this.doc.linkAt(f.para, f.off);
  }

  /** Follow a hyperlink target: an external URL opens in a new tab; an internal `#bookmark` jumps to it. */
  private followLink(target: string): void {
    if (target.startsWith('#')) {
      const para = this.doc.bookmarkParagraph(target.slice(1));
      if (para >= 0) {
        this.setSelection({ para, off: 0 });
        this.drawOverlay();
      }
    } else if (typeof window !== 'undefined') {
      window.open(target, '_blank', 'noopener,noreferrer');
    }
  }

  /** Demote (`delta = +1`) or promote (`delta = -1`) the caret paragraph's list item one level, like
   *  Word's Tab / Shift+Tab in a list. Promoting past level 0 removes it from the list. Levels are
   *  clamped to 0-8 (Word's nine outline levels). Returns whether the caret was in a list (so the key
   *  handler knows to swallow Tab); a no-op outside a list returns false so Tab keeps its default. */
  changeListLevel(delta: number): boolean {
    if (this.opts.mode !== 'edit' || !this.sel) return false;
    const para = this.sel.focus.para;
    const cur = this.doc.paragraphListLevel(para);
    if (cur < 0) return false; // not in a list
    const next = cur + delta;
    if (next < 0) this.clearNumbering();
    else if (next <= 8) this.setNumbering(this.doc.paragraphNumId(para), next);
    // next > 8: already at the deepest level - swallow Tab without changing anything.
    return true;
  }

  // ── paragraph styles (Home tab Styles gallery) ───────────────────────────────

  /** The Styles gallery: one entry per quick style the document defines (Title / Heading / Subtitle /
   *  Normal / ...), each carrying its resolved preview formatting so the gallery can render the name in
   *  its own look. `size` is half-points (0 = inherit); `color` is a hex string ('' = inherit). */
  styleGallery(): StyleGalleryItem[] {
    try {
      return JSON.parse(this.doc.styleGallery()) as StyleGalleryItem[];
    } catch {
      return [];
    }
  }

  /** The caret paragraph's style id ('' = Normal / default) - for the Styles dropdown state. */
  currentStyle(): string {
    if (!this.sel) return '';
    return this.doc.paragraphStyle(this.sel.focus.para);
  }

  /** Apply a named paragraph style ('' = clear to Normal) to every selected paragraph - recorded as a
   *  tracked `w:pPrChange` per paragraph when Track-Changes is on (a style change is a paragraph-
   *  property change). */
  setParagraphStyle(id: string): void {
    const paras = this.selectedParagraphs();
    if (!paras.length) return;
    this.formatEdit(() => {
      for (const para of paras) this.doc.setParagraphStyle(para, id);
    });
  }

  /** The resolved definition of style `id`, for prefilling the Modify-Style dialog (what the style
   *  currently renders at, through its `basedOn` chain with any runtime edit folded in). */
  resolveStyleProps(id: string): ResolvedStyleProps {
    try {
      return JSON.parse(this.doc.resolveStyleProps(id)) as ResolvedStyleProps;
    } catch {
      return {
        size: 0,
        bold: false,
        italic: false,
        color: '',
        font: '',
        lineSpacing: 0,
        lineRule: 'auto',
        spaceBefore: -1,
        spaceAfter: -1,
        align: '',
      };
    }
  }

  /** Edit style `id`'s *definition* (Word's Modify-Style): every paragraph resolving through the style
   *  re-renders with the new properties. Per-field - only the fields present in `edit` change; the rest
   *  keep inheriting. Direct, not a tracked revision (Word doesn't redline a style-definition change). */
  modifyStyle(id: string, edit: StyleEdit): void {
    const num = (n: number | undefined): number => (n === undefined ? -1 : n);
    const tri = (b: boolean | undefined): number => (b === undefined ? -1 : b ? 1 : 0);
    const str = (s: string | undefined): string => s ?? '';
    this.formatEdit(() => {
      this.doc.setStyleProps(
        id,
        num(edit.size),
        tri(edit.bold),
        tri(edit.italic),
        str(edit.color),
        str(edit.font),
        num(edit.lineSpacing),
        num(edit.spaceBefore),
        num(edit.spaceAfter),
        str(edit.align),
        str(edit.lineRule),
      );
    });
  }

  /** Word's "Update <Style> to Match Selection": fold the caret selection's *current* formatting
   *  (font / size / bold / italic / colour from the run, line-spacing from the paragraph) into the
   *  paragraph's own style definition, so every paragraph using it adopts the look. Captures the
   *  subset our `StyleProps` model covers (alignment / indents on a style are not yet covered). No-op
   *  when there's no caret. */
  updateStyleToMatchSelection(): void {
    if (!this.sel) return;
    const styleId = this.currentStyle() || 'Normal';
    const f = this.selectionFormat();
    const p = this.paragraphFormat();
    const edit: StyleEdit = {};
    if (f?.font) edit.font = f.font;
    if (f?.size) edit.size = f.size; // half-points
    if (f?.bold !== undefined) edit.bold = f.bold;
    if (f?.italic !== undefined) edit.italic = f.italic;
    if (f?.color) edit.color = f.color;
    if (p?.lineSpacing) edit.lineSpacing = p.lineSpacing;
    if (p?.align) edit.align = p.align;
    if (Object.keys(edit).length > 0) this.modifyStyle(styleId, edit);
  }

  /** Word's "Create a Style" / "Save Selection as a New Quick Style": make a new paragraph style
   *  named `name`, based on the caret paragraph's current style, capturing the selection's formatting,
   *  and apply it to the selected paragraph(s). Returns the minted style id (or null on no-op). */
  createStyleFromSelection(name: string): string | null {
    if (!this.sel || !name.trim()) return null;
    const f = this.selectionFormat();
    const p = this.paragraphFormat();
    const basedOn = this.currentStyle() || 'Normal';
    const num = (n: number | undefined): number => (n === undefined ? -1 : n);
    const tri = (b: boolean | undefined): number => (b === undefined ? -1 : b ? 1 : 0);
    const str = (s: string | undefined): string => s ?? '';
    let id: string | null = null;
    this.formatEdit(() => {
      id = this.doc.addStyle(
        name.trim(),
        basedOn,
        num(f?.size),
        tri(f?.bold),
        tri(f?.italic),
        str(f?.color),
        str(f?.font),
        num(p?.lineSpacing),
        -1,
        -1,
        str(p?.align),
        '', // new styles use auto line spacing
      );
      if (id) for (const para of this.selectedParagraphs()) this.doc.setParagraphStyle(para, id);
    });
    return id;
  }

  /** The current paragraph's formatting (for the Paragraph-group toolbar state). */
  paragraphFormat(): {
    align: string;
    lineSpacing: number;
    indentLeft: number;
    indentRight: number;
    indentFirst: number;
  } | null {
    if (!this.sel) return null;
    const p = this.doc.paragraphFormat(this.sel.focus.para);
    return {
      align: p.align,
      lineSpacing: p.lineSpacing,
      indentLeft: p.indentLeft,
      indentRight: p.indentRight,
      indentFirst: p.indentFirst,
    };
  }

  // ── page commands (Layout tab) ───────────────────────────────────────────────

  /** Set the page size to a named preset or explicit twips. Letter = 8.5x11in, A4 = 210x297mm. */
  setPageSize(size: 'letter' | 'a4' | { width: number; height: number }): void {
    const dims =
      size === 'letter'
        ? { width: 12240, height: 15840 }
        : size === 'a4'
          ? { width: 11906, height: 16838 }
          : size;
    this.doc.setPageSize(dims.width, dims.height);
    this.fullRender();
  }

  /** Set page margins in twips (1 inch = 1440). */
  setMargins(top: number, right: number, bottom: number, left: number): void {
    this.doc.setMargins(top, right, bottom, left);
    this.fullRender();
  }

  /** Set the color painted between / around page sheets (any CSS color the canvas accepts). Match it
   *  to the host's page-stage backdrop so the gutter and the surrounding app are one continuous
   *  surface (the Workspace does this, tracking the theme). No-op if unchanged. */
  setGutter(color: string): void {
    if (color === this.opts.gutter) return;
    this.opts.gutter = color;
    this.fullRender();
  }

  /** Set orientation (true = landscape). */
  setLandscape(landscape: boolean): void {
    this.doc.setLandscape(landscape);
    this.fullRender();
  }

  /** Page geometry in twips, including header/footer distances. */
  pageGeometry(): {
    width: number;
    height: number;
    marginTop: number;
    marginRight: number;
    marginBottom: number;
    marginLeft: number;
    headerDist: number;
    footerDist: number;
  } {
    const g = this.doc.pageGeometry();
    return {
      width: g[0]!,
      height: g[1]!,
      marginTop: g[2]!,
      marginRight: g[3]!,
      marginBottom: g[4]!,
      marginLeft: g[5]!,
      headerDist: g[6]!,
      footerDist: g[7]!,
    };
  }

  // ── header / footer (plain-text edit, v1) ────────────────────────────────────

  /** The default header as plain text. */
  headerText(): string {
    return this.doc.headerText();
  }
  /** The default footer as plain text. */
  footerText(): string {
    return this.doc.footerText();
  }
  /** Replace the default header with plain `text`, then re-render. */
  setHeader(text: string): void {
    this.doc.setHeaderText(text);
    this.fullRender();
    this.notify();
  }
  /** Replace the default footer with plain `text`, then re-render. */
  setFooter(text: string): void {
    this.doc.setFooterText(text);
    this.fullRender();
    this.notify();
  }

  /** Enter the header for editing (Word's Insert > Header): create an empty default header if the
   *  document has none, then drop the caret into its first paragraph. The header is edited in place
   *  like the body - typing, formatting, and fields all route to the header story. */
  editHeader(): void {
    if (this.opts.mode !== 'edit') return;
    const para = this.doc.ensureHeader();
    this.fullRender(); // a freshly-created header adds a caret line + may grow the top margin
    this.setSelection({ para, off: 0 });
    this.keys.focus({ preventScroll: true });
  }
  /** Enter the footer for editing (see {@link editHeader}). */
  editFooter(): void {
    if (this.opts.mode !== 'edit') return;
    const para = this.doc.ensureFooter();
    this.fullRender();
    this.setSelection({ para, off: 0 });
    this.keys.focus({ preventScroll: true });
  }

  // ── tracked-change display (Word's "Display for Review") ──────────────────────

  /** The current tracked-change display mode. */
  get trackDisplayMode(): TrackDisplay {
    return this.trackDisplay;
  }

  /** Set how tracked changes are displayed (`all` / `simple` / `none` / `original`) and re-render.
   *  See [`TrackDisplay`]; non-`all` modes are preview only (edit in `all`). */
  setTrackDisplay(mode: TrackDisplay): void {
    if (mode === this.trackDisplay) return;
    this.trackDisplay = mode;
    this.doc.setTrackDisplay(mode);
    this.fullRender();
    this.notify();
  }

  /** Whether revision balloons are on (Review > Show Markup > Balloons). */
  get balloonsOn(): boolean {
    return this.doc.balloonsOn();
  }

  /** Turn revision balloons on/off: tracked deletions move to right-margin bubbles (body text
   *  narrows to make room). Only visible in the All / Simple markup modes. */
  setBalloons(on: boolean): void {
    if (on === this.doc.balloonsOn()) return;
    this.doc.setBalloons(on);
    this.fullRender();
    this.notify();
  }

  // ── tracked-change authoring + review (Word's Track Changes) ──────────────────

  /** Whether Track-Changes (suggesting) mode is on. */
  get trackChangesOn(): boolean {
    return this.trackChanges;
  }

  /** Turn Track-Changes mode on/off. While on, typing / deleting author tracked changes (insertions
   *  underlined, deletions struck) attributed to the current author instead of editing directly.
   *  No-op when turning it off while tracking is locked (see [`setTrackLocked`]). */
  setTrackChanges(on: boolean): void {
    if (on === this.trackChanges) return;
    if (!on && this.doc.trackLocked()) return; // a locked document forces tracking on
    this.trackChanges = on;
    this.pendingMove = null; // a half-finished move can't survive a tracking-mode change
    this.doc.setTrackChanges(on);
    this.notify();
  }

  /** Whether Track-Changes is locked on (Review > Lock Tracking). */
  get trackLocked(): boolean {
    return this.doc.trackLocked();
  }

  /** Lock / unlock Track-Changes: while locked it can't be turned off (and is forced on). */
  setTrackLocked(locked: boolean): void {
    this.doc.setTrackLocked(locked);
    if (locked && !this.trackChanges) {
      this.trackChanges = true;
    }
    this.notify();
  }

  // ── reviewer filter (Word's "Show Markup by reviewer") ───────────────────────

  /** Every reviewer who authored a tracked change or comment, with their colour + whether their
   *  markup is currently shown - for a per-author legend / filter (the reviewing pane uses this). */
  reviewers(): { name: string; color: string; visible: boolean }[] {
    try {
      const list = JSON.parse(this.doc.reviewers()) as {
        name: string;
        color: string;
        hidden: boolean;
      }[];
      this.hiddenReviewers = new Set(list.filter((r) => r.hidden).map((r) => r.name));
      return list.map((r) => ({ name: r.name, color: r.color, visible: !r.hidden }));
    } catch {
      return [];
    }
  }

  /** Show / hide a reviewer's markup (display-only). Re-renders + refreshes the reviewing pane. */
  setReviewerVisible(name: string, visible: boolean): void {
    if (visible) this.hiddenReviewers.delete(name);
    else this.hiddenReviewers.add(name);
    this.doc.setReviewerHidden(name, !visible);
    this.fullRender();
    this.notify();
  }

  /** Set the current author for tracked changes (id for the audit trail, display name as `w:author`). */
  setAuthor(id: string, name: string): void {
    this.author = { id, name };
    this.doc.setAuthor(id, name);
  }




  /** Accept the tracked change at the caret (no-op if the caret isn't on one). */
  acceptChange(): void {
    this.resolveAtCaret(true);
  }
  /** Reject the tracked change at the caret. */
  rejectChange(): void {
    this.resolveAtCaret(false);
  }
  /** Accept every tracked change in the document. */
  acceptAll(): void {
    this.resolveAll(true);
  }
  /** Reject every tracked change in the document. */
  rejectAll(): void {
    this.resolveAll(false);
  }

  /** Move the caret to the next tracked change (wraps); no-op when there are none. */
  nextChange(): void {
    this.gotoChange(true);
  }
  /** Move the caret to the previous tracked change (wraps). */
  prevChange(): void {
    this.gotoChange(false);
  }

  // ── comments ─────────────────────────────────────────────────────────────────

  /** Whether a new comment could be anchored right now (drives ribbon state). Like Word, this is true
   *  whenever there's a caret on something anchorable - a range selection, or a word/char at the caret
   *  - not only when a range is selected. (A comment mark needs a non-empty range; an empty paragraph
   *  with no word to grab is the only case that stays disabled.) */
  canAddComment(): boolean {
    return this.opts.mode === 'edit' && this.commentRange() != null;
  }

  /** The range a new comment would anchor to: the selection if there is one, else the word at the
   *  caret (Word grabs the current word when you comment with no selection), else one adjacent char so
   *  the anchor is never empty. Null only on an empty paragraph with nothing to grab. */
  private commentRange(): { anchor: CaretPos; focus: CaretPos } | null {
    const span = this.selSpan();
    if (span)
      return { anchor: { para: span.para, off: span.s }, focus: { para: span.para, off: span.e } };
    if (!this.sel) return null;
    const { para, off } = this.sel.focus;
    const w = this.wordRangeAt(para, off);
    if (!w) return null;
    return { anchor: { para, off: w.s }, focus: { para, off: w.e } };
  }

  /** Word boundaries around `off` in paragraph `para`. Expands over word chars (letters / digits /
   *  underscore) both ways; if the caret sits between non-word chars, grabs one char to the right (else
   *  left) so a comment always has a visible anchor. Null on an empty paragraph. */
  private wordRangeAt(para: number, off: number): { s: number; e: number } | null {
    const text = [...this.doc.paragraphText(para)];
    if (!text.length) return null;
    const isWord = (ch: string | undefined): boolean => ch != null && /[\p{L}\p{N}_]/u.test(ch);
    let s = off;
    let e = off;
    while (s > 0 && isWord(text[s - 1])) s--;
    while (e < text.length && isWord(text[e])) e++;
    if (e > s) return { s, e };
    if (off < text.length) return { s: off, e: off + 1 };
    if (off > 0) return { s: off - 1, e: off };
    return null;
  }

  /** The number of comments in the document (for chrome / "Show Comments" badges). */
  commentCount(): number {
    try {
      return (JSON.parse(this.doc.listComments()) as CommentJson[]).length;
    } catch {
      return 0;
    }
  }

  /** Start a new comment: opens a small composer near the caret, anchored on the selection - or, with
   *  no selection, on the word at the caret (Word's behavior). No-op only when there's nothing to
   *  anchor to (empty paragraph). */
  newComment(): void {
    if (this.opts.mode !== 'edit') return;
    const range = this.commentRange();
    if (!range) return;
    const { x, y } = this.caretClientXY(range.focus.para, range.focus.off);
    this.pop.showCommentComposer(x, y, range.anchor, range.focus);
  }

  /** Move to the next comment anchor (wraps) + open its thread; no-op when there are none. */
  nextComment(): void {
    this.gotoComment(true);
  }
  /** Move to the previous comment anchor (wraps) + open its thread. */
  prevComment(): void {
    this.gotoComment(false);
  }

  /** Delete the comment thread anchored at the caret (no-op if the caret isn't in a comment). */
  deleteCommentAtCaret(): void {
    if (!this.sel) return;
    const ids = this.commentsAt(this.sel.focus.para, this.sel.focus.off);
    if (!ids.length) return;
    try {
      for (const id of ids) this.doc.deleteComment(id);
      this.pop.closeCommentPopup();
      this.afterResolve();
    } catch (err) {
      console.error('scriptor delete-comment error', err);
    }
  }

  // ── reviewing pane data + actions ────────────────────────────────────────────

  /** Every tracked change + top-level comment in the document, in document order - the reviewing
   *  pane's model. Changes carry their kind/text; comments carry `resolved`. Replies are shown inside
   *  the thread popover, not as top-level rows. */
  reviewItems(): ReviewItem[] {
    const items: ReviewItem[] = [];
    try {
      const changes = JSON.parse(this.doc.listChanges()) as ReviewItem[];
      for (const c of changes) items.push(c);
    } catch {
      /* engine error -> no changes */
    }
    try {
      const comments = JSON.parse(this.doc.listComments()) as CommentJson[];
      for (const c of comments) {
        if (c.parent != null) continue; // replies live inside the thread popover
        items.push({
          kind: 'comment',
          id: c.id,
          author: c.author,
          date: c.date,
          text: c.text,
          para: c.para,
          off: c.off,
          resolved: c.resolved,
        });
      }
    } catch {
      /* engine error -> no comments */
    }
    const ord = (n: number): number => (n < 0 ? Number.POSITIVE_INFINITY : n); // orphan anchors last
    // Drop rows for reviewers whose markup is filtered out (R3) - same authors the canvas suppresses.
    const visible = this.hiddenReviewers.size
      ? items.filter((i) => !this.hiddenReviewers.has(i.author))
      : items;
    visible.sort((a, b) => ord(a.para) - ord(b.para) || a.off - b.off || a.id - b.id);
    return visible;
  }

  /** Select + scroll to a review item (and open the thread popover for a comment). */
  revealReviewItem(item: ReviewItem): void {
    if (item.para < 0) return;
    const len = item.kind === 'comment' ? 0 : [...item.text].length;
    if (len > 0) {
      this.setSelection(
        { para: item.para, off: item.off + len },
        { para: item.para, off: item.off },
      );
    } else {
      this.setSelection({ para: item.para, off: item.off });
    }
    this.scrollCaretIntoView();
    this.drawOverlay();
    if (item.kind === 'comment') {
      const ids = this.commentsAt(item.para, item.off);
      if (ids.length) {
        const { x, y } = this.caretClientXY(item.para, item.off);
        this.pop.showCommentThread(x, y, ids);
      }
    }
    this.keys.focus({ preventScroll: true });
  }

  /** Accept a change review item (no-op for a comment). */
  acceptReviewItem(item: ReviewItem): void {
    if (item.kind !== 'comment') this.resolveRevision(item.para, item.id, true);
  }
  /** Reject a change review item (no-op for a comment). */
  rejectReviewItem(item: ReviewItem): void {
    if (item.kind !== 'comment') this.resolveRevision(item.para, item.id, false);
  }
  /** Toggle a comment item's resolved state (no-op for a change). */
  resolveReviewItem(item: ReviewItem): void {
    if (item.kind !== 'comment') return;
    try {
      this.doc.resolveComment(item.id, !item.resolved);
      this.afterResolve();
    } catch (err) {
      console.error('scriptor resolve-comment error', err);
    }
  }
  /** Delete a comment item's thread (no-op for a change - use reject). */
  deleteReviewItem(item: ReviewItem): void {
    if (item.kind !== 'comment') return;
    try {
      this.doc.deleteComment(item.id);
      this.afterResolve();
    } catch (err) {
      console.error('scriptor delete-comment error', err);
    }
  }

  private gotoComment(forward: boolean): void {
    const f = this.sel?.focus ?? { para: 0, off: 0 };
    const r = forward ? this.doc.nextComment(f.para, f.off) : this.doc.prevComment(f.para, f.off);
    if (r.length !== 2) return; // no comments anywhere
    const para = r[0]!;
    const off = r[1]!;
    this.setSelection({ para, off });
    this.scrollCaretIntoView();
    this.drawOverlay();
    const ids = this.commentsAt(para, off);
    if (ids.length) {
      const { x, y } = this.caretClientXY(para, off);
      this.pop.showCommentThread(x, y, ids);
    }
    this.keys.focus({ preventScroll: true });
  }

  /** Client (screen) coords just below a caret position - for anchoring comment popovers. */
  private caretClientXY(para: number, off: number): { x: number; y: number } {
    const c = this.doc.caretRect(para, off); // [x, y, h] device px in document space
    const r = this.sheet.getBoundingClientRect();
    const sx = r.width / Math.max(1, this.deviceW);
    const sy = r.height / Math.max(1, this.deviceTotalH);
    return { x: r.left + c[0]! * sx, y: r.top + (c[1]! + c[2]!) * sy + 4 };
  }

  /** The current selection's bounding box in client (screen) coordinates, for
   *  anchoring an inline prompt (Cmd-K) at the marked text. Returns null when
   *  there is no selection. A collapsed selection yields a zero-width box at the
   *  caret. Mirrors {@link caretClientXY}'s overlay-device-px -> client mapping. */
  selectionScreenRect(): { x: number; y: number; width: number; height: number } | null {
    if (!this.sel) return null;
    const { anchor, focus } = this.sel;
    const r = this.sheet.getBoundingClientRect();
    const sx = r.width / Math.max(1, this.deviceW);
    const sy = r.height / Math.max(1, this.deviceTotalH);
    const rects = this.doc.selectionRects(anchor.para, anchor.off, focus.para, focus.off);
    if (rects.length) {
      let minX = Infinity;
      let minY = Infinity;
      let maxX = -Infinity;
      let maxY = -Infinity;
      for (let i = 0; i < rects.length; i += 4) {
        const x = rects[i]!;
        const y = rects[i + 1]!;
        minX = Math.min(minX, x);
        minY = Math.min(minY, y);
        maxX = Math.max(maxX, x + rects[i + 2]!);
        maxY = Math.max(maxY, y + rects[i + 3]!);
      }
      return {
        x: r.left + minX * sx,
        y: r.top + minY * sy,
        width: (maxX - minX) * sx,
        height: (maxY - minY) * sy,
      };
    }
    const c = this.doc.caretRect(focus.para, focus.off);
    return { x: r.left + c[0]! * sx, y: r.top + c[1]! * sy, width: 0, height: c[2]! * sy };
  }

  private resolveAtCaret(accept: boolean): void {
    if (!this.sel) return;
    const { para, off } = this.sel.focus;
    try {
      const ok = accept ? this.doc.acceptChange(para, off) : this.doc.rejectChange(para, off);
      if (ok) this.afterResolve();
    } catch (err) {
      console.error('scriptor accept/reject error', err);
    }
  }

  private resolveAll(accept: boolean): void {
    try {
      const n = accept ? this.doc.acceptAll() : this.doc.rejectAll();
      if (n) this.afterResolve();
    } catch (err) {
      console.error('scriptor accept/reject-all error', err);
    }
  }

  /** Accept/reject a specific revision id in `para`'s story (body/header/footer); used by the inline
   *  click popup. The paragraph picks the region - revision ids are per-story. */
  private resolveRevision(para: number, id: number, accept: boolean): void {
    try {
      const ok = accept ? this.doc.acceptRevision(para, id) : this.doc.rejectRevision(para, id);
      if (ok) this.afterResolve();
    } catch (err) {
      console.error('scriptor resolve-revision error', err);
    }
  }

  /** After resolving change(s) the text length / pagination can change: clamp the caret + full re-render. */
  private afterResolve(): void {
    this.clampSelection();
    this.fireChange();
    this.notify();
    this.fullRender();
  }

  private gotoChange(forward: boolean): void {
    const f = this.sel?.focus ?? { para: 0, off: 0 };
    const r = forward ? this.doc.nextChange(f.para, f.off) : this.doc.prevChange(f.para, f.off);
    if (r.length !== 2) return; // no tracked changes anywhere
    const para = r[0]!;
    const off = r[1]!;
    // Select the change (Word highlights it) so the jump is visible even on a quiet page; fall back
    // to a collapsed caret if the position carries no span (e.g. an empty paragraph mark).
    const hit = this.trackAt(para, off);
    const len = hit?.text ? [...hit.text].length : 0;
    if (len > 0) this.setSelection({ para, off: off + len }, { para, off });
    else this.setSelection({ para, off });
    this.scrollCaretIntoView();
    this.drawOverlay();
    this.keys.focus({ preventScroll: true });
  }

  /** Scroll the page stack so the current caret is visible (used by change navigation). Drops a 1px
   *  marker at the caret in the (relatively-positioned) sheet and lets the browser scroll any
   *  scrollable ancestor to reveal it - robust regardless of who owns the scroll container. */
  private scrollCaretIntoView(): void {
    if (!this.sel) return;
    const c = this.doc.caretRect(this.sel.focus.para, this.sel.focus.off); // [x, y, h] device px
    if (c[2] === 0) return; // caret not located
    this.scrollRectIntoView(c, 'nearest');
  }

  /** Scroll the document so the position addressed by an edit-stable `anchor`
   *  (e.g. a remote agent's presence caret) is on screen. Used to FOLLOW an
   *  agent as it edits offscreen. Resolves the anchor against the live document;
   *  a no-op if it can't be located. `block: 'center'` keeps the followed edit
   *  comfortably in view (with surrounding context) rather than at the edge. */
  scrollToAnchor(anchor: Uint8Array): void {
    const pos = this.doc.resolveAnchor(anchor); // [para, off] | undefined
    if (!pos) return;
    const c = this.doc.caretRect(pos[0]!, pos[1]!); // [x, y, h] device px
    if (c[2] === 0) return;
    this.scrollRectIntoView(c, 'center');
  }

  /** The current viewport (client) y, in CSS px, of paragraph `para`'s top - or `null` when it can't
   *  be located (out of range / not laid out). The side-by-side comparison view reads this from both
   *  panes to keep aligned paragraphs level as either side scrolls. Uses the live sheet position
   *  (`getBoundingClientRect`) so it is correct regardless of which ancestor owns the scroll. */
  paragraphClientTop(para: number): number | null {
    let c: Float32Array;
    try {
      c = this.doc.caretRect(para, 0); // [x, y, h] device px in the full-document layout
    } catch {
      return null;
    }
    if (c[2] === 0) return null; // not located
    return this.sheet.getBoundingClientRect().top + c[1]! / this.dpr;
  }

  /** Force a full re-layout + repaint. Use after the view's container is resized/revealed by chrome
   *  the view can't observe (e.g. mounting it into a freshly-shown side-by-side split), so its paint
   *  window is recomputed against the settled layout. */
  refresh(): void {
    this.fullRender();
  }

  /** Scroll the page stack so paragraph `para` is in view - change navigation + the side-by-side
   *  "jump to change". A no-op when the paragraph can't be located. */
  scrollToParagraph(para: number, block: ScrollLogicalPosition = 'center'): void {
    let c: Float32Array;
    try {
      c = this.doc.caretRect(para, 0);
    } catch {
      return;
    }
    if (c[2] === 0) return;
    this.scrollRectIntoView(c, block);
  }

  /** Shared scroll helper: drop a 1px marker at a device-px caret rect and let
   *  the browser scroll it into view (the sheet's scroll container handles zoom
   *  + pagination), then remove it. */
  private scrollRectIntoView(c: Float32Array, block: ScrollLogicalPosition): void {
    const marker = document.createElement('div');
    marker.style.cssText =
      `position:absolute;pointer-events:none;left:${c[0]! / this.dpr}px;top:${c[1]! / this.dpr}px;` +
      `width:1px;height:${c[2]! / this.dpr}px;`;
    this.sheet.append(marker);
    marker.scrollIntoView({ block, inline: 'nearest' });
    marker.remove();
  }

  // ── table structure editing (rows + columns) ─────────────────────────────────

  /** The caret's table context (`{ row, col, rows, cols }`, cell indices), or null if the caret
   *  isn't inside a table. Lets host chrome enable/disable table commands. */
  tableContext(): { row: number; col: number; rows: number; cols: number } | null {
    if (!this.sel) return null;
    const t = this.doc.tableContext(this.sel.focus.para);
    return t.length ? { row: t[0]!, col: t[1]!, rows: t[2]!, cols: t[3]! } : null;
  }

  /** Insert a table row above / below the caret's row (no-op outside a table). */
  insertRowAbove(): void {
    this.tableOp((p) => this.doc.insertTableRow(p, false));
  }
  insertRowBelow(): void {
    this.tableOp((p) => this.doc.insertTableRow(p, true));
  }
  /** Insert a table column left / right of the caret's cell (no-op outside a table). */
  insertColumnLeft(): void {
    this.tableOp((p) => this.doc.insertTableColumn(p, false));
  }
  insertColumnRight(): void {
    this.tableOp((p) => this.doc.insertTableColumn(p, true));
  }
  /** Delete the caret's table row / column - removes the table if it empties (no-op outside one). */
  deleteRow(): void {
    this.tableOp((p) => this.doc.deleteTableRow(p));
  }
  deleteColumn(): void {
    this.tableOp((p) => this.doc.deleteTableColumn(p));
  }

  /** Move the caret's table row up / down one position (no-op at the table's edge / outside a table). */
  moveRowUp(): void {
    this.tableOp((p) => this.doc.moveTableRow(p, true));
  }
  moveRowDown(): void {
    this.tableOp((p) => this.doc.moveTableRow(p, false));
  }
  /** Move the caret's table column left / right one position (no-op at the edge / outside a table). */
  moveColumnLeft(): void {
    this.tableOp((p) => this.doc.moveTableColumn(p, true));
  }
  moveColumnRight(): void {
    this.tableOp((p) => this.doc.moveTableColumn(p, false));
  }

  /** Merge the caret's cell with the `count - 1` cells to its right (horizontal `w:gridSpan` merge);
   *  content is preserved in the surviving cell (no-op outside a table / too few cells). */
  mergeCellsRight(count = 2): void {
    this.tableOp((p) => this.doc.mergeCellsRight(p, count));
  }
  /** Split (unmerge) the caret's horizontally-merged cell back into single columns. */
  splitCellHorizontal(): void {
    this.tableOp((p) => this.doc.splitCellHorizontal(p));
  }
  /** Merge the caret's cell with the `count - 1` cells below it (vertical `w:vMerge` merge); the cells
   *  below become empty continuation cells (no-op outside a table / too few rows). */
  mergeCellsDown(count = 2): void {
    this.tableOp((p) => this.doc.mergeCellsDown(p, count));
  }
  /** Split (unmerge) the caret's vertically-merged cell. */
  splitCellVertical(): void {
    this.tableOp((p) => this.doc.splitCellVertical(p));
  }

  /** Set / clear the caret cell's shading fill (RGB hex without `#`; `''` clears it). Recorded as a
   *  tracked `w:tcPrChange` when Track-Changes is on, else applied directly (no-op outside a table). */
  setCellShading(fill: string): void {
    this.tablePropOp((p) => this.doc.setCellShading(p, fill));
  }
  /** Set / clear the caret row's height in twips (`0` clears; `exact` = exact rule, else at-least).
   *  Tracked as `w:trPrChange` when Track-Changes is on (no-op outside a table). */
  setRowHeight(twips: number, exact = false): void {
    this.tablePropOp((p) => this.doc.setRowHeight(p, twips, exact));
  }
  /** Set a uniform single-line border on every edge of the caret's table (`sizeEighths` = weight in
   *  eighths of a point, `0` clears all). Tracked as `w:tblPrChange` when on (no-op outside a table). */
  setTableBorders(sizeEighths: number, color = '000000'): void {
    this.tablePropOp((p) => this.doc.setTableBorders(p, sizeEighths, color));
  }

  /** Run a table property edit on the caret's paragraph; the engine returns whether the caret was in a
   *  table (no caret move, unlike a structural op). Applied -> full re-layout + re-paint. */
  private tablePropOp(run: (para: number) => boolean): void {
    if (!this.sel) return;
    try {
      if (!run(this.sel.focus.para)) return; // caret wasn't in a table
      this.fireChange();
      this.fullRender();
      this.notify();
      this.keys.focus({ preventScroll: true });
    } catch (err) {
      console.error('scriptor table property op error', err);
    }
  }

  /** Run a table structural op on the caret's paragraph; the engine returns the new caret paragraph
   *  (or -1 when not in a table). Structure changed -> full re-layout. */
  private tableOp(run: (para: number) => number): void {
    if (!this.sel) return;
    try {
      const at = run(this.sel.focus.para);
      if (at < 0) return; // caret wasn't in a table
      this.fireChange();
      this.setSelection({ para: at, off: 0 });
      this.fullRender();
      this.notify();
      this.keys.focus({ preventScroll: true });
    } catch (err) {
      console.error('scriptor table op error', err);
    }
  }

  // ── zoom + document stats (status bar) ───────────────────────────────────────

  /** The current zoom factor (1 = 100%). */
  get zoomLevel(): number {
    return this.zoom;
  }

  /** Set the zoom factor (clamped to 25%..400%) and re-render. */
  setZoom(factor: number): void {
    const z = Math.min(4, Math.max(0.25, factor));
    if (z === this.zoom) return;
    this.zoom = z;
    this.fullRender();
  }

  /** Number of pages in the current layout (0 before the first render). */
  pageCount(): number {
    return this.lastFingerprints.length;
  }

  /** The 1-based page the caret is on (for "Page X of N"). */
  currentPage(): number {
    if (!this.sel) return 1;
    return this.doc.paragraphPage(this.sel.focus.para) + 1;
  }

  /** Total word count across all paragraphs (computed in one pass in the engine). */
  wordCount(): number {
    return this.doc.wordCount();
  }

  // ── undo / redo ──────────────────────────────────────────────────────────────

  /** Undo the last edit (Ctrl+Z). */
  undo(): void {
    try {
      if (this.doc.undo()) this.afterHistoryChange();
    } catch (err) {
      console.error('scriptor undo error', err);
    }
  }

  /** Redo the last undone edit (Ctrl+Y / Ctrl+Shift+Z). */
  redo(): void {
    try {
      if (this.doc.redo()) this.afterHistoryChange();
    } catch (err) {
      console.error('scriptor redo error', err);
    }
  }

  /** Whether there is anything to undo / redo (for toolbar button state). */
  canUndo(): boolean {
    return this.doc.canUndo();
  }
  canRedo(): boolean {
    return this.doc.canRedo();
  }

  private afterHistoryChange(): void {
    this.clampSelection();
    this.fireChange();
    this.notify();
    this.requestRender();
  }

  /** Keep the selection within the (possibly changed) document after undo/redo - clamped within the
   *  caret's own story (body/header/footer), so a header caret can't drift into the body. */
  private clampSelection(): void {
    if (!this.sel) return;
    const clamp = (p: CaretPos): CaretPos => {
      const r = this.doc.paragraphRange(p.para);
      const base = r[0]!;
      const cnt = Math.max(1, r[1]!);
      const para = Math.min(Math.max(base, p.para), base + cnt - 1);
      const off = Math.min(Math.max(0, p.off), this.doc.paragraphLength(para));
      return { para, off };
    };
    this.sel = { anchor: clamp(this.sel.anchor), focus: clamp(this.sel.focus) };
  }

  /** The current selection as a single-paragraph range (null if collapsed or cross-paragraph). Used
   *  for the toolbar's format read-out (which reflects the focus paragraph). */
  private formatSpan(): { para: number; s: number; e: number } | null {
    return this.selSpan();
  }

  /** Every per-paragraph run span the selection covers, in order: the first paragraph from its start
   *  offset to its end, interior paragraphs in full, the last from 0 to its end offset. One entry for
   *  a single-paragraph selection; empty for a collapsed caret. This is what lets run formatting apply
   *  across a multi-paragraph selection (the engine formats one paragraph + range at a time). */
  private formatSpans(): Array<{ para: number; s: number; e: number }> {
    if (!this.sel) return [];
    const { anchor, focus } = this.sel;
    // Order the endpoints - a selection can run backwards.
    const fwd = anchor.para < focus.para || (anchor.para === focus.para && anchor.off <= focus.off);
    const [a, b] = fwd ? [anchor, focus] : [focus, anchor];
    if (a.para === b.para) {
      return a.off === b.off ? [] : [{ para: a.para, s: a.off, e: b.off }];
    }
    const spans: Array<{ para: number; s: number; e: number }> = [];
    for (let p = a.para; p <= b.para; p++) {
      const len = this.doc.paragraphLength(p);
      const s = p === a.para ? a.off : 0;
      const e = p === b.para ? Math.min(b.off, len) : len;
      if (e > s) spans.push({ para: p, s, e });
    }
    return spans;
  }

  /** The paragraph indices the selection touches (inclusive), for paragraph-level commands
   *  (alignment / line spacing / indent / style). A collapsed caret yields its one paragraph. */
  private selectedParagraphs(): number[] {
    if (!this.sel) return [];
    const lo = Math.min(this.sel.anchor.para, this.sel.focus.para);
    const hi = Math.max(this.sel.anchor.para, this.sel.focus.para);
    const out: number[] = [];
    for (let p = lo; p <= hi; p++) out.push(p);
    return out;
  }

  private toggleRun(
    attr: 'bold' | 'italic' | 'underline' | 'strike',
    cmd: (p: number, s: number, e: number, on: boolean) => void,
  ): void {
    const spans = this.formatSpans();
    if (!spans.length) return;
    // Word's toggle: turn OFF only when every covered span already has the attribute; else turn ON.
    const allOn = spans.every((sp) => this.doc.selectionFormat(sp.para, sp.s, sp.e)[attr] === true);
    const on = !allOn;
    this.formatEdit(() => {
      for (const sp of spans) cmd(sp.para, sp.s, sp.e, on);
    });
  }

  private applyFmt(cmd: (p: number, s: number, e: number) => void): void {
    const spans = this.formatSpans();
    if (!spans.length) return;
    this.formatEdit(() => {
      for (const sp of spans) cmd(sp.para, sp.s, sp.e);
    });
  }

  /** Apply a formatting mutation that KEEPS the current selection (unlike text edits, which move
   *  the caret), then refresh + notify so the toolbar reflects the new state. */
  private formatEdit(mutate: () => void): void {
    try {
      // A tracked formatting / numbering change needs the current wall-clock stamped (the engine
      // never invents time), exactly like a text edit in `edit()`.
      if (this.trackChanges) this.doc.setNow(new Date().toISOString());
      mutate();
      this.fireChange();
      this.opts.onSelectionChange?.(this.sel);
      this.notify();
      this.requestRender();
    } catch (err) {
      console.error('scriptor format error', err);
    }
  }

  /** The current mode. */
  get mode(): ScriptorMode {
    return this.opts.mode;
  }

  /** Switch between read-only and edit mode (rewires input + caret). */
  setMode(mode: ScriptorMode): void {
    if (mode === this.opts.mode) return;
    this.opts.mode = mode;
    this.applyMode();
    this.drawOverlay();
    this.notify();
  }

  /** Subscribe to state changes (selection moved, an edit happened, mode changed) - used by chrome
   *  like the ribbon to refresh its button states. Returns an unsubscribe function. */
  addListener(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private notify(): void {
    for (const fn of this.listeners) fn();
  }

  /** A monotonic id of the currently loaded document, bumped on every whole-document replacement
   *  (open / new / snapshot / a loaded compare redline). Chrome that pins state to one document - the
   *  reviewing pane's comparison scope - reads this to drop that state when the document is swapped. */
  documentEpoch(): number {
    return this.docEpoch;
  }

  /** Tear down: stop timers, remove listeners + DOM, free the WASM document. */
  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    if (this.blinkTimer) clearInterval(this.blinkTimer);
    window.removeEventListener('mousemove', this.onMouseMove);
    window.removeEventListener('mouseup', this.onMouseUp);
    window.removeEventListener('keydown', this.onZoomKey, true);
    if (this.onScrollOrResize) {
      window.removeEventListener('scroll', this.onScrollOrResize, true);
      window.removeEventListener('resize', this.onScrollOrResize);
    }
    this.pop.closeTableMenu();
    this.pop.closeTrackPopup();
    this.pop.closeCommentPopup();
    this.pop.hideTrackTip();
    this.presence.clearPresenceChips();
    this.sheet.remove();
    this.keys.remove();
    this.doc.free();
  }

  // ── mode + lifecycle ─────────────────────────────────────────────────────────

  private replaceDoc(next: ScriptorDoc): void {
    this.pop.closeCommentPopup();
    this.commentRectsCache = [];
    this.pendingMove = null;
    this.hiddenReviewers.clear(); // a fresh engine doc starts with every reviewer shown
    this.doc.free();
    this.doc = next;
    // A fresh document: no retained bytes (loadDocx re-sets them) and no edits yet.
    this.loadedBytes = null;
    this.editedSinceLoad = false;
    this.docEpoch++;
    this.syncDocState();
    this.setSelection({ para: 0, off: 0 });
    this.fullRender();
  }

  /** Fire the host's change callback and record that the document has diverged from [`loadedBytes`],
   *  so [`compareWith`] knows it can no longer use the on-disk bytes as the comparison original. */
  private fireChange(): void {
    this.editedSinceLoad = true;
    this.opts.onChange?.();
  }

  /** Push the view's author / track-changes / display state onto a (possibly fresh) engine doc. */
  private syncDocState(): void {
    this.doc.setAuthor(this.author.id, this.author.name);
    this.doc.setTrackChanges(this.trackChanges);
    // A fresh ScriptorDoc defaults to All-Markup; carry the view's chosen display mode onto it.
    if (this.trackDisplay !== 'all') this.doc.setTrackDisplay(this.trackDisplay);
  }

  private applyMode(): void {
    const editing = this.opts.mode === 'edit';
    this.caretInkCache = null; // re-resolve the theme ink (the host theme may have changed)
    this.sheet.style.cursor = editing || this.opts.selectable ? 'text' : 'default';
    if (this.blinkTimer) {
      clearInterval(this.blinkTimer);
      this.blinkTimer = null;
    }
    if (editing) {
      this.blinkTimer = setInterval(() => {
        if (!this.sel) return;
        this.caretOn = !this.caretOn;
        this.drawOverlay();
      }, 530);
    } else {
      this.caretOn = false;
    }
  }

  // ── rendering (incremental: relayout cheap, re-raster only changed pages) ─────

  private fullRender(): void {
    this.deviceW = 0; // force a re-size + full window repaint
    this.lastFingerprints = [];
    this.paintedAt = [];
    this.render();
  }

  /** Coalesce renders to one per frame; the page-diff inside keeps each cheap. */
  private requestRender(): void {
    if (this.renderScheduled) return;
    this.renderScheduled = true;
    requestAnimationFrame(() => {
      this.renderScheduled = false;
      if (!this.destroyed) this.render();
    });
  }

  /** Rebuild the per-page frame rects (white sheet + drop shadow) behind the canvas, one per page,
   *  positioned at each page's CSS top. The canvas paints page content over each frame and leaves
   *  the gutters transparent, so each page reads as its own drop-shadowed sheet. Reuses existing
   *  child divs, adding/removing only the difference (cheap on a page-count change). */
  private rebuildFrames(count: number, pageW: number, pageH: number, gap: number): void {
    const layer = this.framesLayer;
    while (layer.childElementCount > count) layer.lastElementChild!.remove();
    while (layer.childElementCount < count) {
      const f = document.createElement('div');
      f.className = 'scriptor-page-frame';
      // A DOWNWARD-ONLY drop shadow - zero horizontal bleed. The reported bug: a shadow with 0
      // x-offset and a blur radius fans out sideways by ~blur px along the FULL height of the
      // left/right edges; stacked pages line those side-bands up at one x, so they read as a single
      // continuous vertical rail that only lightens (never returns to desk) across each gap - the
      // measured C7CAD1 on-page / DADBE2 in-gap tint over an EEF0F5 desk. The cure is a negative
      // spread >= the blur radius (spread <= -blur): the shadow box is inset by |spread|, so blur
      // adds back exactly to the edge and never past it on the sides/top, while the downward offset
      // still pushes a soft crescent below each page's bottom edge. Net: sides + top meet clean
      // desk (no rail), the only shadow is beneath each page and breaks naturally at every gap.
      f.style.cssText =
        'position:absolute;left:0;background:#fff;' +
        'box-shadow:0 1px 2px -1px rgba(15,23,42,0.20), 0 6px 12px -8px rgba(15,23,42,0.30);';
      layer.append(f);
    }
    const wCss = Math.round(pageW / this.dpr);
    const hCss = Math.round(pageH / this.dpr);
    const strideCss = (pageH + gap) / this.dpr;
    let i = 0;
    for (const el of Array.from(layer.children) as HTMLElement[]) {
      el.style.top = `${Math.round(i * strideCss)}px`;
      el.style.width = `${wCss}px`;
      el.style.height = `${hCss}px`;
      i++;
    }
    this.framesCount = count;
  }

  private render(): void {
    // Render at device resolution times the zoom; CSS divides by dpr, so the page grows with zoom.
    const info = this.doc.relayout(this.dpr * this.zoom);
    const pageW = info.pageWidth;
    const pageH = info.pageHeight;
    const gap = info.gap;
    this.pageH = pageH;
    this.pageGap = gap;
    const total = info.totalHeight;

    // The wrapper spans the whole document (scroll extent + event surface + client<->device
    // mapping); the canvases only back the viewport window (see updateWindow).
    const count = info.fingerprints.length;
    const resized = this.deviceW !== pageW || this.deviceTotalH !== total;
    if (resized) {
      this.deviceW = pageW;
      this.deviceTotalH = total;
      this.sheet.style.width = `${Math.round(pageW / this.dpr)}px`;
      this.sheet.style.height = `${Math.round(total / this.dpr)}px`;
      this.paintedAt = [];
    }
    if (resized || count !== this.framesCount) this.rebuildFrames(count, pageW, pageH, gap);
    this.lastFingerprints = Array.from(info.fingerprints);
    this.commentRectsCache = Array.from(this.doc.commentRects());
    this.updateWindow(resized);
    this.notify(); // page count / pagination may have changed - let chrome (status bar) refresh
  }

  /** Re-derive the painted canvas window from the viewport, repositioning + repainting as needed.
   *  The window is snapped to whole pages with a one-page buffer each side, so scrolling only
   *  repaints when the viewport crosses into an unpainted page. A full-document canvas is not an
   *  option: Safari caps canvas area at ~16.8M px (a dozen pages at 100% zoom) and fails BLANK
   *  past it, and memory would grow with document length. `force` repaints every window page (a
   *  resize / zoom / reload invalidated the backing store). */
  private updateWindow(force: boolean): void {
    const stride = this.pageH + this.pageGap;
    const count = this.lastFingerprints.length;
    if (!stride || !count || !this.deviceW) return;
    const r = this.sheet.getBoundingClientRect();
    // CSS px per device px - derived from the live rect so browser zoom / transforms stay correct.
    // A zero-height rect (the view mounted display:none) falls back to the leading pages.
    const cssPerDev = r.height / Math.max(1, this.deviceTotalH);
    const viewH = window.innerHeight || document.documentElement.clientHeight;
    const vTop = r.height > 0 ? (0 - r.top) / Math.max(1e-6, cssPerDev) : 0;
    const vBot = r.height > 0 ? (viewH - r.top) / Math.max(1e-6, cssPerDev) : viewH;
    const first = Math.max(0, Math.min(count - 1, Math.floor((vTop - this.pageH) / stride)));
    let last = Math.max(first, Math.min(count - 1, Math.floor((vBot + this.pageH) / stride)));
    // Cap the window's device height so the canvas stays under conservative browser limits at any
    // zoom (at least one page even when a single page exceeds the target).
    const maxPages = Math.max(1, Math.floor(24_000 / stride));
    if (last - first + 1 > maxPages) last = first + maxPages - 1;
    const topDev = first * stride;
    const winH = (last - first) * stride + this.pageH;

    const moved =
      force ||
      first !== this.windowFirst ||
      last - first + 1 !== this.windowCount ||
      this.page.width !== this.deviceW ||
      this.page.height !== winH;
    this.windowFirst = first;
    this.windowCount = last - first + 1;
    this.windowTopDev = topDev;
    if (moved) {
      for (const c of [this.page, this.overlay]) {
        if (c.width !== this.deviceW) c.width = this.deviceW;
        if (c.height !== winH) c.height = winH;
        c.style.width = `${Math.round(this.deviceW / this.dpr)}px`;
        c.style.height = `${Math.round(winH / this.dpr)}px`;
        c.style.top = `${topDev / this.dpr}px`;
      }
      // The move/resize invalidated the window's pixels. Clear to transparent (default) so the
      // frames' drop shadows + the host's page-stage backdrop show through the gaps; an opaque
      // `gutter` color fills them instead (the pre-frames flat look).
      this.ctx.setTransform(1, 0, 0, 1, 0, 0);
      if (this.opts.gutter && this.opts.gutter !== 'transparent') {
        this.ctx.fillStyle = this.opts.gutter;
        this.ctx.fillRect(0, 0, this.deviceW, winH);
      } else {
        this.ctx.clearRect(0, 0, this.deviceW, winH);
      }
    }
    for (let i = first; i <= last; i++) {
      const fp = this.lastFingerprints[i] ?? null;
      if (!moved && this.paintedAt[i] === fp) continue;
      if (!moved && this.paintedAt[i] !== null) {
        // The canvas already shows this page's previous raster and only its content changed
        // (typing edits one paragraph): fetch just the changed row band - pixel-diffed in the
        // engine - instead of shipping the whole page across the wasm boundary.
        const band = this.doc.paintPageBand(i);
        const dv = new DataView(band.buffer, band.byteOffset, 8);
        const y0 = dv.getUint32(0, true);
        const y1 = dv.getUint32(4, true);
        if (y1 > y0) {
          const rows = band.subarray(8, 8 + (y1 - y0) * this.deviceW * 4);
          this.ctx.putImageData(
            new ImageData(new Uint8ClampedArray(rows), this.deviceW, y1 - y0),
            0,
            i * stride - topDev + y0,
          );
        }
      } else {
        const px = this.doc.paintPage(i);
        this.ctx.putImageData(
          new ImageData(new Uint8ClampedArray(px), this.deviceW, this.pageH),
          0,
          i * stride - topDev,
        );
      }
      this.paintedAt[i] = fp;
    }
    // Pages outside the window hold no pixels any more.
    for (let i = 0; i < count; i++) {
      if (i < first || i > last) this.paintedAt[i] = null;
    }
    this.drawOverlay();
  }

  private drawOverlay(): void {
    // Every overlay rect below is in full-document device coords; the canvas only backs the
    // viewport window, so one translation maps absolute -> window-local for all of them.
    this.octx.setTransform(1, 0, 0, 1, 0, -this.windowTopDev);
    this.octx.clearRect(0, this.windowTopDev, this.overlay.width, this.overlay.height);

    // Comment anchors: a soft amber band behind commented text (editor chrome - not in the export).
    const cr = this.commentRectsCache;
    if (cr.length) {
      this.octx.fillStyle = 'rgba(245, 158, 11, 0.20)';
      for (let i = 0; i < cr.length; i += 4) {
        this.octx.fillRect(cr[i]!, cr[i + 1]!, cr[i + 2]!, cr[i + 3]!);
      }
    }

    // Comparison change bands: one coloured row per changed paragraph, the active one emphasized with
    // a left accent bar. Culled to the painted window so a document with many changes stays cheap.
    if (this.presence.changeMarkers.length) {
      const winTop = this.windowTopDev;
      const winBot = winTop + this.overlay.height;
      for (const m of this.presence.changeMarkers) {
        let rects: Float32Array;
        try {
          rects = this.doc.selectionRects(m.para, 0, m.para, this.doc.paragraphLength(m.para));
        } catch {
          continue; // paragraph gone / out of range
        }
        if (!rects.length) {
          // Empty paragraph (no text rects): a short band at the paragraph's caret line.
          let c: Float32Array;
          try {
            c = this.doc.caretRect(m.para, 0);
          } catch {
            continue;
          }
          if (c[2] === 0) continue;
          rects = Float32Array.of(c[0]!, c[1]!, 24 * this.dpr, c[2]!);
        }
        let minX = Number.POSITIVE_INFINITY;
        let minY = Number.POSITIVE_INFINITY;
        let maxY = Number.NEGATIVE_INFINITY;
        for (let i = 0; i < rects.length; i += 4) {
          minX = Math.min(minX, rects[i]!);
          minY = Math.min(minY, rects[i + 1]!);
          maxY = Math.max(maxY, rects[i + 1]! + rects[i + 3]!);
        }
        if (maxY < winTop || minY > winBot) continue; // entirely outside the window
        this.octx.fillStyle = changeBandColor(m.kind, m.active === true);
        for (let i = 0; i < rects.length; i += 4) {
          this.octx.fillRect(rects[i]!, rects[i + 1]!, rects[i + 2]!, rects[i + 3]!);
        }
        if (m.active === true) {
          this.octx.fillStyle = changeAccentColor(m.kind);
          this.octx.fillRect(
            minX - 6 * this.dpr,
            minY,
            Math.max(2, Math.round(3 * this.dpr)),
            maxY - minY,
          );
        }
      }
    }

    // Remote collaborators' carets (presence). Drawn before the local-selection
    // early-returns so they show whatever the local user has selected.
    this.presence.drawRemoteCursors();

    // Picture selection (box + resize handles) - mutually exclusive with a text selection/caret.
    if (this.img.selectedImage !== null) {
      this.img.drawImageSelection();
      return;
    }

    if (!this.sel) return;
    const { anchor, focus } = this.sel;

    const rects = this.doc.selectionRects(anchor.para, anchor.off, focus.para, focus.off);
    if (rects.length) {
      this.octx.fillStyle = 'rgba(37, 99, 235, 0.25)';
      for (let i = 0; i < rects.length; i += 4) {
        this.octx.fillRect(rects[i]!, rects[i + 1]!, rects[i + 2]!, rects[i + 3]!);
      }
    }

    if (this.opts.mode === 'edit' && this.caretOn) {
      const c = this.doc.caretRect(focus.para, focus.off);
      // Canvas colors can't be CSS `var()` - the assignment was silently ignored, so the caret
      // inherited whatever translucent fill was last set on the context (selection blue / comment
      // amber) and was barely visible. Resolve the theme ink through the DOM instead, and snap the
      // bar to whole device pixels so a fractional rect isn't anti-aliased half-faint.
      this.octx.fillStyle = this.caretInk();
      this.octx.fillRect(Math.round(c[0]!), c[1]!, Math.max(1, Math.round(this.dpr)), c[2]!);
    }
  }

  /** The caret's ink color: the theme's `--scr-ink` resolved via the DOM (canvas fill styles do
   *  not understand `var()`), near-black when unset. Cached; re-resolved on a mode change. */
  private caretInkCache: string | null = null;
  private caretInk(): string {
    if (!this.caretInkCache) {
      const v = getComputedStyle(this.overlay).getPropertyValue('--scr-ink').trim();
      this.caretInkCache = v || '#1a1a1a';
    }
    return this.caretInkCache;
  }





  // ── selection + pointer ──────────────────────────────────────────────────────

  private setSelection(focus: CaretPos, anchor: CaretPos = focus): void {
    this.img.exitCropMode(); // leaving the picture for a text caret also leaves crop
    this.img.selectedImage = null; // a text caret/selection and a picture selection are mutually exclusive
    this.sel = { anchor: { ...anchor }, focus: { ...focus } };
    this.caretOn = true;
    // Tell the engine which story the caret is in, so undo/redo (Ctrl+Z) route to that story's
    // history - a header edit undoes in the header, not the body.
    this.doc.setActiveStory(focus.para);
    // Word dims whichever story isn't being edited; when the caret crosses into a different story
    // (body <-> header <-> footer) every page's dimming flips, so force a full repaint.
    const region = regionOf(focus.para);
    if (region !== this.activeRegion) {
      this.activeRegion = region;
      this.fullRender();
    }
    this.opts.onSelectionChange?.(this.sel);
    this.notify();
  }



















  private toCanvas(e: MouseEvent): [number, number] {
    // Client -> full-document device coords via the wrapper (the canvases only back the viewport
    // window; the wrapper spans the whole document, so this mapping is window-independent).
    const r = this.sheet.getBoundingClientRect();
    return [
      (e.clientX - r.left) * (this.deviceW / Math.max(1, r.width)),
      (e.clientY - r.top) * (this.deviceTotalH / Math.max(1, r.height)),
    ];
  }

  private hit(e: MouseEvent): CaretPos {
    const [x, y] = this.toCanvas(e);
    const p = this.doc.hitTest(x, y);
    return { para: p[0]!, off: p[1]! };
  }

  /**
   * The paragraph whose margin change-bar is under canvas point `(x, y)`, or `null`. Drives
   * Simple-Markup click-to-expand: a click anywhere in the left-margin gutter beside a changed
   * visual line expands that paragraph's inline redline. `changeBars()` returns flat
   * `[page, x, y, w, h, para]` sextuples (device px, page-local `y`); page `i` paints at canvas
   * `i * (pageH + pageGap)`.
   */
  private changeBarHitAt(x: number, y: number): number | null {
    const bars = this.doc.changeBars();
    for (let i = 0; i + 6 <= bars.length; i += 6) {
      const top = bars[i]! * (this.pageH + this.pageGap) + bars[i + 2]!;
      const barRight = bars[i + 1]! + bars[i + 3]!;
      // Generous horizontal target: the whole gutter from the page edge to just past the bar.
      if (x <= barRight + 10 && y >= top && y <= top + bars[i + 4]!) {
        return bars[i + 5]!;
      }
    }
    return null;
  }

  private bindPointer(): void {
    this.sheet.addEventListener('mousedown', (e) => {
      this.suppressNextClick = false;
      if (this.opts.mode !== 'edit' && !this.opts.selectable) return;
      const [cx, cy] = this.toCanvas(e);
      // Record which page's header/footer instance this click is on, so the caret resolves to that
      // instance on a multi-page document (a header/footer is one story painted on every page). Set
      // here (a real click) rather than on hover, so hovering another page's header can't move it.
      if (this.pageH + this.pageGap > 0) {
        this.doc.setHeaderFooterPage(Math.floor(cy / (this.pageH + this.pageGap)));
      }
      // Ctrl/Cmd+click follows a hyperlink under the pointer (external URL in a new tab; internal
      // #anchor jumps to its bookmark) instead of moving the caret.
      if (e.ctrlKey || e.metaKey) {
        const pos = this.hit(e);
        const target = this.doc.linkAt(pos.para, pos.off);
        if (target) {
          e.preventDefault();
          this.followLink(target);
          return;
        }
      }
      // Simple Markup: a click on a margin change-bar reveals that paragraph's inline redline
      // (click again to collapse) instead of moving the caret. Only this mode has clickable bars.
      if (this.trackDisplay === 'simple') {
        const para = this.changeBarHitAt(cx, cy);
        if (para !== null) {
          this.doc.toggleParagraphExpanded(para);
          this.suppressNextClick = true; // the trailing 'click' must not open a track popup
          this.fullRender();
          e.preventDefault();
          return;
        }
      }
      // A picture under the pointer (or a grab of the selected picture's resize handle) takes the
      // click - select / resize / move it instead of moving the caret. Edit mode only.
      if (this.img.imageMouseDown(cx, cy, e)) return;
      // Hold keyboard focus in the offscreen textarea so typing, shortcuts, and clipboard
      // (copy/cut/paste) events route here - in read+selectable mode too, so selections can be copied.
      this.keys.focus({ preventScroll: true });
      const pos = this.hit(e);
      // Word's multi-click ladder: a double-click selects the word under the pointer, a
      // triple-click the whole paragraph (`e.detail` counts the click run). Neither arms
      // `dragging`, so the selection survives the incidental pointer movement of a fast
      // multi-click; a plain click still starts a character-wise drag.
      if (e.detail === 2) {
        const w = this.wordRangeAt(pos.para, pos.off);
        if (w) {
          this.setSelection({ para: pos.para, off: w.e }, { para: pos.para, off: w.s });
          this.drawOverlay();
          e.preventDefault();
          return;
        }
      }
      if (e.detail >= 3) {
        const len = [...this.doc.paragraphText(pos.para)].length;
        this.setSelection({ para: pos.para, off: len }, { para: pos.para, off: 0 });
        this.drawOverlay();
        e.preventDefault();
        return;
      }
      this.setSelection(pos);
      this.dragging = true;
      this.drawOverlay();
      e.preventDefault();
    });
    this.sheet.addEventListener('mousemove', this.img.onImageHover);
    window.addEventListener('mousemove', this.onMouseMove);
    window.addEventListener('mouseup', this.onMouseUp);
  }


  private onMouseMove = (e: MouseEvent): void => {
    if (this.img.imageDrag) {
      const [cx, cy] = this.toCanvas(e);
      const d = this.img.imageDrag;
      if (Math.abs(cx - d.startX) > 2 || Math.abs(cy - d.startY) > 2) d.moved = true;
      let r: { x: number; y: number; w: number; h: number };
      if (d.mode === 'crop') {
        const full = this.img.cropFullRect(d.id);
        r = full
          ? this.img.cropWindow(d, cx, cy, full)
          : { x: d.rectX, y: d.rectY, w: d.rectW, h: d.rectH };
      } else if (d.mode === 'move') {
        r = this.img.moveRect(d, cx, cy);
      } else {
        r = this.img.resizeRect(d, cx, cy, e.shiftKey);
      }
      d.curX = r.x;
      d.curY = r.y;
      d.curW = r.w;
      d.curH = r.h;
      this.drawOverlay();
      return;
    }
    if (!this.dragging || !this.sel) return;
    this.sel.focus = this.hit(e);
    this.opts.onSelectionChange?.(this.sel);
    this.drawOverlay();
  };

  private onMouseUp = (): void => {
    if (this.img.imageDrag) {
      this.img.finishImageDrag();
      return;
    }
    this.dragging = false;
  };







  // ── table context menu (built-in; opt out via `tableMenu: false`) ─────────────

  private bindContextMenu(): void {
    this.sheet.addEventListener('contextmenu', (e) => this.onContextMenu(e));
  }

  /** Own the right-click across the whole canvas body with a document context menu (never the
   *  browser's canvas menu, which shows "Save image as…" and the like). The item list adapts to what
   *  the pointer is over - clipboard, hyperlink, tracked change, table, picture, Select All - and is
   *  handed to `opts.onContextMenu` so an integrator can add/remove commands. In read mode the menu
   *  is Copy / Select All only (still better than the browser default over a canvas). */
  private onContextMenu(e: MouseEvent): void {
    const editing = this.opts.mode === 'edit';
    if (!editing && !this.opts.selectable) return; // pure read-only: nothing to offer
    const [cx, cy] = this.toCanvas(e);

    // A right-click on a picture selects it (Word behavior); other clicks move the caret UNLESS the
    // click lands inside the current selection, which is kept (so "right-click the selection -> Copy"
    // works). Then gather what's under the pointer.
    const imgId = editing ? this.doc.imageAtPoint(cx, cy) : undefined;
    let pos: CaretPos;
    let onImage = false;
    if (imgId !== undefined) {
      this.img.selectImage(imgId);
      onImage = true;
      pos = this.hit(e);
    } else {
      pos = this.hit(e);
      if (!this.clickInSelection(pos)) this.setSelection(pos);
      this.drawOverlay();
    }

    // "Is there something to copy?", not "is there something I can edit in place?" - those parted
    // company when `selectionText` learned to span paragraphs. Deriving this from `selSpan` greyed
    // out Copy on every multi-paragraph selection, Select All included, which is the whole point of
    // read+selectable mode. Cut keeps the narrower test (see `buildContextMenuItems`).
    const hasSelection = !!this.selectionText();
    const linkTarget = editing ? this.doc.linkAt(pos.para, pos.off) : '';
    const onTrackedChange = editing && !!this.trackAt(pos.para, pos.off);
    const inTable = editing && this.doc.tableContext(pos.para).length > 0;

    const ctx: ScriptorContextMenuContext = {
      para: pos.para,
      offset: pos.off,
      hasSelection,
      selectionText: this.selectionText(),
      linkTarget,
      onTrackedChange,
      inTable,
      onImage,
      clientX: e.clientX,
      clientY: e.clientY,
    };
    const defaults = this.buildContextMenuItems(ctx, editing);

    let items: ScriptorContextMenuItem[] = defaults;
    const hook = this.opts.onContextMenu?.(ctx, defaults);
    if (hook === null) return; // integrator defers to the browser's native menu
    if (hook !== undefined) items = hook;
    if (!items.length) {
      // Nothing to show, but still suppress the browser's canvas menu (the editor owns the surface).
      e.preventDefault();
      return;
    }
    e.preventDefault();
    this.pop.openMenu(e.clientX, e.clientY, items);
  }

  /** The built-in, context-adapted menu items (before `opts.onContextMenu`). */
  private buildContextMenuItems(
    ctx: ScriptorContextMenuContext,
    editing: boolean,
  ): ScriptorContextMenuItem[] {
    // A picture gets its own command set (crop / wrap / delete) - the text/clipboard items don't
    // apply to a selected image.
    if (editing && ctx.onImage) return this.img.imageMenuItems();
    const items: ScriptorContextMenuItem[] = [];
    const sep = () => {
      if (items.length && !items[items.length - 1]!.separator) items.push({ separator: true });
    };
    if (editing) {
      // Cut, unlike Copy, has to be able to REMOVE what it takes, and in-place removal is still
      // single-paragraph - so it tests the edit span rather than `hasSelection`.
      items.push({ label: 'Cut', disabled: !this.selSpan(), onClick: () => this.menuCut() });
      items.push({ label: 'Copy', disabled: !ctx.hasSelection, onClick: () => this.menuCopy() });
      items.push({ label: 'Paste', onClick: () => void this.menuPaste() });
    } else {
      // Read mode: Copy a selection (the reason `selectable` exists) - no editing commands.
      items.push({ label: 'Copy', disabled: !ctx.hasSelection, onClick: () => this.menuCopy() });
    }
    if (editing && ctx.linkTarget) {
      sep();
      items.push({ label: 'Open Link', onClick: () => this.followLink(ctx.linkTarget) });
      items.push({ label: 'Edit Link…', onClick: () => void this.addHyperlink() });
      items.push({ label: 'Remove Link', onClick: () => this.removeHyperlink() });
    }
    if (editing && ctx.onTrackedChange) {
      sep();
      items.push({ label: 'Accept Change', onClick: () => this.acceptChange() });
      items.push({ label: 'Reject Change', onClick: () => this.rejectChange() });
    }
    if (editing && ctx.inTable && this.opts.tableMenu) {
      sep();
      for (const it of this.tableMenuItems()) items.push(it);
    }
    sep();
    items.push({ label: 'Select All', onClick: () => this.selectAll() });
    return items;
  }

  /** Whether caret position `pos` falls within the current (non-empty) selection - a right-click there
   *  keeps the selection instead of collapsing it to the click (Word behavior). */
  private clickInSelection(pos: CaretPos): boolean {
    const span = this.selSpan();
    if (!span || pos.para !== span.para) return false;
    return pos.off >= span.s && pos.off <= span.e;
  }

  /** Select the whole document (Select All / Ctrl+A): from the first paragraph to the end of the last. */
  private selectAll(): void {
    const n = this.doc.paragraphCount();
    if (n <= 0) return;
    const last = n - 1;
    const lastLen = [...this.doc.paragraphText(last)].length;
    this.setSelection({ para: last, off: lastLen }, { para: 0, off: 0 });
    this.drawOverlay();
    this.keys.focus({ preventScroll: true });
  }

  // Clipboard commands driven from the context menu. Copy/Cut reuse the offscreen textarea's native
  // clipboard events (execCommand from the menu-click gesture); Paste uses the async Clipboard API
  // (a menu click is a user gesture, so readText resolves - it may prompt for permission once).
  private menuCopy(): void {
    this.keys.focus({ preventScroll: true });
    try {
      document.execCommand('copy');
    } catch {
      // Fallback: write the plain selection text directly.
      const t = this.selectionText();
      if (t) void navigator.clipboard?.writeText(t);
    }
  }

  private menuCut(): void {
    if (this.opts.mode !== 'edit') return;
    this.keys.focus({ preventScroll: true });
    try {
      document.execCommand('cut');
    } catch {
      /* the offscreen textarea path handles the model delete on the cut event */
    }
  }

  private async menuPaste(): Promise<void> {
    if (this.opts.mode !== 'edit') return;
    try {
      const text = await navigator.clipboard?.readText();
      if (text) this.pasteText(text);
    } catch {
      // Clipboard read denied / unavailable: focus the editor so Ctrl/Cmd+V (the event path) works.
      this.keys.focus({ preventScroll: true });
    }
  }


  /** The table commands (insert / delete / move / merge / split rows & columns, shading, borders) -
   *  shown as a section of the context menu when the pointer is in a table cell. */
  private tableMenuItems(): ScriptorContextMenuItem[] {
    const item = (label: string, onClick: () => void): ScriptorContextMenuItem => ({
      label,
      onClick,
    });
    return [
      item('Insert row above', () => this.insertRowAbove()),
      item('Insert row below', () => this.insertRowBelow()),
      item('Insert column left', () => this.insertColumnLeft()),
      item('Insert column right', () => this.insertColumnRight()),
      { separator: true },
      item('Delete row', () => this.deleteRow()),
      item('Delete column', () => this.deleteColumn()),
      { separator: true },
      item('Move row up', () => this.moveRowUp()),
      item('Move row down', () => this.moveRowDown()),
      item('Move column left', () => this.moveColumnLeft()),
      item('Move column right', () => this.moveColumnRight()),
      { separator: true },
      item('Merge cell right', () => this.mergeCellsRight()),
      item('Merge cell down', () => this.mergeCellsDown()),
      item('Split cell across', () => this.splitCellHorizontal()),
      item('Split cell down', () => this.splitCellVertical()),
      { separator: true },
      // Property edits (tracked as w:tcPrChange / w:trPrChange / w:tblPrChange when Track-Changes is on).
      item('Shade cell yellow', () => this.setCellShading('FFFF00')),
      item('Shade cell grey', () => this.setCellShading('D9D9D9')),
      item('Clear cell shading', () => this.setCellShading('')),
      { separator: true },
      item('All table borders', () => this.setTableBorders(4)),
      item('No table borders', () => this.setTableBorders(0)),
    ];
  }




  // ── inline tracked-change UX: hover tooltip + click accept/reject popup (1C) ──

  private bindTrackInteraction(): void {
    this.sheet.addEventListener('mousemove', this.onTrackHover);
    this.sheet.addEventListener('mouseleave', this.onTrackLeave);
    this.sheet.addEventListener('click', this.onTrackClick);
  }

  /** Hover over a tracked run -> a "{author} {inserted|deleted}: ..." tooltip (Word's inline hint).
   *  Throttled to recompute only when the hovered caret cell changes (the model query materializes
   *  the paragraph; the geometry hit-test is cheap). */
  private onTrackHover = (e: MouseEvent): void => {
    if (this.dragging) {
      this.pop.hideTrackTip();
      return;
    }
    // A picture owns the cursor (`onImageHover` sets move/resize; the mousedown sets it for a drag).
    // Both handlers fire on every move and this one is registered last, so bail when a picture or its
    // resize handle is under the pointer (or a drag is live) - otherwise it stomps the cursor back to
    // the text caret and you never see the resize/move pointer.
    const [x, y] = this.toCanvas(e);
    if (
      this.img.imageDrag ||
      (this.opts.mode === 'edit' &&
        (this.img.imageHandleAt(x, y) !== null || this.doc.imageAtPoint(x, y) !== undefined))
    ) {
      this.pop.hideTrackTip();
      return;
    }
    // Simple Markup: a margin change-bar is clickable (expand). Show a pointer + skip the tracked-run
    // tooltip while hovering it; otherwise restore the editing caret cursor.
    if (this.trackDisplay === 'simple' && this.changeBarHitAt(x, y) !== null) {
      this.sheet.style.cursor = 'pointer';
      this.pop.hideTrackTip();
      return;
    }
    this.sheet.style.cursor =
      this.opts.mode === 'edit' || this.opts.selectable ? 'text' : 'default';
    const p = this.doc.hitTest(x, y);
    const key = `${p[0]}:${p[1]}`;
    if (key === this.lastHoverKey) {
      if (this.pop.trackTip) this.pop.positionTip(e.clientX, e.clientY);
      return;
    }
    this.lastHoverKey = key;
    const hit = this.trackAt(p[0]!, p[1]!);
    if (!hit) {
      this.pop.hideTrackTip();
      return;
    }
    const verb = TRACK_VERB[hit.kind] ?? 'changed';
    const text = hit.text.length > 80 ? `${hit.text.slice(0, 79)}...` : hit.text;
    this.pop.showTrackTip(
      this.presence.displayAuthor(hit.author) || 'Unknown',
      `${verb}: "${text}"`,
      e.clientX,
      e.clientY,
    );
  };

  private onTrackLeave = (): void => {
    this.pop.hideTrackTip();
  };

  /** Click a tracked run (edit mode, collapsed caret) -> a small Accept / Reject popup at the pointer. */
  private onTrackClick = (e: MouseEvent): void => {
    if (this.suppressNextClick) {
      this.suppressNextClick = false;
      return; // this click toggled a change-bar (Simple-Markup expand) - don't also open a popup
    }
    if (this.opts.mode !== 'edit' || this.selSpan()) return; // skip while a range is selected
    const [x, y] = this.toCanvas(e);
    const p = this.doc.hitTest(x, y);
    const para = p[0]!;
    const off = p[1]!;
    const hit = this.trackAt(para, off);
    if (hit) {
      this.pop.hideTrackTip();
      this.pop.showTrackPopup(e.clientX, e.clientY, para, hit);
      return;
    }
    // Not over a tracked change: open the comment thread if the caret is inside a comment anchor.
    const ids = this.commentsAt(para, off);
    if (ids.length) {
      this.pop.hideTrackTip();
      this.pop.showCommentThread(e.clientX, e.clientY, ids);
    }
  };

  /** The tracked change under `(para, off)`, or undefined (swallows engine errors -> no chrome). */
  private trackAt(para: number, off: number): ReturnType<ScriptorDoc['trackAt']> {
    try {
      return this.doc.trackAt(para, off);
    } catch {
      return undefined;
    }
  }

  /** The comment ids anchored at `(para, off)`, or `[]` (swallows engine errors -> no chrome). */
  private commentsAt(para: number, off: number): number[] {
    try {
      return Array.from(this.doc.commentsAt(para, off));
    } catch {
      return [];
    }
  }






  // ── comment popovers (compose + thread) ──────────────────────────────────────








  // ── keyboard (edit mode) ─────────────────────────────────────────────────────

  private bindKeyboard(): void {
    this.keys.addEventListener('keydown', (e) => this.onKeyDown(e));
  }

  // ── clipboard (Ctrl/Cmd + C / X / V) ─────────────────────────────────────────
  // Handled via the real copy/cut/paste events on the focused textarea (synchronous, no
  // permissions), not the async Clipboard API. Plain text only; single-paragraph for now.

  private bindClipboard(): void {
    this.keys.addEventListener('copy', (e) => this.onCopy(e, false));
    this.keys.addEventListener('cut', (e) => this.onCopy(e, true));
    this.keys.addEventListener('paste', (e) => this.onPaste(e));
  }

  private onCopy(e: ClipboardEvent, cut: boolean): void {
    const text = this.selectionText();
    if (!text) return; // collapsed selection -> leave the browser's default alone
    e.preventDefault();
    e.clipboardData?.setData('text/plain', text);
    if (!cut || this.opts.mode !== 'edit') return;
    const span = this.selSpan();
    // A cross-paragraph CUT copies but does not delete: multi-paragraph removal is the same engine
    // gap `replaceSelection` documents for paste. Keeping the clipboard half is the lesser evil -
    // dropping it too would lose the text the user asked for as well as leaving the document alone.
    if (!span) return;
    if (this.trackChanges) {
      // A tracked cut is the source half of a move: mark the range `w:moveFrom` (text retained) and
      // hold the cut text + revision id so the next matching paste relocates it as `w:moveTo`. If the
      // paste never comes (or goes elsewhere), the mark stands on its own as a tracked deletion.
      let id = -1;
      this.edit(
        () => {
          id = this.doc.markMoveSource(span.para, span.s, span.e);
        },
        { para: span.para, off: span.s },
      );
      this.pendingMove = id >= 0 ? { id, text } : null;
      return;
    }
    this.edit(() => this.doc.deleteRange(span.para, span.s, span.e), {
      para: span.para,
      off: span.s,
    });
  }

  private onPaste(e: ClipboardEvent): void {
    if (this.opts.mode !== 'edit') return;
    e.preventDefault();
    this.pasteText(e.clipboardData?.getData('text/plain') ?? '');
  }

  /** Insert clipboard `text` at the caret / over the selection, honoring a pending tracked move.
   *  Shared by the paste event and the context-menu Paste command. */
  private pasteText(text: string): void {
    if (this.opts.mode !== 'edit' || !text) return;
    // Complete a move: a tracked cut is pending, the clipboard still holds its text, and the caret is
    // collapsed (a range paste is an ordinary replace). Relocate the cut text as the move destination.
    const pm = this.pendingMove;
    if (this.trackChanges && pm && pm.text === text && this.sel && !this.selSpan()) {
      const dest = this.sel.focus;
      this.pendingMove = null;
      this.edit(() => this.doc.addMoveDest(dest.para, dest.off, pm.text, pm.id), {
        para: dest.para,
        off: dest.off + [...text].length,
      });
      return;
    }
    this.replaceSelection(text);
  }

  // ── zoom (Ctrl/Cmd +/-/0, Ctrl/Cmd + wheel) ──────────────────────────────────

  private bindZoom(): void {
    // Capturing window listener so we run before (and cancel) the browser's own page zoom, and so it
    // works whenever focus is anywhere in this view (not just the textarea).
    window.addEventListener('keydown', this.onZoomKey, true);
    // Ctrl/Cmd + wheel over the page is the always-reliable zoom path (wheel default is cancelable).
    this.sheet.addEventListener('wheel', this.onZoomWheel, { passive: false });
  }

  private onZoomKey = (e: KeyboardEvent): void => {
    // Global Ctrl/Cmd shortcuts that must beat the browser's defaults + work in any mode (zoom, save).
    // A capturing window listener so it runs whatever element in the view holds focus. Alt is excluded.
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const a = document.activeElement;
    if (a && a !== document.body && !this.container.contains(a)) return; // focus is on something else
    // Ctrl/Cmd+S saves (overrides the browser's Save-page dialog); read mode included.
    if ((e.key === 's' || e.key === 'S') && !e.shiftKey) {
      e.preventDefault();
      this.save();
      return;
    }
    if (e.key === '+' || e.key === '=') {
      e.preventDefault();
      this.zoomBy(0.1);
    } else if (e.key === '-' || e.key === '_') {
      e.preventDefault();
      this.zoomBy(-0.1);
    } else if (e.key === '0') {
      e.preventDefault();
      this.setZoom(1);
    }
  };

  private onZoomWheel = (e: WheelEvent): void => {
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    this.zoomBy(e.deltaY < 0 ? 0.1 : -0.1);
  };

  private zoomBy(delta: number): void {
    this.setZoom(Math.round((this.zoom + delta) * 10) / 10);
  }

  /** A single-paragraph selection range, or null if collapsed / cross-paragraph (not yet editable). */
  private selSpan(): { para: number; s: number; e: number } | null {
    if (!this.sel) return null;
    const { anchor, focus } = this.sel;
    if (anchor.para === focus.para && anchor.off === focus.off) return null;
    if (anchor.para !== focus.para) return null;
    return {
      para: anchor.para,
      s: Math.min(anchor.off, focus.off),
      e: Math.max(anchor.off, focus.off),
    };
  }

  private edit(mutate: () => void, caret: CaretPos): void {
    try {
      // Any edit other than a move's own two steps invalidates a pending move (the source offsets /
      // pairing id can no longer be trusted). The cut + paste steps set `pendingMove` *after* their
      // own `edit()` call, so this clear never drops a move mid-gesture.
      this.pendingMove = null;
      // The engine never invents time: hand it the current wall-clock so a tracked edit is stamped.
      if (this.trackChanges) this.doc.setNow(new Date().toISOString());
      mutate();
      this.setSelection(caret);
      this.fireChange();
      this.requestRender();
    } catch (err) {
      console.error('scriptor edit error', err);
    }
  }

  /** Join paragraph `joinPara` into the one before it, landing the caret at the merge point in
   *  `targetPara`. The engine returns the merge offset, or `-1` when the join is refused because it
   *  would cross a table-cell boundary - in which case this is a no-op (the caret stays put). */
  private joinInto(joinPara: number, targetPara: number): void {
    try {
      if (this.trackChanges) this.doc.setNow(new Date().toISOString());
      const at = this.doc.joinParagraph(joinPara);
      if (at < 0) return; // refused at a cell boundary
      this.fireChange();
      this.setSelection({ para: targetPara, off: at });
      this.requestRender();
    } catch (err) {
      console.error('scriptor join error', err);
    }
  }

  private onKeyDown(e: KeyboardEvent): void {
    this.pop.closeTrackPopup(); // any keystroke dismisses the inline accept/reject popup
    this.pop.closeCommentPopup();

    // A selected picture owns the keyboard (no caret). In crop mode Esc / Enter leave crop (keeping
    // the picture selected); otherwise Esc deselects and Delete / Backspace removes it. Any other key
    // is swallowed so it can't edit the (cleared) text caret.
    if (this.img.selectedImage !== null) {
      if (this.img.cropMode) {
        if (e.key === 'Escape' || e.key === 'Enter') {
          e.preventDefault();
          this.img.exitCropMode();
          this.drawOverlay();
        }
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        this.img.deselectImage();
      } else if ((e.key === 'Delete' || e.key === 'Backspace') && this.opts.mode === 'edit') {
        e.preventDefault();
        this.img.removeSelectedImage();
      }
      return;
    }

    // SELECTION keys come before the edit gate. Read mode dropped every keystroke, which made
    // Ctrl/Cmd+A a right-click-only command even with `selectable` on - and select-all-then-copy is
    // how people lift a document's text. (Zoom and Save have their own capturing listener; this is
    // the only selection command bound here, in either mode - it was never wired up at all.)
    if ((e.ctrlKey || e.metaKey) && !e.altKey && (e.key === 'a' || e.key === 'A')) {
      if (this.opts.mode === 'edit' || this.opts.selectable) {
        e.preventDefault();
        this.selectAll();
        return;
      }
    }

    if (this.opts.mode !== 'edit' || !this.sel) return;

    // F9 - update fields (regenerate the table of contents in place), matching Word.
    if (e.key === 'F9') {
      e.preventDefault();
      this.updateToc();
      return;
    }

    const f = this.sel.focus;
    const len = this.doc.paragraphLength(f.para);
    // Caret movement is bounded to the focus paragraph's own story (body/header/footer) so editing a
    // header can't arrow/join across into the body. `paragraphRange` returns [firstIndex, count].
    const range = this.doc.paragraphRange(f.para);
    const firstPara = range[0]!;
    const lastPara = range[0]! + Math.max(1, range[1]!) - 1;
    const span = this.selSpan();

    // Caret movement (no model change). Shift extends the selection.
    if (e.key === 'ArrowLeft' || e.key === 'ArrowRight' || e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      let np: CaretPos = { ...f };
      if (e.key === 'ArrowLeft') {
        if (f.off > 0) np = { para: f.para, off: f.off - 1 };
        else if (f.para > firstPara)
          np = { para: f.para - 1, off: this.doc.paragraphLength(f.para - 1) };
      } else if (e.key === 'ArrowRight') {
        if (f.off < len) np = { para: f.para, off: f.off + 1 };
        else if (f.para < lastPara) np = { para: f.para + 1, off: 0 };
      } else if (e.key === 'Home') {
        np = { para: f.para, off: 0 };
      } else {
        np = { para: f.para, off: len };
      }
      this.moveCaret(np, e.shiftKey);
      return;
    }

    // Vertical movement: step to the nearest visual line above/below at the caret's column - the
    // engine walks the line geometry, so it honors wraps, variable line heights, paragraph
    // spacing, and page boundaries. (A 1px hit-test probe snapped back inside inter-paragraph
    // spacing, so the caret never crossed a paragraph.)
    if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
      e.preventDefault();
      const c = this.doc.caretRect(f.para, f.off); // [x, y, h]
      const p = this.doc.caretLineStep(f.para, f.off, c[0]!, e.key === 'ArrowDown');
      if (p.length === 2) this.moveCaret({ para: p[0]!, off: p[1]! }, e.shiftKey);
      return;
    }

    // Editing (model change -> re-render).
    if (e.key === 'Backspace') {
      e.preventDefault();
      if (span)
        this.edit(() => this.doc.deleteRange(span.para, span.s, span.e), {
          para: span.para,
          off: span.s,
        });
      else if (f.off > 0)
        this.edit(() => this.doc.deleteRange(f.para, f.off - 1, f.off), {
          para: f.para,
          off: f.off - 1,
        });
      else if (f.para > firstPara) this.joinInto(f.para, f.para - 1);
      return;
    }
    if (e.key === 'Delete') {
      e.preventDefault();
      if (span)
        this.edit(() => this.doc.deleteRange(span.para, span.s, span.e), {
          para: span.para,
          off: span.s,
        });
      else if (f.off < len)
        this.edit(() => this.doc.deleteRange(f.para, f.off, f.off + 1), {
          para: f.para,
          off: f.off,
        });
      else if (f.para < lastPara) this.joinInto(f.para + 1, f.para);
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      // Enter on an empty list item ends the list instead of adding another empty marker: promote one
      // level, or drop the list at level 0 (Word's behaviour). `changeListLevel(-1)` does both.
      if (!span && len === 0 && this.doc.paragraphListLevel(f.para) >= 0) {
        this.changeListLevel(-1);
        return;
      }
      const at: CaretPos = span ? { para: span.para, off: span.s } : { para: f.para, off: f.off };
      this.edit(
        () => {
          if (span) this.doc.deleteRange(span.para, span.s, span.e);
          this.doc.splitParagraph(at.para, at.off);
        },
        { para: at.para + 1, off: 0 },
      );
      return;
    }
    // Tab / Shift+Tab: in a list, demote / promote the item one level (a list inside a table cell still
    // demotes, like Word). Else in a table cell, move to the next / previous cell. Else keep Tab's
    // default (focus traversal) so the editor stays keyboard-navigable.
    if (e.key === 'Tab' && !e.ctrlKey && !e.metaKey && !e.altKey) {
      if (this.changeListLevel(e.shiftKey ? -1 : 1)) {
        e.preventDefault();
        return;
      }
      const cell = this.doc.cellStep(f.para, !e.shiftKey);
      if (cell >= 0) {
        e.preventDefault();
        this.moveCaret({ para: cell, off: 0 }, false);
      }
      return;
    }

    // Heading style shortcuts (Word's Ctrl+Alt+1/2/3 -> Heading 1/2/3). Uses `e.code` so it's keyed to
    // the physical digit regardless of layout; routed through setParagraphStyle (tracked when on).
    if (
      (e.ctrlKey || e.metaKey) &&
      e.altKey &&
      !e.shiftKey &&
      (e.code === 'Digit1' || e.code === 'Digit2' || e.code === 'Digit3')
    ) {
      e.preventDefault();
      const n = e.code === 'Digit1' ? 1 : e.code === 'Digit2' ? 2 : 3;
      this.setParagraphStyle(`Heading${n}`);
      return;
    }

    // Undo / redo + formatting shortcuts (Ctrl/Cmd).
    if ((e.ctrlKey || e.metaKey) && !e.altKey) {
      const k = e.key.toLowerCase();
      // Ctrl+Shift+N resets the paragraph to Normal (Word's default-style shortcut).
      if (k === 'n' && e.shiftKey) {
        e.preventDefault();
        this.setParagraphStyle('');
        return;
      }
      if (k === 'z' && !e.shiftKey) {
        e.preventDefault();
        this.undo();
        return;
      }
      if (k === 'y' || (k === 'z' && e.shiftKey)) {
        e.preventDefault();
        this.redo();
        return;
      }
      // Zoom (Ctrl/Cmd +/-/0) is handled by a capturing window listener - see `bindZoom` - so it
      // works regardless of which element in the view holds focus and beats the browser's own zoom.
      if (k === 'b') {
        e.preventDefault();
        this.toggleBold();
        return;
      }
      if (k === 'i') {
        e.preventDefault();
        this.toggleItalic();
        return;
      }
      if (k === 'u') {
        e.preventDefault();
        this.toggleUnderline();
        return;
      }
      if (k === 'k') {
        e.preventDefault();
        void this.addHyperlink();
        return;
      }
    }

    // Printable character: replace any single-paragraph selection, then insert.
    if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      const at: CaretPos = span ? { para: span.para, off: span.s } : { para: f.para, off: f.off };
      const ch = e.key;
      this.edit(
        () => {
          if (span) this.doc.deleteRange(span.para, span.s, span.e);
          this.doc.insertText(at.para, at.off, ch);
        },
        { para: at.para, off: at.off + [...ch].length },
      );
    }
  }

  private moveCaret(focus: CaretPos, extend: boolean): void {
    const anchor = extend && this.sel ? this.sel.anchor : focus;
    this.setSelection(focus, anchor);
    this.drawOverlay();
  }
}
