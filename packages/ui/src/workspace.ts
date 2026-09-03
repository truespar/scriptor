import {
  type CompareAnnotation,
  type CompareResult,
  compareAnnotationsById,
  type ScriptorOptions,
  ScriptorView,
} from '@truespar/scriptor-core';
import type { CompareVersion } from './compare-dialog';
import { CompareView, type CompareViewData } from './compare-view';
import { h, injectStyles } from './dom';
import { ReviewingPane } from './reviewing-pane';
import { inferUnits, Ribbon, type Units } from './ribbon';
import { Rulers } from './rulers';
import { StatusBar } from './status-bar';

export interface WorkspaceOptions extends ScriptorOptions {
  /** Initial measurement units (US inch / EU mm). Default `'us'`. */
  units?: Units;
  /** Show the ribbon's Quick-Access "Save" button. Default `true`. Set `false`
   *  for a continuously-persisted host (live collaboration), where a manual Save
   *  is misleading. */
  showSave?: boolean;
  /** Optional review model: after a comparison (Review → Compare), called with the redline + manifest
   *  to produce the semantic overlay (materiality / category / summary / risks per change). The
   *  reviewing pane then shows materiality badges, risk flags, a summary bar, and a Substantive-only
   *  filter. The redline is never altered - annotations are pure judgment. Omit for a plain
   *  (deterministic) change list. */
  compareAnnotate?: (result: CompareResult) => CompareAnnotation[] | Promise<CompareAnnotation[]>;
  /** Optional source of named documents (e.g. saved versions) the Compare dialog offers for either
   *  side, in addition to the current document and a file - the version-aware compare path. */
  compareVersions?: () => CompareVersion[] | Promise<CompareVersion[]>;
}

/**
 * The batteries-included Word-style editor: a ribbon on top, the page framed by horizontal +
 * vertical rulers in a scrollable stage, and a status bar below - all bound to one [`ScriptorView`].
 * Apps that want the full chrome use this; apps that want their own UI use `ScriptorView` directly.
 *
 *   const ws = await Workspace.create(document.getElementById('app'), { mode: 'edit' });
 *   ws.loadDocx(bytes);
 */
export class Workspace {
  readonly element: HTMLElement;
  readonly view: ScriptorView;
  readonly ribbon: Ribbon;
  readonly rulers: Rulers;
  readonly statusBar: StatusBar;
  readonly reviewingPane: ReviewingPane;
  /** Drop any active comparison (exit side-by-side, disable its toggle, clear the pane overlay) - run
   *  before loading a *different* document so a stale comparison can't linger. Assigned in `create`. */
  private resetCompareContext: () => void = () => {};

