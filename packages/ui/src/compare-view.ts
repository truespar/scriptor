import type { AlignEntry, ChangeMarker, CompareChange } from '@truespar/scriptor-core';
import { h, injectStyles } from './dom';
import { icon } from './icons';
import { Viewer } from './viewer';

/** The inputs a [`CompareView`] renders: the two source documents plus the comparison's paragraph
 *  correspondence (for scroll-lock) and change list (for navigation). */
export interface CompareViewData {
  original: Uint8Array;
  revised: Uint8Array;
  alignment: AlignEntry[];
  changes: CompareChange[];
  /** Optional labels shown under "Original" / "Revised" (e.g. the file / version names). */
  originalName?: string;
  revisedName?: string;
  /** Invoked by the in-view "Close" control - the host returns to the redline view. */
  onClose?: () => void;
}

/** A scroll anchor: paragraph `a` in the original lines up with paragraph `b` in the revised. */
interface Anchor {
  a: number;
  b: number;
}

/**
 * The side-by-side comparison review surface: the **original** and
 * **revised** documents rendered read-only in a split, **semantically scroll-locked** via the
 * comparison's paragraph alignment (clause 5 stays across from clause 5 even at different lengths),
 * with next/previous change stepping both panes. A review lens - to *act* on changes, use the redline.
 */
export class CompareView {
  readonly element: HTMLElement;
  private readonly left: Viewer; // original
  private readonly right: Viewer; // revised
  private readonly leftStage: HTMLElement;
  private readonly rightStage: HTMLElement;
  /** Equal/edited pairs (both sides present), in document order - the scroll-lock anchors. */
  private readonly anchors: Anchor[];
  /** Changed entries (edited/delete/insert), in document order - the navigation targets. */
  private readonly changes: AlignEntry[];
  /** The manifest changes, keyed for `revealByRevisionId` (revision id -> original paragraph). */
  private readonly changeById: Map<number, CompareChange>;
  private readonly counter: HTMLElement;
  /** scrollTop values we assigned programmatically, per pane: their (async) scroll events are
   *  echoes to consume, not user scrolls to mirror. Suppressing by value instead of by time means
   *  a fast user drag on the other pane is never swallowed while a guard window elapses. */
  private readonly expected: { left: number | null; right: number | null } = {
    left: null,
    right: null,
  };
  /** The previous scroll event (pane, position, time) - the velocity source for extrapolation. */
  private lastEvent: { pane: 'left' | 'right'; top: number; time: number } | null = null;
  /** Debounced exact-alignment pass for when the scroll events stop. */
  private settleTimer = 0;
  /** Content-space anchor positions per pane, aligned index-for-index; null until built (and after
   *  any re-render invalidates it). */
  private map: { l: number[]; r: number[] } | null = null;
  private readonly unsubs: Array<() => void> = [];
  private navIndex = -1;
  private disposed = false;

