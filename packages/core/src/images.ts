// The picture subsystem: selection, resize, move, crop and wrap.
//
// This owns the four pieces of state a selected picture needs - which image is selected, any drag
// in progress, whether the crop handles are showing, and the decoded bitmap the crop overlay draws.
// Nothing else in the view reads them, which is what let this move out.
//
// It reaches the rest of the view through [`ImageHost`] rather than holding the view itself: the
// interface is the whole contract, so what the pictures need from everything else is one list you
// can read, not something to infer from 477 lines.

import { pxToEmu, type ScriptorDoc } from '@truespar/scriptor-wasm';

import { decodeImageSize, EMU_PER_TWIP, pickImageFile, RESIZE_CURSORS, SELECT_BLUE } from './helpers';
import type { CaretPos, ImageDrag, ScriptorContextMenuItem, ScriptorOptions, Selection } from './types';

/** What the picture subsystem needs from the view around it. */
export interface ImageHost {
  readonly doc: ScriptorDoc;
  readonly sheet: HTMLDivElement;
  readonly octx: CanvasRenderingContext2D;
  readonly keys: HTMLTextAreaElement;
  readonly opts: ScriptorOptions;
  readonly dpr: number;
  readonly zoom: number;
  readonly pageH: number;
  readonly pageGap: number;
  readonly trackChanges: boolean;
  readonly deviceW: number;
  dragging: boolean;
  sel: Selection | null;
  caretOn: boolean;
  bodyCaret(): CaretPos;
  hit(e: MouseEvent): CaretPos;
  toCanvas(e: MouseEvent): [number, number];
  setSelection(focus: CaretPos, anchor?: CaretPos): void;
  drawOverlay(): void;
  fullRender(): void;
  requestRender(): void;
  fireChange(): void;
  notify(): void;
}

export class ImageController {
  private readonly h: ImageHost;

  constructor(host: ImageHost) {
    this.h = host;
  }

  // The currently selected editable picture (its engine id), or null. Mutually exclusive with the text
  // selection: selecting a picture clears `sel`, and setting a caret/selection clears this. Drives the
  // overlay's selection box + resize handles and routes Delete / arrow keys to the picture (P3). The id
  // is a bigint (the wasm image ids are u64); geometry is re-fetched from `imageRect` on every draw, so
  // it follows the picture across relayouts.
  selectedImage: bigint | null = null;

  // An in-progress picture drag (resize via a handle, or reposition). Null when not dragging.
  imageDrag: ImageDrag | null = null;

  // Crop mode (Picture Tools > Crop): the selected picture shows its full (uncropped) bitmap dimmed
  // behind a bright crop window whose handles set `<a:srcRect>` + the display extent. `cropBitmap` is
  // the decoded original (held only while cropping; closed on exit).
  cropMode = false;

  cropBitmap: ImageBitmap | null = null;

  /** Insert a picture from the user's filesystem at the caret (the Insert > Pictures command): open a
   *  file picker, read the bytes, size the picture from its natural pixels (capped to the text-column
   *  width so a large photo fits the page), insert it inline at the caret, and select it. Edit mode
   *  only; resolves when the picture lands or the dialog is cancelled. */
  async insertPicture(): Promise<void> {
    if (this.h.opts.mode !== 'edit') return;
    const file = await pickImageFile();
    if (!file) return;
    const bytes = new Uint8Array(await file.arrayBuffer());
    const mime = file.type || 'image/png';
    const [natW, natH] = await decodeImageSize(bytes, mime);
    // Natural pixels are 96 dpi (scale 1.0); cap the width to the text column.
    let wEmu = pxToEmu(natW, 1);
    let hEmu = pxToEmu(natH, 1);
    const geo = this.h.doc.pageGeometry(); // [w, h, mT, mR, mB, mL, ...] twips
    const contentTwips = Math.max(1, geo[0]! - geo[5]! - geo[3]!);
    const maxEmu = contentTwips * EMU_PER_TWIP;
    if (wEmu > maxEmu) {
      hEmu *= maxEmu / wEmu;
      wEmu = maxEmu;
    }
    const at = this.h.bodyCaret();
    try {
      if (this.h.trackChanges) this.h.doc.setNow(new Date().toISOString()); // stamp a tracked insertion
      const id = this.h.doc.insertImage(at.para, at.off, bytes, mime, wEmu, hEmu);
      this.h.fireChange();
      this.h.fullRender(); // a new run + media shifts geometry - force a full repaint
      this.selectImage(id);
    } catch (err) {
      console.error('scriptor insert picture error', err);
    }
  }

