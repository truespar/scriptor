import type { ScriptorView } from '@truespar/scriptor-core';
import { h } from './dom';
import type { Units } from './ribbon';

const THICK = 18; // ruler thickness (CSS px)
// CSS px per twip at zoom 1: 1pt = 96/72 px, 1 twip = 1/20 pt.
const PX_PER_TWIP = 96 / 72 / 20;

// Half an inch of content must survive between a margin/indent and its opposite one, so a drag can
// never collapse the text column to nothing (Word keeps a comparable floor).
const MIN_GAP = 720;

interface UnitSpec {
  majorTwips: number;
  minorDiv: number;
}

/** What a pointer can grab on the rulers. */
type Handle =
  | 'leftMargin'
  | 'rightMargin'
  | 'topMargin'
  | 'bottomMargin'
  | 'leftIndent'
  | 'firstIndent'
  | 'rightIndent';

/** Cached horizontal-ruler geometry (CSS px + twips), for hit-testing + drag. */
interface HGeo {
  k: number; // CSS px per twip
  pageTwipsW: number;
  marginLeftTwips: number;
  marginRightTwips: number;
  marginTopTwips: number;
  marginBottomTwips: number;
  mL: number; // content-left x (CSS)
  contentEnd: number; // content-right x (CSS)
  leftIndentX: number;
  firstIndentX: number;
  rightIndentX: number;
  indentLeftTwips: number;
  indentRightTwips: number;
  indentFirstTwips: number;
}
const UNIT: Record<Units, UnitSpec> = {
  us: { majorTwips: 1440, minorDiv: 8 }, // 1 inch, 1/8 ticks
  eu: { majorTwips: 1440 / 2.54, minorDiv: 10 }, // 1 cm, mm ticks
};

/**
 * The horizontal + vertical canvas rulers around the page. They read the view's page geometry,
 * zoom, and the caret paragraph's indents, and draw ticks (inch or mm), shaded margins, and the
 * paragraph's indent markers - matching Word's page chrome. Display-only for now (no drag).
 */
export class Rulers {
  readonly corner: HTMLElement;
  readonly hCanvas: HTMLCanvasElement;
  readonly vCanvas: HTMLCanvasElement;
  private readonly view: ScriptorView;
  private units: Units;
  private readonly dpr: number;
  private readonly unsub: () => void;
  private hGeo: HGeo | null = null;
  private vTopBand: { topTwips: number; bottomTwips: number; pageTwipsH: number } | null = null;
  private drag: Handle | null = null;
  private readonly tip: HTMLElement;

  constructor(view: ScriptorView, units: Units) {
    this.view = view;
    this.units = units;
    this.dpr = Math.max(1, Math.floor(globalThis.devicePixelRatio || 1));
    this.corner = h('div', { class: 'scr-corner' });
    this.corner.style.width = `${THICK}px`;
    this.corner.style.height = `${THICK}px`;
    this.hCanvas = h('canvas', { class: 'scr-hruler' }) as HTMLCanvasElement;
    this.vCanvas = h('canvas', { class: 'scr-vruler' }) as HTMLCanvasElement;
    this.tip = h('div', { class: 'scr-ruler-tip' });
    this.tip.style.display = 'none';
    document.body.append(this.tip);

    this.hCanvas.addEventListener('pointerdown', (e) => this.onDown(e, 'h'));
    this.vCanvas.addEventListener('pointerdown', (e) => this.onDown(e, 'v'));
    this.hCanvas.addEventListener('pointermove', (e) => this.onHover(e));
    this.onMove = this.onMove.bind(this);
    this.onUp = this.onUp.bind(this);

    this.unsub = view.addListener(() => this.refresh());
  }

  setUnits(units: Units): void {
    this.units = units;
    this.refresh();
  }

  destroy(): void {
    this.unsub();
    window.removeEventListener('pointermove', this.onMove);
    window.removeEventListener('pointerup', this.onUp);
    this.tip.remove();
  }