  static async create(container: HTMLElement, data: CompareViewData): Promise<CompareView> {
    injectStyles();
    const element = h('div', { class: 'scr-compareview' });

    // A two-column label bar (each label centered over its pane) with the change-nav + close floated
    // at the right, so "Original" / "Revised" line up with the split beneath them.
    const bar = h('div', { class: 'scr-compare-bar' });
    const origLabel = h('span', { class: 'scr-compare-label' });
    origLabel.textContent = data.originalName ? `Original - ${data.originalName}` : 'Original';
    const revLabel = h('span', { class: 'scr-compare-label' });
    revLabel.textContent = data.revisedName ? `Revised - ${data.revisedName}` : 'Revised';
    const controls = h('div', { class: 'scr-compare-controls' });
    const prev = h('button', {
      class: 'scr-compare-navbtn',
      type: 'button',
      title: 'Previous change',
    });
    prev.append(icon('prev', 14));
    const counter = h('span', { class: 'scr-compare-count' });
    const next = h('button', { class: 'scr-compare-navbtn', type: 'button', title: 'Next change' });
    next.append(icon('next', 14));
    controls.append(prev, counter, next);
    if (data.onClose) {
      const close = h('button', {
        class: 'scr-compare-navbtn scr-compare-close',
        type: 'button',
        title: 'Close side-by-side (back to the redline)',
      });
      close.append(icon('reject', 14));
      close.addEventListener('click', () => data.onClose?.());
      controls.append(close);
    }
    bar.append(origLabel, revLabel, controls);

    const split = h('div', { class: 'scr-compare-split' });
    const leftWell = h('div', { class: 'scr-compare-pane' });
    const rightWell = h('div', { class: 'scr-compare-pane' });
    split.append(leftWell, rightWell);
    element.append(bar, split);
    container.append(element);

    const [left, right] = await Promise.all([Viewer.create(leftWell), Viewer.create(rightWell)]);
    left.loadDocx(data.original);
    right.loadDocx(data.revised);

    const cv = new CompareView(element, left, right, data, counter);
    prev.addEventListener('click', () => cv.step(-1));
    next.addEventListener('click', () => cv.step(1));
    // The panes were laid out (and may have been mounted into a just-revealed split), so recompute
    // each view's paint window against the settled layout and start both panes at the top, aligned.
    requestAnimationFrame(() => {
      if (cv.disposed) return;
      left.view.refresh();
      right.view.refresh();
      cv.leftStage.scrollTop = 0;
      cv.rightStage.scrollTop = 0;
      cv.applyMarkers(); // band every change (none active yet)
    });
    return cv;
  }

  private constructor(
    element: HTMLElement,
    left: Viewer,
    right: Viewer,
    data: CompareViewData,
    counter: HTMLElement,
  ) {
    this.element = element;
    this.left = left;
    this.right = right;
    this.counter = counter;
    // The `.scr-stage` inside each viewer is that pane's own scroll container.
    this.leftStage = left.element.querySelector('.scr-stage') as HTMLElement;
    this.rightStage = right.element.querySelector('.scr-stage') as HTMLElement;

    this.anchors = data.alignment
      .filter((e) => (e.kind === 'equal' || e.kind === 'edited') && e.a != null && e.b != null)
      .map((e) => ({ a: e.a as number, b: e.b as number }));
    this.changes = data.alignment.filter((e) => e.kind !== 'equal');
    this.changeById = new Map(data.changes.filter((c) => c.id > 0).map((c) => [c.id, c]));

    this.counter.textContent = this.changes.length ? `0 / ${this.changes.length}` : 'no changes';
    this.leftStage.addEventListener('scroll', () => this.onScroll('left'), { passive: true });
    this.rightStage.addEventListener('scroll', () => this.onScroll('right'), { passive: true });
    // Any re-render (zoom, resize, reflow) can move paragraphs - rebuild the anchor map lazily.
    this.unsubs.push(
      left.view.addListener(() => {
        this.map = null;
      }),
      right.view.addListener(() => {
        this.map = null;
      }),
    );
  }

  /** Zoom both panes together - the side-by-side has one zoom, not one per pane. The scroll-lock
   *  anchors are computed live from on-screen positions, so they stay correct across a re-render. */
  setZoom(factor: number): void {
    this.left.view.setZoom(factor);
    this.right.view.setZoom(factor);
  }