  /** The currently selected editable picture's engine id, or null. Lets host chrome (the ribbon)
   *  enable picture-only actions (crop, wrap, delete) and read the selection. */
  get selectedImageId(): bigint | null {
    return this.selectedImage;
  }

  /** Select picture `id`: clears any text selection, draws its box + resize handles, and routes
   *  keyboard (Delete / Esc / arrows) to the picture. Focuses the offscreen textarea so those keys
   *  arrive here. */
  selectImage(id: bigint): void {
    if (this.selectedImage !== id) this.exitCropMode(); // selecting a different picture leaves crop
    this.selectedImage = id;
    this.h.sel = null;
    this.h.caretOn = false;
    this.h.keys.focus({ preventScroll: true });
    this.h.opts.onSelectionChange?.(null);
    this.h.notify();
    this.h.drawOverlay();
  }

  /** Clear the picture selection (no-op if none). Returns whether anything was selected. */
  deselectImage(): boolean {
    if (this.selectedImage === null) return false;
    this.exitCropMode();
    this.selectedImage = null;
    this.imageDrag = null;
    this.h.notify();
    this.h.drawOverlay();
    return true;
  }

  /** Remove the selected picture (Delete / Backspace): drop its run + placement, land the caret where
   *  its anchor was (clamped into the paragraph), and re-render. No-op if nothing is selected. */
  removeSelectedImage(): void {
    const id = this.selectedImage;
    if (id === null) return;
    // Resolve a caret near the picture's anchor before removing it (its old top-left, hit-tested on
    // the still-current layout), so the caret lands where the picture was rather than at the top.
    let caret: CaretPos = { para: 0, off: 0 };
    const r = this.h.doc.imageRect(id);
    if (r && r.length >= 4) {
      const p = this.h.doc.hitTest(r[0]!, r[1]!);
      caret = { para: p[0]!, off: p[1]! };
    }
    this.selectedImage = null;
    this.imageDrag = null;
    try {
      if (this.h.trackChanges) this.h.doc.setNow(new Date().toISOString()); // stamp a tracked deletion
      this.h.doc.removeImage(id);
      this.h.fireChange();
      const len = this.h.doc.paragraphLength(caret.para);
      this.h.setSelection({ para: caret.para, off: Math.min(caret.off, len) });
      this.h.fullRender(); // a removed run + media shifts geometry - force a full repaint
    } catch (err) {
      console.error('scriptor image remove error', err);
      this.h.drawOverlay();
    }
  }

  /** Whether crop mode is active (drives the ribbon Crop button's pressed state). */
  get cropActive(): boolean {
    return this.cropMode;
  }

  /** The selected picture's wrap state token - one of `inline` / `square` / `tight` / `through` /
   *  `topAndBottom` / `behind` / `front` - or null when no picture is selected. Drives the Wrap Text
   *  menu's current value. */
  get selectedImageWrap(): string | null {
    const id = this.selectedImage;
    if (id === null) return null;
    return this.h.doc.imageWrapState(id) ?? null;
  }

  /** Set how the selected picture sits relative to the text (the Wrap Text command). `token` is one
   *  of `inline` (in the flow), `square` / `tight` / `through` / `topAndBottom` (floating, text wraps),
   *  `behind` (under the text) or `front` (over the text). No-op without a selected picture. */
  setImageWrap(token: string): void {
    const id = this.selectedImage;
    if (id === null) return;
    this.exitCropMode();
    try {
      if (token === 'inline') this.h.doc.setImageFloating(id, false, '', false);
      else if (token === 'behind') this.h.doc.setImageFloating(id, true, 'none', true);
      else if (token === 'front') this.h.doc.setImageFloating(id, true, 'none', false);
      else this.h.doc.setImageFloating(id, true, token, false); // square / tight / through / topAndBottom
      this.h.fireChange();
      this.h.fullRender();
    } catch (err) {
      console.error('scriptor image wrap error', err);
    }
  }