  refresh(): void {
    const g = this.view.pageGeometry();
    const k = PX_PER_TWIP * this.view.zoomLevel;
    const pageW = g.width * k;
    const pageH = g.height * k;
    const mL = g.marginLeft * k;
    const mR = g.marginRight * k;
    const mT = g.marginTop * k;
    const mB = g.marginBottom * k;
    const spec = UNIT[this.units];
    const majorPx = spec.majorTwips * k;
    const para = this.view.paragraphFormat();
    const pages = Math.max(1, this.view.pageCount());
    const gapCss = 24 * this.view.zoomLevel; // matches the view's inter-page gutter

    // Cache geometry for hit-testing + drag.
    const contentEnd = pageW - mR;
    const il = para?.indentLeft ?? 0;
    const ir = para?.indentRight ?? 0;
    const ifst = para?.indentFirst ?? 0;
    this.hGeo = {
      k,
      pageTwipsW: g.width,
      marginLeftTwips: g.marginLeft,
      marginRightTwips: g.marginRight,
      marginTopTwips: g.marginTop,
      marginBottomTwips: g.marginBottom,
      mL,
      contentEnd,
      leftIndentX: mL + il * k,
      firstIndentX: mL + (il + ifst) * k,
      rightIndentX: contentEnd - ir * k,
      indentLeftTwips: il,
      indentRightTwips: ir,
      indentFirstTwips: ifst,
    };
    this.vTopBand = { topTwips: g.marginTop, bottomTwips: g.marginBottom, pageTwipsH: g.height };

    this.drawHorizontal(pageW, mL, mR, majorPx, spec.minorDiv, para);
    this.drawVertical(pageH, mT, mB, majorPx, spec.minorDiv, pages, gapCss);
  }

  // ── drag interactivity ───────────────────────────────────────────────────────

  private onHover(e: PointerEvent): void {
    if (this.drag) return;
    this.hCanvas.style.cursor = this.hitH(e.offsetX, e.offsetY) ? 'ew-resize' : 'default';
  }

  private onDown(e: PointerEvent, which: 'h' | 'v'): void {
    if (this.view.mode === 'read') return;
    const handle = which === 'h' ? this.hitH(e.offsetX, e.offsetY) : this.hitV(e.offsetY);
    if (!handle) return;
    this.drag = handle;
    window.addEventListener('pointermove', this.onMove);
    window.addEventListener('pointerup', this.onUp);
    this.onMove(e);
    e.preventDefault();
  }

  /** Which horizontal handle is under (x,y) in CSS px (or null). Gray margin zones drag the
   *  margins; the indent triangles drag the paragraph indents. */
  private hitH(x: number, y: number): Handle | null {
    const g = this.hGeo;
    if (!g) return null;
    const near = (a: number, b: number) => Math.abs(a - b) <= 6;
    // Indent triangles first (top half = first-line/right; bottom half = left/right).
    if (y < THICK / 2) {
      if (near(x, g.firstIndentX)) return 'firstIndent';
      if (near(x, g.rightIndentX)) return 'rightIndent';
    } else {
      if (near(x, g.leftIndentX)) return 'leftIndent';
      if (near(x, g.rightIndentX)) return 'rightIndent';
    }
    // Margin boundaries / gray zones.
    if (x <= g.mL + 2) return 'leftMargin';
    if (x >= g.contentEnd - 2) return 'rightMargin';
    return null;
  }

  private hitV(y: number): Handle | null {
    const b = this.vTopBand;
    if (!b) return null;
    const k = (this.hGeo?.k ?? 0);
    const topPx = b.topTwips * k;
    const bottomStart = b.pageTwipsH * k - b.bottomTwips * k;
    if (y <= topPx + 2) return 'topMargin';
    if (y >= bottomStart - 2 && y <= b.pageTwipsH * k) return 'bottomMargin';
    return null;
  }

  private onMove(e: PointerEvent): void {
    const g = this.hGeo;
    if (!this.drag || !g) return;
    const k = g.k;
    const contentTwips = g.pageTwipsW - g.marginLeftTwips - g.marginRightTwips;
    const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
    // Pointer position in canvas-local CSS px. Every handle needs one of these, so resolve the
    // rect once per move rather than per case.
    const x = e.clientX - this.hCanvas.getBoundingClientRect().left;
    const y = e.clientY - this.vCanvas.getBoundingClientRect().top;
    let valueTwips = 0;

    switch (this.drag) {
      case 'leftMargin': {
        valueTwips = clamp(x / k, 0, g.pageTwipsW - g.marginRightTwips - MIN_GAP);
        this.view.setMargins(g.marginTopTwips, g.marginRightTwips, g.marginBottomTwips, valueTwips);
        break;
      }
      case 'rightMargin': {
        valueTwips = clamp((g.pageTwipsW * k - x) / k, 0, g.pageTwipsW - g.marginLeftTwips - MIN_GAP);
        this.view.setMargins(g.marginTopTwips, valueTwips, g.marginBottomTwips, g.marginLeftTwips);
        break;
      }
      case 'leftIndent': {
        valueTwips = clamp((x - g.mL) / k, 0, contentTwips - g.indentRightTwips - MIN_GAP);
        this.view.setIndentLeft(valueTwips);
        break;
      }
      case 'rightIndent': {
        valueTwips = clamp((g.contentEnd - x) / k, 0, contentTwips - g.indentLeftTwips - MIN_GAP);
        this.view.setIndentRight(valueTwips);
        break;
      }
      case 'firstIndent': {
        valueTwips = clamp((x - g.mL) / k - g.indentLeftTwips, -g.indentLeftTwips, contentTwips - g.indentLeftTwips - MIN_GAP);
        this.view.setIndentFirst(valueTwips);
        break;
      }
      case 'topMargin': {
        const b = this.vTopBand!;
        valueTwips = clamp(y / k, 0, b.pageTwipsH - b.bottomTwips - MIN_GAP);
        this.view.setMargins(valueTwips, g.marginRightTwips, g.marginBottomTwips, g.marginLeftTwips);
        break;
      }
      case 'bottomMargin': {
        const b = this.vTopBand!;
        valueTwips = clamp((b.pageTwipsH * k - y) / k, 0, b.pageTwipsH - b.topTwips - MIN_GAP);
        this.view.setMargins(g.marginTopTwips, g.marginRightTwips, valueTwips, g.marginLeftTwips);
        break;
      }
    }
    this.showTip(e, valueTwips);
  }