  static async create(container: HTMLElement, options: WorkspaceOptions = {}): Promise<Workspace> {
    injectStyles();
    const { units: unitsOpt, showSave, compareAnnotate, compareVersions, ...viewOptions } = options;
    // Pages render as per-page frames on a transparent gutter (the view's default), so the gaps show
    // the page-stage backdrop (`--scr-bg-stage`) through automatically - no gutter-color sync needed.

    const root = h('div', { class: 'scr-workspace' });
    const ribbonHost = h('div');
    // The page area (rulers + scrolling stage) and the docked reviewing pane sit side by side in a
    // body row; the status bar spans below.
    const body = h('div', { class: 'scr-body' });
    const main = h('div', { class: 'scr-main' });
    // Fixed top band: the horizontal ruler stays put above the scrolling pages (Word-style), so it
    // applies to every page and never covers content. The vertical ruler scrolls with the pages.
    const top = h('div', { class: 'scr-top' });
    const topRow = h('div', { class: 'scr-top-row' });
    top.append(topRow);
    const stage = h('div', { class: 'scr-stage' });
    const inner = h('div', { class: 'scr-stage-inner' });
    const prow = h('div', { class: 'scr-prow' });
    const pageHost = h('div', { class: 'scr-page-host' });
    inner.append(prow);
    stage.append(inner);
    main.append(top, stage);
    body.append(main);
    root.append(ribbonHost, body);
    container.append(root);

    // The view mounts into the page host (inside the ruler grid).
    const view = await ScriptorView.create(pageHost, viewOptions);

    // Default the unit to the document's paper size (A4 -> mm) unless the host forces one; the ribbon
    // then keeps it in sync as documents are opened at runtime.
    const units = unitsOpt ?? inferUnits(view);

    const rulers = new Rulers(view, units);
    topRow.append(rulers.corner, rulers.hCanvas);
    prow.append(rulers.vCanvas, pageHost);

    // Keep the fixed horizontal-ruler band aligned with the page. The page area lives in the
    // scrolling stage and is centered by `.scr-stage-inner { margin: 0 auto }`, whose width the
    // stage's vertical scrollbar narrows - so the page sits a few px off the fixed top band, which
    // has no scrollbar. Measuring the actual gap between the vertical ruler (page row) and the corner
    // (band) captures centering + scrollbar + horizontal scroll in one number, unlike a plain
    // `-scrollLeft` (which only handled scroll and left the initial centering offset). rAF-coalesced.
    let alignScheduled = false;
    const alignRuler = (): void => {
      if (alignScheduled) return;
      alignScheduled = true;
      requestAnimationFrame(() => {
        alignScheduled = false;
        topRow.style.transform = 'none'; // reset so the corner's rect is untransformed
        const dx =
          rulers.vCanvas.getBoundingClientRect().left - rulers.corner.getBoundingClientRect().left;
        topRow.style.transform = `translateX(${dx}px)`;
      });
    };
    stage.addEventListener('scroll', alignRuler);
    window.addEventListener('resize', alignRuler);
    // Re-align whenever the view re-renders (page size / zoom / document change can move the page).
    view.addListener(alignRuler);
    alignRuler(); // initial

    // Reviewing pane: docked on the right of the body row, hidden until toggled. Visibility goes
    // through setVisible so a hidden pane skips per-keystroke rebuilds and catches up on show.
    const reviewingPane = new ReviewingPane(body, view, {
      onClose: () => {
        reviewingPane.setVisible(false);
      },
    });
    reviewingPane.setVisible(false);

    // Side-by-side state: the last comparison's two source documents + its alignment, and the mounted
    // `CompareView` (over the editor) when active. Managed as closures here since the handlers are
    // wired into the ribbon before the `Workspace` instance exists.
    let compareData: CompareViewData | null = null;
    let compareView: CompareView | null = null;
    let sideBySideOn = false;
    const applySideBySide = async (on: boolean): Promise<void> => {
      sideBySideOn = on && compareData != null;
      if (sideBySideOn && compareData) {
        top.style.display = 'none';
        stage.style.display = 'none';
        compareView?.destroy();
        compareView = await CompareView.create(main, {
          ...compareData,
          onClose: () => void applySideBySide(false), // in-view exit back to the redline
        });
        // The status bar's zoom drives the (hidden) editor view; mirror it into both panes now and
        // on every subsequent change (the view listener below), so one zoom control rules the split.
        compareView.setZoom(view.zoomLevel);
        // The redline view is hidden; route reviewing-pane clicks to the split instead.
        reviewingPane.setRevealHandler((item) => {
          compareView?.revealByRevisionId(item.id);
          return true;
        });
      } else {
        compareView?.destroy();
        compareView = null;
        reviewingPane.setRevealHandler(null);
        top.style.display = '';
        stage.style.display = '';
      }
      ribbon.setSideBySide(compareData != null, sideBySideOn);
    };
    // Status-bar zoom lands on the editor view; when the split is up, echo it into both panes
    // (setZoom no-ops on an unchanged factor, so this is free on ordinary re-renders).
    view.addListener(() => compareView?.setZoom(view.zoomLevel));

    const ribbon = new Ribbon(ribbonHost, view, {
      units,
      showSave,
      onUnits: (u) => rulers.setUnits(u),
      onReviewingPane: () => {
        reviewingPane.setVisible(!reviewingPane.visible);
      },
      // Switch the compared documents into (or out of) the side-by-side review view.
      onSideBySide: () => {
        void applySideBySide(!sideBySideOn);
      },
      // Open the redline in this workspace's view and reveal the reviewing pane (the redline's tracked
      // changes populate it). The result view is the editor in review mode - no separate viewer. When
      // a review model is wired, annotate the changes (materiality / risk) and attach the overlay.
      compareVersions,
      onCompare: (result, sources) => {
        // Retain the two source documents + alignment for the side-by-side view, and enable its toggle.
        compareData = {
          original: sources.original,
          revised: sources.revised,
          originalName: sources.originalName,
          revisedName: sources.revisedName,
          alignment: result.alignment,
          changes: result.changes,
        };

        // The redline is the editable working copy (hidden behind the split); load it so the reviewing
        // pane + accept/reject act on it, but SHOW the side-by-side by default - a comparison's first
        // job is to let you see what changed.
        view.loadDocx(result.redline);
        reviewingPane.setAnnotations(null);
        // Scope the pane to the comparison's own delta: the redline sits on top of the original's
        // pre-existing tracked changes, and without this those would bury the changes just found.
        reviewingPane.setCompareScope(result.changes.map((c) => c.id));
        reviewingPane.setVisible(true);
        if (compareAnnotate) {
          void Promise.resolve(compareAnnotate(result))
            .then((anns) => reviewingPane.setAnnotations(compareAnnotationsById(result, anns)))
            .catch(() => {
              /* a failed annotation pass just leaves the plain (deterministic) change list */
            });
        }
        void applySideBySide(true); // open side-by-side straight after comparing
      },
    });
    const statusBar = new StatusBar(view);
    root.append(statusBar.element);

    // The view's first render happened before rulers/status subscribed - prime them once.
    rulers.refresh();
    statusBar.refresh();

    const ws = new Workspace(root, view, ribbon, rulers, statusBar, reviewingPane);
    // Exit + forget any comparison: closes side-by-side, disables its toggle, clears the pane overlay.
    ws.resetCompareContext = () => {
      if (sideBySideOn) void applySideBySide(false);
      compareData = null;
      ribbon.setSideBySide(false, false);
      reviewingPane.setCompareScope(null);
      reviewingPane.setAnnotations(null);
    };
    return ws;
  }