  /** Reset the selected picture's crop (Word's "Reset Crop"): un-crop it, restoring the full image at
   *  the same scale. No-op without a selected (cropped) picture. */
  resetCrop(): void {
    const id = this.selectedImage;
    if (id === null) return;
    this.exitCropMode();
    try {
      if (this.h.doc.resetImageCrop(id)) {
        this.h.fireChange();
        this.h.requestRender();
      }
    } catch (err) {
      console.error('scriptor reset crop error', err);
    }
  }

  /** Reset the selected picture to its natural aspect ratio: keep the current display width, set the
   *  height from the decoded image's proportions - undoing a non-proportional resize. No-op without a
   *  selected picture (or if the image can't be decoded). */
  async resetAspect(): Promise<void> {
    const id = this.selectedImage;
    if (id === null) return;
    const bytes = this.h.doc.imageMedia(id);
    if (!bytes || bytes.length === 0) return;
    const [nw, nh] = await decodeImageSize(bytes, 'image/png'); // type is only a hint; decode sniffs
    if (nw <= 0 || nh <= 0 || this.selectedImage !== id) return;
    const rect = this.h.doc.imageRect(id);
    if (!rect || rect.length < 4) return;
    const scale = this.h.dpr * this.h.zoom;
    const wEmu = pxToEmu(rect[2]!, scale); // keep the current display width
    try {
      this.h.doc.setImageSize(id, wEmu, wEmu * (nh / nw));
      this.h.fireChange();
      this.h.requestRender();
    } catch (err) {
      console.error('scriptor reset aspect error', err);
    }
  }

  /** Toggle crop mode for the selected picture (the Crop command). Entering decodes the original
   *  bitmap so the full image shows behind the crop window; toggling again (or Enter/Esc/click-away)
   *  exits. No-op without a selected picture. */
  toggleCrop(): void {
    if (this.cropMode) {
      this.exitCropMode();
      this.h.drawOverlay();
      return;
    }
    if (this.selectedImage === null) return;
    void this.enterCropMode();
  }

  async enterCropMode(): Promise<void> {
    const id = this.selectedImage;
    if (id === null) return;
    const bytes = this.h.doc.imageMedia(id);
    if (!bytes || bytes.length === 0) return; // nothing to decode (shouldn't happen for a placed picture)
    let bmp: ImageBitmap | null = null;
    try {
      if (typeof createImageBitmap === 'function') {
        bmp = await createImageBitmap(new Blob([bytes as BlobPart]));
      }
    } catch {
      bmp = null;
    }
    if (bmp === null) return; // without the original we can't show a crop frame
    if (this.selectedImage !== id) {
      bmp.close();
      return; // selection changed while decoding
    }
    this.cropBitmap?.close();
    this.cropBitmap = bmp;
    this.cropMode = true;
    this.h.notify();
    this.h.drawOverlay();
  }

  exitCropMode(): void {
    if (!this.cropMode && this.cropBitmap === null) return;
    this.cropMode = false;
    this.cropBitmap?.close();
    this.cropBitmap = null;
    if (this.imageDrag?.mode === 'crop') this.imageDrag = null;
    this.h.notify();
  }

  /** The full (uncropped) display rect of picture `id` in canvas px: the rect the whole original image
   *  would occupy, derived from its currently-shown (cropped) rect + crop fractions. Returns null if
   *  the picture isn't placed or is cropped to nothing. */
  cropFullRect(id: bigint): { x: number; y: number; w: number; h: number } | null {
    const disp = this.h.doc.imageRect(id);
    const crop = this.h.doc.imageCrop(id);
    if (!disp || disp.length < 4 || !crop) return null;
    const visW = 1 - (crop[0]! + crop[2]!) / 1e5;
    const visH = 1 - (crop[1]! + crop[3]!) / 1e5;
    if (visW <= 0 || visH <= 0) return null;
    const w = disp[2]! / visW;
    const h = disp[3]! / visH;
    return { x: disp[0]! - w * (crop[0]! / 1e5), y: disp[1]! - h * (crop[1]! / 1e5), w, h };
  }