  private onUp(): void {
    this.drag = null;
    window.removeEventListener('pointermove', this.onMove);
    window.removeEventListener('pointerup', this.onUp);
    this.tip.style.display = 'none';
  }

  private showTip(e: PointerEvent, twips: number): void {
    const v = this.units === 'us' ? `${(twips / 1440).toFixed(2)}"` : `${(twips / 567).toFixed(2)} cm`;
    this.tip.textContent = v;
    this.tip.style.display = 'block';
    this.tip.style.left = `${e.clientX + 12}px`;
    this.tip.style.top = `${e.clientY + 12}px`;
  }

  private ctx(canvas: HTMLCanvasElement, cssW: number, cssH: number): CanvasRenderingContext2D {
    canvas.width = Math.round(cssW * this.dpr);
    canvas.height = Math.round(cssH * this.dpr);
    canvas.style.width = `${cssW}px`;
    canvas.style.height = `${cssH}px`;
    const ctx = canvas.getContext('2d')!;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);
    return ctx;
  }

  private drawHorizontal(
    pageW: number,
    mL: number,
    mR: number,
    majorPx: number,
    minorDiv: number,
    para: { indentLeft: number; indentRight: number; indentFirst: number } | null,
  ): void {
    const ctx = this.ctx(this.hCanvas, pageW, THICK);
    const contentStart = mL;
    const contentEnd = pageW - mR;

    // Margin zones shaded; content white.
    ctx.fillStyle = '#dfe3ec';
    ctx.fillRect(0, 0, pageW, THICK);
    ctx.fillStyle = '#ffffff';
    ctx.fillRect(contentStart, 0, contentEnd - contentStart, THICK);

    // Ticks from the left margin (0) rightward.
    ctx.strokeStyle = '#9aa3b8';
    ctx.fillStyle = '#5b647c';
    ctx.font = '9px system-ui, sans-serif';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.lineWidth = 1;
    for (let n = 0; ; n++) {
      const x = contentStart + n * majorPx;
      if (x > pageW + 0.5) break;
      line(ctx, x, 4, x, THICK - 4);
      if (n > 0) ctx.fillText(String(n), x, THICK / 2);
      for (let m = 1; m < minorDiv; m++) {
        const mx = x + (m * majorPx) / minorDiv;
        if (mx > pageW) break;
        const tall = minorDiv % 2 === 0 && m === minorDiv / 2;
        line(ctx, mx, tall ? 6 : THICK - 7, mx, THICK - 6);
      }
    }

    // Margin-boundary markers: crisp vertical lines at the left-margin end (content start) and the
    // right-margin start (content end) - the horizontal counterpart to the vertical ruler's
    // top/bottom boundary lines, so both rulers mark the content box the same way. Drawn before the
    // indent triangles so those stay on top.
    ctx.strokeStyle = '#7c8595'; // a shade darker than the ticks
    ctx.lineWidth = 1;
    line(ctx, contentStart, 0, contentStart, THICK); // full thickness, matching the vertical ruler
    line(ctx, contentEnd, 0, contentEnd, THICK);

    // Indent markers (triangles) for the caret paragraph.
    if (para) {
      const k = PX_PER_TWIP * this.view.zoomLevel;
      const leftX = contentStart + para.indentLeft * k;
      const firstX = contentStart + (para.indentLeft + para.indentFirst) * k;
      const rightX = contentEnd - para.indentRight * k;
      ctx.fillStyle = '#5b647c';
      marker(ctx, firstX, 0, true); // first-line: top triangle pointing down
      marker(ctx, leftX, THICK, false); // left indent: bottom triangle pointing up
      marker(ctx, rightX, THICK, false); // right indent: bottom triangle pointing up
    }
  }

  private drawVertical(
    pageH: number,
    mT: number,
    mB: number,
    majorPx: number,
    minorDiv: number,
    pages: number,
    gapCss: number,
  ): void {
    // Span the whole stacked document; draw one page's scale per page (Word resets per page).
    const total = pages * pageH + (pages - 1) * gapCss;
    const ctx = this.ctx(this.vCanvas, THICK, total);
    // Paint the whole strip the page-stage backdrop first, so the GAP between page ruler-segments
    // reads as the same desk as the page gap beside it (each page's ruler is drawn over this). Left
    // unpainted, the gap fell back to the canvas's chrome background - a differently-coloured band
    // next to the desk-coloured page gap, which looked like a separate plane.
    ctx.fillStyle = getComputedStyle(this.vCanvas).getPropertyValue('--scr-bg-stage').trim() || '#eef0f5';
    ctx.fillRect(0, 0, THICK, total);
    ctx.font = '9px system-ui, sans-serif';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.lineWidth = 1;

    for (let p = 0; p < pages; p++) {
      const top = p * (pageH + gapCss);
      const contentStart = top + mT;
      const contentEnd = top + pageH - mB;

      ctx.fillStyle = '#dfe3ec';
      ctx.fillRect(0, top, THICK, pageH);
      ctx.fillStyle = '#ffffff';
      ctx.fillRect(0, contentStart, THICK, contentEnd - contentStart);

      ctx.strokeStyle = '#9aa3b8';
      ctx.fillStyle = '#5b647c';
      for (let n = 0; ; n++) {
        const y = contentStart + n * majorPx;
        if (y > top + pageH + 0.5) break;
        line(ctx, 4, y, THICK - 4, y);
        if (n > 0) {
          ctx.save();
          ctx.translate(THICK / 2, y);
          ctx.rotate(-Math.PI / 2);
          ctx.fillText(String(n), 0, 0);
          ctx.restore();
        }
        for (let m = 1; m < minorDiv; m++) {
          const my = y + (m * majorPx) / minorDiv;
          if (my > top + pageH) break;
          const tall = minorDiv % 2 === 0 && m === minorDiv / 2;
          line(ctx, tall ? 6 : THICK - 7, my, THICK - 6, my);
        }
      }

      // Margin-boundary markers: a crisp full-width line where the top margin ends (content begins)
      // and where the bottom margin begins (footer area) - Word's draggable top/bottom margin
      // handles. The grey->white shading alone left the boundary implicit; these make it explicit and
      // signal the grab zones that `hitV` already targets.
      ctx.strokeStyle = '#7c8595'; // a shade darker than the ticks
      ctx.lineWidth = 1;
      hline(ctx, contentStart);
      hline(ctx, contentEnd);
    }
  }
}