  /** A user scroll on one pane syncs the other in the same frame (scroll events already fire at
   *  most once per frame); an echo of our own programmatic scroll is consumed instead.
   *
   *  During a drag the source pane moves on the compositor thread, so each event describes where
   *  it WAS one event-interval ago - mirrored as-is, the passive pane visibly trails. So sync with
   *  one interval of measured velocity as lead (pre-positioning the passive pane where the source
   *  will be when this frame paints), and settle to exact zero-lead alignment when events stop. */
  private onScroll(source: 'left' | 'right'): void {
    if (this.disposed) return;
    const stage = source === 'left' ? this.leftStage : this.rightStage;
    const exp = this.expected[source];
    if (exp != null) {
      this.expected[source] = null;
      if (Math.abs(stage.scrollTop - exp) < 2) return; // our own assignment echoing back
      // The value moved past what we set - a real user scroll landed meanwhile; fall through.
    }
    const now = performance.now();
    let lead = 0;
    if (this.lastEvent && this.lastEvent.pane === source && now - this.lastEvent.time < 80) {
      lead = stage.scrollTop - this.lastEvent.top;
      if (Math.abs(lead) > 400) lead = 0; // a scrollbar-track teleport, not a drag
    }
    this.lastEvent = { pane: source, top: stage.scrollTop, time: now };
    this.sync(source, lead);
    clearTimeout(this.settleTimer);
    this.settleTimer = window.setTimeout(() => {
      if (!this.disposed) this.sync(source, 0);
    }, 90);
  }

  /** Scroll-lock: map the source viewport top through the alignment **piecewise-linearly**, via a
   *  content-space anchor map cached once per layout and binary-searched per event - the handler
   *  costs microseconds, so the passive pane keeps up at any drag speed (the previous per-event
   *  geometry walk was itself slow enough to make frames late, which no extrapolation can cancel).
   *  Falls back to a proportional sync when no anchor has geometry. `lead` is extra source travel
   *  (px) to extrapolate against compositor lag. */
  private sync(source: 'left' | 'right', lead = 0): void {
    // Built lazily and cached: the map is invalidated (set to null) on relayout, not per event.
    if (!this.map) this.map = this.buildMap();
    const m = this.map;
    if (!m) {
      this.syncProportional(source);
      return;
    }
    const srcStage = source === 'left' ? this.leftStage : this.rightStage;
    const tgtStage = source === 'left' ? this.rightStage : this.leftStage;
    const src = source === 'left' ? m.l : m.r;
    const tgt = source === 'left' ? m.r : m.l;
    const pos = srcStage.scrollTop + lead; // the viewport top, in source content space

    // Binary search: the last anchor at or above `pos`.
    let lo = 0;
    let hi = src.length - 1;
    let i = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (src[mid]! <= pos) {
        i = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }

    let target: number;
    if (i < 0) {
      target = tgt[0]! - (src[0]! - pos); // before the first anchor: 1:1
    } else if (i >= src.length - 1 || src[i + 1]! <= src[i]!) {
      target = tgt[i]! + (pos - src[i]!); // past the last anchor (or degenerate span): 1:1
    } else {
      const t = (pos - src[i]!) / (src[i + 1]! - src[i]!);
      target = tgt[i]! + t * (tgt[i + 1]! - tgt[i]!);
    }
    if (Math.abs(target - tgtStage.scrollTop) < 1) return;
    this.assign(tgtStage, source === 'left' ? 'right' : 'left', target);
  }

  /** Every usable anchor's paragraph top in each pane's **content space** (clientY - stage top +
   *  scrollTop, which is invariant under scrolling): the viewport top of a pane is then simply its
   *  scrollTop. Rebuilt lazily after any re-render (zoom / resize / reflow invalidate it). */
  private buildMap(): { l: number[]; r: number[] } | null {
    if (this.anchors.length === 0) return null;
    const lRect = this.leftStage.getBoundingClientRect().top;
    const rRect = this.rightStage.getBoundingClientRect().top;
    const lScroll = this.leftStage.scrollTop;
    const rScroll = this.rightStage.scrollTop;
    const l: number[] = [];
    const r: number[] = [];
    for (const a of this.anchors) {
      const ly = this.left.view.paragraphClientTop(a.a);
      const ry = this.right.view.paragraphClientTop(a.b);
      if (ly == null || ry == null) continue;
      l.push(ly - lRect + lScroll);
      r.push(ry - rRect + rScroll);
    }
    return l.length ? { l, r } : null;
  }