  /** The proposed crop window (canvas px) for an in-progress crop drag: the grabbed edge/corner moves
   *  with the pointer, clamped inside the full image and to a small minimum. */
  cropWindow(
    d: ImageDrag,
    curX: number,
    curY: number,
    full: { x: number; y: number; w: number; h: number },
  ): { x: number; y: number; w: number; h: number } {
    const dx = curX - d.startX;
    const dy = curY - d.startY;
    const h = d.handle;
    const min = 8 * this.h.dpr;
    let x0 = d.rectX;
    let y0 = d.rectY;
    let x1 = d.rectX + d.rectW;
    let y1 = d.rectY + d.rectH;
    const clamp = (v: number, lo: number, hi: number): number => Math.min(Math.max(v, lo), hi);
    if (h === 'nw' || h === 'w' || h === 'sw') x0 = clamp(d.rectX + dx, full.x, x1 - min);
    if (h === 'ne' || h === 'e' || h === 'se')
      x1 = clamp(d.rectX + d.rectW + dx, x0 + min, full.x + full.w);
    if (h === 'nw' || h === 'n' || h === 'ne') y0 = clamp(d.rectY + dy, full.y, y1 - min);
    if (h === 'sw' || h === 's' || h === 'se')
      y1 = clamp(d.rectY + d.rectH + dy, y0 + min, full.y + full.h);
    return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
  }

  /** Commit a crop drag: the crop window's size becomes the new display extent (so the picture keeps
   *  its on-screen scale and just shows less) and its position within the full image becomes the new
   *  `<a:srcRect>`. Pushes both through the engine, then re-renders (staying in crop mode). */
  commitCrop(d: ImageDrag): void {
    const full = this.cropFullRect(d.id);
    if (!full || full.w <= 0 || full.h <= 0) return;
    const win = { x: d.curX, y: d.curY, w: d.curW, h: d.curH };
    const clampPct = (v: number): number => Math.min(Math.max(Math.round(v), 0), 100000);
    const l = clampPct(((win.x - full.x) / full.w) * 1e5);
    const t = clampPct(((win.y - full.y) / full.h) * 1e5);
    const r = clampPct(((full.x + full.w - (win.x + win.w)) / full.w) * 1e5);
    const b = clampPct(((full.y + full.h - (win.y + win.h)) / full.h) * 1e5);
    const scale = this.h.dpr * this.h.zoom;
    try {
      this.h.doc.setImageCrop(d.id, l, t, r, b);
      this.h.doc.setImageSize(d.id, pxToEmu(win.w, scale), pxToEmu(win.h, scale));
      this.h.fireChange();
      this.h.requestRender();
    } catch (err) {
      console.error('scriptor image crop error', err);
      this.h.drawOverlay();
    }
  }

  /** The 8 resize-handle centres for an image canvas rect `[x, y, w, h]` (4 corners + 4 edge
   *  midpoints), named by compass direction. Shared by the overlay (drawing) and the hit-test. */
  imageHandles(rect: Float32Array): { name: string; x: number; y: number }[] {
    const x = rect[0]!;
    const y = rect[1]!;
    const w = rect[2]!;
    const h = rect[3]!;
    const mx = x + w / 2;
    const my = y + h / 2;
    return [
      { name: 'nw', x, y },
      { name: 'n', x: mx, y },
      { name: 'ne', x: x + w, y },
      { name: 'w', x, y: my },
      { name: 'e', x: x + w, y: my },
      { name: 'sw', x, y: y + h },
      { name: 's', x: mx, y: y + h },
      { name: 'se', x: x + w, y: y + h },
    ];
  }

  /** Draw the selected picture's box + resize handles on the overlay. No-op if nothing is selected or
   *  the picture isn't currently placed (off-page). */
  drawImageSelection(): void {
    const id = this.selectedImage;
    if (id === null) return;
    if (this.cropMode && this.cropBitmap) {
      this.drawCropOverlay(id, this.cropBitmap);
      return;
    }
    // While resizing, draw the proposed rect (the live preview); otherwise the picture's current rect.
    const d = this.imageDrag;
    const r: Float32Array =
      d && d.id === id
        ? Float32Array.of(d.curX, d.curY, d.curW, d.curH)
        : (this.h.doc.imageRect(id) ?? Float32Array.of());
    if (r.length < 4) return;
    const o = this.h.octx;
    o.save();
    o.strokeStyle = SELECT_BLUE;
    o.lineWidth = Math.max(1, this.h.dpr);
    o.strokeRect(r[0]!, r[1]!, r[2]!, r[3]!);
    // Resize handles: solid white knobs with an opaque accent border + a soft shadow, so they read on
    // a white page and stay visible beside the OS resize cursor. Sized in CSS px (scaled by dpr) and
    // floored so they don't vanish on a low-dpr display.
    const hs = Math.max(5, 5 * this.h.dpr); // half-size (~10 CSS px squares)
    o.lineWidth = Math.max(2, 2 * this.h.dpr);
    o.fillStyle = '#ffffff';
    o.shadowColor = 'rgba(0, 0, 0, 0.35)';
    o.shadowBlur = Math.max(2, this.h.dpr);
    for (const hnd of this.imageHandles(r)) {
      o.fillRect(hnd.x - hs, hnd.y - hs, hs * 2, hs * 2);
    }
    o.shadowColor = 'transparent'; // borders without the shadow doubling up
    o.strokeStyle = SELECT_BLUE;
    for (const hnd of this.imageHandles(r)) {
      o.strokeRect(hnd.x - hs, hnd.y - hs, hs * 2, hs * 2);
    }
    o.restore();
  }