  /** Load a `.docx` (raw OPC zip bytes) and render it. Use this rather than `view.loadDocx` so opening
   *  a *different* document tears down any active comparison (the side-by-side view + its toggle refer
   *  to the previously compared documents). */
  loadDocx(bytes: Uint8Array): void {
    this.resetCompareContext();
    this.view.loadDocx(bytes);
  }

  /** Replace the content with a fresh empty document (tears down any active comparison). */
  newDocument(): void {
    this.resetCompareContext();
    this.view.newDocument();
  }

  private constructor(
    element: HTMLElement,
    view: ScriptorView,
    ribbon: Ribbon,
    rulers: Rulers,
    statusBar: StatusBar,
    reviewingPane: ReviewingPane,
  ) {
    this.element = element;
    this.view = view;
    this.ribbon = ribbon;
    this.rulers = rulers;
    this.statusBar = statusBar;
    this.reviewingPane = reviewingPane;
  }

  /** Show / hide the docked reviewing pane. */
  setReviewingPaneVisible(visible: boolean): void {
    this.reviewingPane.element.style.display = visible ? '' : 'none';
  }
  /** Toggle the docked reviewing pane. */
  toggleReviewingPane(): void {
    const el = this.reviewingPane.element;
    el.style.display = el.style.display === 'none' ? '' : 'none';
  }

  /** Tear down the whole editor + chrome. */
  destroy(): void {
    this.ribbon.destroy();
    this.rulers.destroy();
    this.statusBar.destroy();
    this.reviewingPane.destroy();
    this.view.destroy();
    this.element.remove();
  }
}