  /** Proportional fallback: lock scroll fraction (used when there is no usable alignment). */
  private syncProportional(source: 'left' | 'right'): void {
    const srcStage = source === 'left' ? this.leftStage : this.rightStage;
    const tgtStage = source === 'left' ? this.rightStage : this.leftStage;
    const srcMax = srcStage.scrollHeight - srcStage.clientHeight;
    const tgtMax = tgtStage.scrollHeight - tgtStage.clientHeight;
    if (srcMax <= 0 || tgtMax <= 0) return;
    const frac = srcStage.scrollTop / srcMax;
    this.assign(tgtStage, source === 'left' ? 'right' : 'left', frac * tgtMax);
  }

  /** Programmatic scroll of a pane: record the value actually applied (post-clamp) so the async
   *  scroll event it produces is recognized as an echo and consumed. */
  private assign(stage: HTMLElement, pane: 'left' | 'right', top: number): void {
    stage.scrollTop = top;
    this.expected[pane] = stage.scrollTop;
  }

  /** Step to the next (`+1`) / previous (`-1`) change, scrolling both panes to it. */
  step(dir: 1 | -1): void {
    if (this.changes.length === 0) return;
    this.navIndex = (this.navIndex + dir + this.changes.length) % this.changes.length;
    this.gotoChange(this.navIndex);
  }

  /** Scroll both panes to the change with the given redline revision id (a reviewing-pane row click,
   *  routed here while the split is shown since the redline view is hidden). Maps the id to its
   *  original-side paragraph, scrolls the left pane there, and locks the right pane to it. */
  revealByRevisionId(id: number): void {
    const change = this.changeById.get(id);
    if (!change) return;
    this.left.view.scrollToParagraph(change.para, 'center');
    this.expected.left = this.leftStage.scrollTop; // instant scroll - consume its echo
    requestAnimationFrame(() => this.sync('left'));
    // Reflect the nav counter + emphasize this change if it's a step target.
    const idx = this.changes.findIndex((e) => e.a === change.para);
    if (idx >= 0) {
      this.navIndex = idx;
      this.counter.textContent = `${idx + 1} / ${this.changes.length}`;
      this.applyMarkers(this.changes[idx]);
    }
  }

  private gotoChange(i: number): void {
    const e = this.changes[i];
    if (!e) return;
    // Center whichever side has the paragraph; then align the other side to it.
    if (e.a != null) {
      this.left.view.scrollToParagraph(e.a, 'center');
      this.expected.left = this.leftStage.scrollTop; // instant scroll - consume its echo
    }
    if (e.b != null) {
      this.right.view.scrollToParagraph(e.b, 'center');
      this.expected.right = this.rightStage.scrollTop;
    }
    // The centered side drives; align its counterpart. (Both-present entries are already aligned by
    // centering each on its own paragraph.)
    if (e.a == null) requestAnimationFrame(() => this.sync('right'));
    else if (e.b == null) requestAnimationFrame(() => this.sync('left'));
    this.counter.textContent = `${i + 1} / ${this.changes.length}`;
    this.applyMarkers(e);
  }

  /** Paint every change as a coloured band in each pane (the active one emphasized): deletions on the
   *  original, insertions on the revised, edited paragraphs on both. */
  applyMarkers(active: AlignEntry | null = null): void {
    const left: ChangeMarker[] = [];
    const right: ChangeMarker[] = [];
    for (const e of this.changes) {
      const isActive = e === active;
      if (e.a != null) {
        left.push({ para: e.a, kind: e.kind === 'delete' ? 'delete' : 'edited', active: isActive });
      }
      if (e.b != null) {
        right.push({
          para: e.b,
          kind: e.kind === 'insert' ? 'insert' : 'edited',
          active: isActive,
        });
      }
    }
    this.left.view.setChangeMarkers(left);
    this.right.view.setChangeMarkers(right);
  }

  /** Tear down both viewers and remove the element. */
  destroy(): void {
    this.disposed = true;
    clearTimeout(this.settleTimer);
    for (const unsub of this.unsubs) unsub();
    this.left.destroy();
    this.right.destroy();
    this.element.remove();
  }
}