  /** Draw the crop UI: the full (uncropped) original dimmed, the crop window bright (the original
   *  clipped to it), and a dark frame + handles on the window. The window is the live drag rect while
   *  cropping, else the picture's current display rect. */
  drawCropOverlay(id: bigint, bmp: ImageBitmap): void {
    const full = this.cropFullRect(id);
    if (!full) return;
    const d = this.imageDrag;
    let win: { x: number; y: number; w: number; h: number } | null;
    if (d && d.mode === 'crop' && d.id === id) {
      win = { x: d.curX, y: d.curY, w: d.curW, h: d.curH };
    } else {
      const r = this.h.doc.imageRect(id);
      win = r && r.length >= 4 ? { x: r[0]!, y: r[1]!, w: r[2]!, h: r[3]! } : null;
    }
    if (!win) return;
    const o = this.h.octx;
    o.save();
    // The full original, dimmed (what cropping out would hide).
    o.globalAlpha = 0.35;
    o.drawImage(bmp, full.x, full.y, full.w, full.h);
    // The crop window, at full opacity (the original clipped to the window).
    o.globalAlpha = 1;
    o.save();
    o.beginPath();
    o.rect(win.x, win.y, win.w, win.h);
    o.clip();
    o.drawImage(bmp, full.x, full.y, full.w, full.h);
    o.restore();
    // Frame + handles on the crop window.
    o.strokeStyle = 'rgba(0, 0, 0, 0.9)';
    o.lineWidth = Math.max(1, this.h.dpr);
    o.strokeRect(win.x, win.y, win.w, win.h);
    const hs = 4 * this.h.dpr;
    o.fillStyle = '#000000';
    for (const hnd of this.imageHandles(Float32Array.of(win.x, win.y, win.w, win.h))) {
      o.fillRect(hnd.x - hs, hnd.y - hs, hs * 2, hs * 2);
    }
    o.restore();
  }

  /** Set the overlay cursor to hint picture interactions: a resize cursor over a selected picture's
   *  handle, a pointer over any picture body. Skipped mid-drag (the grabbed cursor stands). */
  onImageHover = (e: MouseEvent): void => {
    if (this.h.opts.mode !== 'edit' || this.imageDrag) return;
    const [cx, cy] = this.h.toCanvas(e);
    const grab = this.imageHandleAt(cx, cy);
    if (grab) {
      this.h.sheet.style.cursor = RESIZE_CURSORS[grab.name] ?? 'default';
      return;
    }
    const over = this.h.doc.imageAtPoint(cx, cy);
    // A move cursor over the selected picture's body (you can drag to reposition); a pointer over any
    // other picture (click to select). Crop mode shows neither - the body is a no-op there.
    if (!this.cropMode && over !== undefined && over === this.selectedImage) {
      this.h.sheet.style.cursor = 'move';
    } else {
      this.h.sheet.style.cursor = over !== undefined ? 'pointer' : '';
    }
  };

  /** Commit (or discard) an in-progress picture drag. A real resize converts the proposed rect to EMU
   *  at the current zoom and pushes it through `setImageSize`; a non-move (a click on a handle that
   *  didn't drag) just snaps the box back. */
  finishImageDrag(): void {
    const d = this.imageDrag;
    this.imageDrag = null;
    if (!d) return;
    if (!d.moved) {
      this.h.drawOverlay();
      return;
    }
    if (d.mode === 'resize') {
      const scale = this.h.dpr * this.h.zoom;
      try {
        this.h.doc.setImageSize(d.id, pxToEmu(d.curW, scale), pxToEmu(d.curH, scale));
        this.h.fireChange();
        this.h.requestRender();
      } catch (err) {
        console.error('scriptor image resize error', err);
        this.h.drawOverlay();
      }
    } else if (d.mode === 'crop') {
      this.commitCrop(d);
    } else if (d.mode === 'move') {
      this.commitMove(d);
    } else {
      this.h.drawOverlay();
    }
  }

