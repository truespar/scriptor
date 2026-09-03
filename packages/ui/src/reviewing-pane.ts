import type { CompareAnnotation, ReviewItem, ScriptorView } from '@truespar/scriptor-core';
import { h, injectStyles } from './dom';
import { type IconName, icon } from './icons';

export interface ReviewingPaneOptions {
  /** Called when the user clicks the pane's close button (the host can hide / unmount it). When
   *  omitted, no close button is shown. */
  onClose?: () => void;
}

/** Per-kind label + badge colours (text, background) for a review row. */
const KIND_META: Record<ReviewItem['kind'], { label: string; fg: string; bg: string }> = {
  ins: { label: 'Insertion', fg: '#1d4ed8', bg: '#dbe7ff' },
  del: { label: 'Deletion', fg: '#b42318', bg: '#fde2dd' },
  fmt: { label: 'Format', fg: '#6d28d9', bg: '#ece4fd' },
  movefrom: { label: 'Moved out', fg: '#1b6e2f', bg: '#d7f0dd' },
  moveto: { label: 'Moved in', fg: '#1b6e2f', bg: '#d7f0dd' },
  rowins: { label: 'Row added', fg: '#1d4ed8', bg: '#dbe7ff' },
  rowdel: { label: 'Row deleted', fg: '#b42318', bg: '#fde2dd' },
  colins: { label: 'Column added', fg: '#1d4ed8', bg: '#dbe7ff' },
  coldel: { label: 'Column deleted', fg: '#b42318', bg: '#fde2dd' },
  tblprop: { label: 'Table format', fg: '#6d28d9', bg: '#ece4fd' },
  rowprop: { label: 'Row format', fg: '#6d28d9', bg: '#ece4fd' },
  cellprop: { label: 'Cell format', fg: '#6d28d9', bg: '#ece4fd' },
  comment: { label: 'Comment', fg: '#92560b', bg: '#fdedd3' },
};

function formatDate(iso: string): string {
  if (!iso) return '';
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? iso
    : d.toLocaleString(undefined, {
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      });
}

/**
 * A scrollable side list of every tracked change + top-level comment in the document, bound to a
 * [`ScriptorView`]. Clicking a row selects + scrolls to it (and opens a comment's thread); each row
 * carries inline actions (Accept / Reject for changes; Resolve / Delete for comments). It reflects
 * the view's state via the view's change listener, so resolving anything anywhere refreshes the list.
 *
 * Standalone like the ribbon / status bar: mount it wherever (the [`Workspace`] docks it on the right).
 */
export class ReviewingPane {
  readonly element: HTMLElement;
  private readonly view: ScriptorView;
  private readonly list: HTMLElement;
  private readonly legend: HTMLElement;
  private readonly countEl: HTMLElement;
  private readonly summaryEl: HTMLElement;
  private readonly unsub: () => void;
  /** A view change arrived while the pane was hidden - rebuild on the next show. */
  private stale = false;
  /** The semantic overlay: `revision id -> annotation` (set after a comparison the host has
   *  annotated). `null` when there is no overlay - the pane then behaves as a plain change list. */
  private annotations: Map<number, CompareAnnotation> | null = null;
  /** "Substantive only" filter - active only while an overlay is present. */
  private substantiveOnly = false;
  /** After a comparison, the revision ids the comparison itself produced (from the manifest). While
   *  set, the pane shows *only* these - the comparison's delta - not the document's own pre-existing
   *  tracked changes (an input already redlined by others would otherwise bury the one real diff). */
  private compareScope: Set<number> | null = null;
  /** The view's document epoch when [`compareScope`] was set; if the document is later swapped the
   *  scope no longer applies and is dropped (see [`refresh`]). */
  private scopeEpoch = -1;
  /** Escape hatch: show every change in the document, not just the comparison's delta. */
  private showAllChanges = false;
  /** When set, a row click is routed here instead of scrolling the bound view - used by the
   *  side-by-side comparison view (the redline view is hidden, so clicks must drive the split). Return
   *  `true` to consume the click; `false`/absent falls back to `view.revealReviewItem`. */
  private onReveal: ((item: ReviewItem) => boolean) | null = null;