function line(ctx: CanvasRenderingContext2D, x1: number, y1: number, x2: number, y2: number): void {
  ctx.beginPath();
  ctx.moveTo(x1 + 0.5, y1);
  ctx.lineTo(x2 + 0.5, y2);
  ctx.stroke();
}

/** A crisp full-thickness horizontal rule at `y` across the vertical ruler (the `+0.5` snaps it to a
 *  device pixel, like `line`'s x offset for vertical strokes). */
function hline(ctx: CanvasRenderingContext2D, y: number): void {
  ctx.beginPath();
  ctx.moveTo(0, Math.round(y) + 0.5);
  ctx.lineTo(THICK, Math.round(y) + 0.5);
  ctx.stroke();
}

/** A small indent-marker triangle at canvas x `x`. `top` = anchored to the top edge pointing down,
 *  else anchored to the bottom edge pointing up. */
function marker(ctx: CanvasRenderingContext2D, x: number, edgeY: number, top: boolean): void {
  const s = 4;
  ctx.beginPath();
  if (top) {
    ctx.moveTo(x - s, 0);
    ctx.lineTo(x + s, 0);
    ctx.lineTo(x, s + 1);
  } else {
    ctx.moveTo(x - s, edgeY);
    ctx.lineTo(x + s, edgeY);
    ctx.lineTo(x, edgeY - s - 1);
  }
  ctx.closePath();
  ctx.fill();
}