  /** Handle a mousedown on the picture layer: grab a resize handle on the selected picture, else
   *  select the picture under the pointer. Returns whether it consumed the event (so the caret logic
   *  is skipped). Edit mode only. */
  imageMouseDown(cx: number, cy: number, e: MouseEvent): boolean {
    if (this.h.opts.mode !== 'edit') return false;
    // Crop mode: the handles edit the crop window; a click inside the picture is ignored; a click
    // outside exits crop and falls through to normal selection / caret handling.
    if (this.cropMode && this.selectedImage !== null) {
      const cropGrab = this.imageHandleAt(cx, cy);
      if (cropGrab) {
        const r = cropGrab.rect;
        this.imageDrag = {
          id: this.selectedImage,
          mode: 'crop',
          handle: cropGrab.name,
          startX: cx,
          startY: cy,
          rectX: r[0]!,
          rectY: r[1]!,
          rectW: r[2]!,
          rectH: r[3]!,
          aspect: r[3]! > 0 ? r[2]! / r[3]! : 1,
          moved: false,
          curX: r[0]!,
          curY: r[1]!,
          curW: r[2]!,
          curH: r[3]!,
        };
        e.preventDefault();
        return true;
      }
      if (this.h.doc.imageAtPoint(cx, cy) === this.selectedImage) {
        e.preventDefault();
        return true; // a click on the picture body (not a handle) is a no-op in crop mode
      }
      this.exitCropMode(); // clicked away - leave crop, then handle the click normally below
    }
    // Grab a resize handle on the already-selected picture (handles sit on/just outside the box edge,
    // so this must run before the body hit-test below).
    const grab = this.imageHandleAt(cx, cy);
    if (grab) {
      const r = grab.rect;
      this.imageDrag = {
        id: this.selectedImage!,
        mode: 'resize',
        handle: grab.name,
        startX: cx,
        startY: cy,
        rectX: r[0]!,
        rectY: r[1]!,
        rectW: r[2]!,
        rectH: r[3]!,
        aspect: r[3]! > 0 ? r[2]! / r[3]! : 1,
        moved: false,
        curX: r[0]!,
        curY: r[1]!,
        curW: r[2]!,
        curH: r[3]!,
      };
      this.h.sheet.style.cursor = RESIZE_CURSORS[grab.name] ?? 'default';
      e.preventDefault();
      return true;
    }
    const id = this.h.doc.imageAtPoint(cx, cy);
    if (id === undefined) return false;
    if (this.selectedImage !== id) this.selectImage(id);
    // Arm a move drag from the picture body: dragging repositions a floating picture (or floats an
    // inline one); a click without a drag just leaves it selected.
    const r = this.h.doc.imageRect(id);
    if (r && r.length >= 4) {
      this.imageDrag = {
        id,
        mode: 'move',
        handle: '',
        startX: cx,
        startY: cy,
        rectX: r[0]!,
        rectY: r[1]!,
        rectW: r[2]!,
        rectH: r[3]!,
        aspect: r[3]! > 0 ? r[2]! / r[3]! : 1,
        moved: false,
        curX: r[0]!,
        curY: r[1]!,
        curW: r[2]!,
        curH: r[3]!,
      };
    }
    e.preventDefault();
    return true;
  }

  /** The proposed rect for a move drag: the picture translated by the pointer delta, clamped to stay
   *  on its page (the full page width horizontally, the page band vertically). */
  moveRect(
    d: ImageDrag,
    curX: number,
    curY: number,
  ): { x: number; y: number; w: number; h: number } {
    const band = this.h.pageH + this.h.pageGap;
    const pageTop = band > 0 ? Math.floor((d.rectY + d.rectH / 2) / band) * band : 0;
    const clamp = (v: number, lo: number, hi: number): number => Math.min(Math.max(v, lo), hi);
    return {
      x: clamp(d.rectX + (curX - d.startX), 0, Math.max(0, this.h.deviceW - d.rectW)),
      y: clamp(d.rectY + (curY - d.startY), pageTop, pageTop + Math.max(0, this.h.pageH - d.rectH)),
      w: d.rectW,
      h: d.rectH,
    };
  }