  constructor(container: HTMLElement, view: ScriptorView, options: ReviewingPaneOptions = {}) {
    this.view = view;
    injectStyles();

    this.element = h('div', { class: 'scr-reviewing-pane' });
    const head = h('div', { class: 'scr-rp-head' });
    const title = h('span');
    title.textContent = 'Revisions';
    this.countEl = h('span', { class: 'scr-rp-count' });
    const titleWrap = h('span', { class: 'scr-rp-title' });
    titleWrap.append(title, this.countEl);
    head.append(titleWrap);
    if (options.onClose) {
      const close = h('button', { class: 'scr-rp-close', type: 'button', title: 'Close' });
      close.append(icon('reject', 16));
      close.addEventListener('click', () => options.onClose?.());
      head.append(close);
    }

    // A per-reviewer colour legend + "Show Markup by reviewer" toggles (R3).
    this.legend = h('div', { class: 'scr-rp-legend' });
    // The comparison overlay summary bar (materiality breakdown + a Substantive-only filter); hidden
    // until the host attaches a semantic overlay via `setAnnotations`.
    this.summaryEl = h('div', { class: 'scr-rp-summary' });
    this.summaryEl.style.display = 'none';
    this.list = h('div', { class: 'scr-rp-list' });
    // A drag handle on the pane's left edge (it docks on the right): drag left to widen, right to
    // narrow. Clamped to a sensible range; overrides the default flex-basis.
    const handle = h('div', { class: 'scr-rp-resize', title: 'Drag to resize' });
    this.element.append(handle, head, this.legend, this.summaryEl, this.list);
    container.append(this.element);
    this.setupResize(handle);

    this.unsub = view.addListener(() => this.refresh());
    this.refresh();
  }

  /** Wire the left-edge handle to resize the pane (pointer-captured drag, clamped 220-640px). */
  private setupResize(handle: HTMLElement): void {
    let startX = 0;
    let startW = 0;
    const onMove = (e: PointerEvent): void => {
      const w = Math.min(640, Math.max(220, startW + (startX - e.clientX)));
      this.element.style.flexBasis = `${w}px`;
    };
    const end = (e: PointerEvent): void => {
      handle.classList.remove('dragging');
      handle.removeEventListener('pointermove', onMove);
      try {
        handle.releasePointerCapture(e.pointerId);
      } catch {
        // capture may already be released
      }
    };
    handle.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      startX = e.clientX;
      startW = this.element.getBoundingClientRect().width;
      handle.classList.add('dragging');
      handle.setPointerCapture(e.pointerId);
      handle.addEventListener('pointermove', onMove);
    });
    handle.addEventListener('pointerup', end);
    handle.addEventListener('pointercancel', end);
  }

  /** Tear down: unsubscribe + remove the DOM. */
  destroy(): void {
    this.unsub();
    this.element.remove();
  }

  /** Show / hide the pane. Hosts must use this (not `style.display` directly) so a pane that went
   *  stale while hidden rebuilds on show - and a hidden pane skips per-keystroke rebuilds. */
  setVisible(visible: boolean): void {
    this.element.style.display = visible ? '' : 'none';
    if (visible && this.stale) this.refresh();
  }

  /** Attach (or clear, with `null`) the semantic overlay: a `revision id -> annotation` map
   *  (build it with `compareAnnotationsById`). Each matching change row then shows a materiality badge
   *  + risk flags, a summary bar appears (counts + a Substantive-only filter), and the pane becomes a
   *  "what changed and how much it matters" review surface. Pass `null` on loading a fresh document so
   *  a prior comparison's overlay does not linger. */
  setAnnotations(byRevisionId: Map<number, CompareAnnotation> | null): void {
    this.annotations = byRevisionId && byRevisionId.size ? byRevisionId : null;
    if (!this.annotations) this.substantiveOnly = false;
    this.stale = true;
    if (this.visible) this.refresh();
  }

  /** Route row clicks to `handler` instead of scrolling the bound view (return `true` to consume). The
   *  side-by-side view sets this so a change click scrolls the split, not the hidden redline. `null`
   *  restores the default reveal-in-view behavior. */
  setRevealHandler(handler: ((item: ReviewItem) => boolean) | null): void {
    this.onReveal = handler;
  }

  /** Scope the pane to just a comparison's own changes (the revision ids in its manifest), so a
   *  document that other reviewers already redlined doesn't bury the one diff this comparison found.
   *  Pass `null` to clear. Automatically dropped once a different document is loaded. */
  setCompareScope(ids: Iterable<number> | null): void {
    const set = ids ? new Set(ids) : null;
    this.compareScope = set?.size ? set : null;
    this.scopeEpoch = this.view.documentEpoch();
    this.showAllChanges = false;
    this.stale = true;
    if (this.visible) this.refresh();
  }

  /** Whether the pane is currently shown (its element not display-none). */
  get visible(): boolean {
    return this.element.style.display !== 'none';
  }

  private refresh(): void {
    // The view notifies on every render (each keystroke). Rebuilding ~50 DOM rows + measuring
    // their overflow while the pane isn't even visible taxed typing latency for nothing - defer.
    if (!this.visible) {
      this.stale = true;
      return;
    }
    this.stale = false;
    this.renderLegend();
    // A comparison scope only applies to the document it was computed on; drop it once swapped.
    if (this.compareScope && this.view.documentEpoch() !== this.scopeEpoch) {
      this.compareScope = null;
      this.showAllChanges = false;
    }
    const all = this.view.reviewItems();
    // Comparison delta: only the changes this comparison authored, unless the user asked for all.
    const scopeActive = this.compareScope != null && !this.showAllChanges;
    const scoped = scopeActive ? all.filter((it) => this.compareScope?.has(it.id)) : all;
    this.renderSummary(all, scoped.length);
    const items =
      this.substantiveOnly && this.annotations
        ? scoped.filter((it) => this.annotations?.get(it.id)?.materiality === 'substantive')
        : scoped;
    this.countEl.textContent =
      items.length === all.length ? String(all.length) : `${items.length}/${all.length}`;
    this.list.replaceChildren();
    if (!items.length) {
      const empty = h('div', { class: 'scr-rp-empty' });
      empty.textContent = this.substantiveOnly
        ? 'No substantive changes.'
        : scopeActive
          ? 'This comparison found no changes.'
          : 'No changes or comments.';
      this.list.append(empty);
      return;
    }
    for (const item of items) this.list.append(this.renderRow(item));
    this.addOverflowToggles();
  }

  /** The comparison summary bar. Two independent parts, shown when relevant:
   *  - a **comparison-scope** line (this comparison's change count + a "Show all" toggle), whenever a
   *    comparison scope is attached - so a document already redlined by others doesn't hide the delta;
   *  - the **semantic overlay** line (substantive / risk counts + a "Substantive only" filter), when a
   *    host has annotated the changes.
   *  `all` is every review item; `scopedCount` is how many survive the comparison scope. */
  private renderSummary(all: ReviewItem[], scopedCount: number): void {
    if (!this.annotations && !this.compareScope) {
      this.summaryEl.style.display = 'none';
      return;
    }
    this.summaryEl.replaceChildren();
    this.summaryEl.style.display = '';

    if (this.compareScope) {
      const row = h('div', { class: 'scr-rp-sum-row' });
      const label = h('span', { class: 'scr-rp-sum-counts' });
      const n = this.showAllChanges ? scopedCount : this.compareScope.size;
      label.textContent = `${n} change${n === 1 ? '' : 's'} from this comparison`;
      const filter = h('label', { class: 'scr-rp-sum-filter' });
      const cb = document.createElement('input');
      cb.type = 'checkbox';
      // "Comparison only" is on by default; unchecking reveals the document's pre-existing changes.
      cb.checked = !this.showAllChanges;
      cb.addEventListener('change', () => {
        this.showAllChanges = !cb.checked;
        this.refresh();
      });
      const flabel = document.createElement('span');
      flabel.textContent = `Comparison only (${all.length} total)`;
      filter.append(cb, flabel);
      row.append(label, filter);
      this.summaryEl.append(row);
    }

    if (this.annotations) {
      const scopedItems =
        this.compareScope && !this.showAllChanges
          ? all.filter((it) => this.compareScope?.has(it.id))
          : all;
      const anns = scopedItems
        .map((it) => this.annotations?.get(it.id))
        .filter((a): a is CompareAnnotation => a != null);
      const substantive = anns.filter((a) => a.materiality === 'substantive').length;
      const risks = anns.reduce((n, a) => n + (a.risks?.length ?? 0), 0);

      const row = h('div', { class: 'scr-rp-sum-row' });
      const counts = h('span', { class: 'scr-rp-sum-counts' });
      counts.textContent = `${substantive} substantive · ${risks} risk${risks === 1 ? '' : 's'}`;
      const filter = h('label', { class: 'scr-rp-sum-filter' });
      const cb = document.createElement('input');
      cb.type = 'checkbox';
      cb.checked = this.substantiveOnly;
      cb.addEventListener('change', () => {
        this.substantiveOnly = cb.checked;
        this.refresh();
      });
      const flabel = document.createElement('span');
      flabel.textContent = 'Substantive only';
      filter.append(cb, flabel);
      row.append(counts, filter);
      this.summaryEl.append(row);
    }
  }

  /** Row text is collapsed to a 3-line preview; a long comment or change must still be fully
   *  readable, so rows whose text overflows the clamp get a "Show more" / "Show less" toggle.
   *  Measured after the rows are in the DOM (`scrollHeight` needs layout) - all reads before any
   *  writes, so the pass forces one reflow instead of one per row. */
  private addOverflowToggles(): void {
    const overflowing: HTMLElement[] = [];
    for (const text of this.list.querySelectorAll<HTMLElement>('.scr-rp-text')) {
      if (text.scrollHeight > text.clientHeight + 1) overflowing.push(text);
    }
    for (const text of overflowing) {
      const more = h('button', { class: 'scr-rp-more', type: 'button' });
      more.textContent = 'Show more';
      more.addEventListener('click', (e) => {
        e.stopPropagation(); // don't also trigger the row's reveal
        const open = text.classList.toggle('scr-rp-text-open');
        more.textContent = open ? 'Show less' : 'Show more';
      });
      text.after(more);
    }
  }

  /** The per-reviewer colour legend + "show markup" toggles. Hidden when there are no reviewers. */
  private renderLegend(): void {
    const reviewers = this.view.reviewers();
    this.legend.replaceChildren();
    this.legend.style.display = reviewers.length ? '' : 'none';
    for (const r of reviewers) {
      const row = h('label', { class: 'scr-rp-rev' + (r.visible ? '' : ' scr-rp-rev-off') });
      const cb = document.createElement('input');
      cb.type = 'checkbox';
      cb.checked = r.visible;
      cb.addEventListener('change', () => this.view.setReviewerVisible(r.name, cb.checked));
      const swatch = h('span', { class: 'scr-rp-swatch' });
      swatch.style.background = r.color;
      const name = h('span', { class: 'scr-rp-rev-name' });
      name.textContent = r.name;
      row.append(cb, swatch, name);
      this.legend.append(row);
    }
  }

  private renderRow(item: ReviewItem): HTMLElement {
    const meta = KIND_META[item.kind];
    const ann = this.annotations?.get(item.id) ?? null;
    const row = h('div', { class: 'scr-rp-item' + (item.resolved ? ' scr-rp-resolved' : '') });
    row.addEventListener('click', () => {
      if (!this.onReveal?.(item)) this.view.revealReviewItem(item);
    });

    const headRow = h('div', { class: 'scr-rp-head-row' });
    const badge = h('span', { class: 'scr-rp-kind' });
    badge.textContent = item.resolved ? `${meta.label} ✓` : meta.label;
    badge.style.color = meta.fg;
    badge.style.background = meta.bg;
    const author = h('span', { class: 'scr-rp-author' });
    author.textContent = item.author || 'Unknown';
    const date = h('span', { class: 'scr-rp-date' });
    date.textContent = formatDate(item.date);
    headRow.append(badge, author, date);
    // Materiality badge (from the semantic overlay), with the category as a tooltip.
    if (ann) {
      const sub = ann.materiality === 'substantive';
      const mat = h('span', { class: 'scr-rp-mat' + (sub ? ' scr-rp-mat-sub' : '') });
      mat.textContent = ann.category || (sub ? 'Substantive' : 'Trivial');
      mat.title = sub ? 'Substantive change' : 'Trivial change';
      headRow.append(mat);
    }

    const text = h('div', { class: 'scr-rp-text' });
    const formatKinds = ['fmt', 'tblprop', 'rowprop', 'cellprop'];
    text.textContent = item.text || (formatKinds.includes(item.kind) ? '(formatting)' : '');

    // The overlay's natural-language summary + risk flags, when annotated.
    const overlay: HTMLElement[] = [];
    if (ann?.summary) {
      const s = h('div', { class: 'scr-rp-ann-summary' });
      s.textContent = ann.summary;
      overlay.push(s);
    }
    for (const risk of ann?.risks ?? []) {
      const r = h('div', { class: 'scr-rp-risk' });
      r.append(icon('trackChanges', 12), document.createTextNode(risk));
      overlay.push(r);
    }

    const actions = h('div', { class: 'scr-rp-actions' });
    if (item.kind === 'comment') {
      actions.append(
        this.action(item.resolved ? 'Reopen' : 'Resolve', 'accept', false, () =>
          this.view.resolveReviewItem(item),
        ),
        this.action('Delete', 'trash', true, () => this.view.deleteReviewItem(item)),
      );
    } else {
      actions.append(
        this.action('Accept', 'accept', false, () => this.view.acceptReviewItem(item)),
        this.action('Reject', 'reject', false, () => this.view.rejectReviewItem(item)),
      );
    }

    row.append(headRow, text, ...overlay, actions);
    return row;
  }

  private action(label: string, name: IconName, danger: boolean, onClick: () => void): HTMLElement {
    const b = h('button', {
      class: 'scr-rp-btn' + (danger ? ' scr-rp-danger' : ''),
      type: 'button',
    });
    b.append(icon(name, 14), document.createTextNode(label));
    b.addEventListener('click', (e) => {
      e.stopPropagation(); // don't also trigger the row's reveal
      onClick();
    });
    return b;
  }
}