  /** Commit a move drag: float an inline picture (square wrap), then set its page-relative position
   *  from the dropped top-left (canvas px -> EMU from the page's top-left). Re-renders. */
  commitMove(d: ImageDrag): void {
    const scale = this.h.dpr * this.h.zoom;
    const band = this.h.pageH + this.h.pageGap;
    const pageTop = band > 0 ? Math.floor((d.rectY + d.rectH / 2) / band) * band : 0;
    try {
      if (this.h.doc.imageWrapState(d.id) === 'inline')
        this.h.doc.setImageFloating(d.id, true, 'square', false);
      this.h.doc.setImagePosition(
        d.id,
        'page',
        pxToEmu(d.curX, scale),
        'page',
        pxToEmu(d.curY - pageTop, scale),
      );
      this.h.fireChange();
      this.h.requestRender();
    } catch (err) {
      console.error('scriptor image move error', err);
      this.h.drawOverlay();
    }
  }

  /** The resize handle of the selected picture within grab tolerance of canvas point `(x, y)`, or
   *  null. Returns the handle name + the picture's current rect (so the caller seeds the drag). */
  imageHandleAt(x: number, y: number): { name: string; rect: Float32Array } | null {
    const id = this.selectedImage;
    if (id === null) return null;
    const rect = this.h.doc.imageRect(id);
    if (!rect) return null;
    const tol = 6 * this.h.dpr;
    for (const h of this.imageHandles(rect)) {
      if (Math.abs(x - h.x) <= tol && Math.abs(y - h.y) <= tol) return { name: h.name, rect };
    }
    return null;
  }

  /** The proposed picture rect (canvas px) for an in-progress resize drag at pointer `(curX, curY)`.
   *  The handle's opposite edge/corner stays fixed; `shift` locks the aspect ratio. Sizes clamp to a
   *  small minimum so the picture can't invert or vanish. Pure geometry - the model is only touched on
   *  drop. */
  resizeRect(
    d: ImageDrag,
    curX: number,
    curY: number,
    shift: boolean,
  ): { x: number; y: number; w: number; h: number } {
    const dx = curX - d.startX;
    const dy = curY - d.startY;
    const h = d.handle;
    const west = h === 'nw' || h === 'w' || h === 'sw';
    const east = h === 'ne' || h === 'e' || h === 'se';
    const north = h === 'nw' || h === 'n' || h === 'ne';
    const south = h === 'sw' || h === 's' || h === 'se';
    let w = d.rectW + (east ? dx : west ? -dx : 0);
    let ht = d.rectH + (south ? dy : north ? -dy : 0);
    const min = 8 * this.h.dpr;
    w = Math.max(min, w);
    ht = Math.max(min, ht);
    if (shift && d.aspect > 0) {
      // Vertical-only handles drive width from height; everything else drives height from width.
      if (h === 'n' || h === 's') w = ht * d.aspect;
      else ht = w / d.aspect;
    }
    // Keep the handle's opposite edge anchored: a west/north grab moves the left/top edge.
    const x = west ? d.rectX + (d.rectW - w) : d.rectX;
    const y = north ? d.rectY + (d.rectH - ht) : d.rectY;
    return { x, y, w, h: ht };
  }

  /** A selected picture's menu items: crop / reset / wrap / delete - the home for the less-used
   *  picture commands (kept consistent with the ribbon's contextual buttons). */
  imageMenuItems(): ScriptorContextMenuItem[] {
    const item = (label: string, onClick: () => void): ScriptorContextMenuItem => ({
      label,
      onClick,
    });
    return [
      item('Crop', () => this.toggleCrop()),
      item('Reset Crop', () => this.resetCrop()),
      item('Reset Aspect Ratio', () => void this.resetAspect()),
      { separator: true },
      item('Wrap: In Line with Text', () => this.setImageWrap('inline')),
      item('Wrap: Square', () => this.setImageWrap('square')),
      item('Wrap: Behind Text', () => this.setImageWrap('behind')),
      item('Wrap: In Front of Text', () => this.setImageWrap('front')),
      { separator: true },
      item('Delete Picture', () => this.removeSelectedImage()),
    ];
  }
}
