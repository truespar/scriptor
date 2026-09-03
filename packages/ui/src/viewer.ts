import { ScriptorView, type ScriptorOptions } from '@truespar/scriptor-core';
import { h, injectStyles } from './dom';

/** Configuration for a [`Viewer`]. Same surface as [`ScriptorOptions`]; `mode`
 *  defaults to `'read'` and editing options have no effect (a viewer never
 *  edits). Selection stays on by default so users can copy text. */
export type ViewerOptions = ScriptorOptions;

/**
 * A read-only document viewer: the same scrolling page stage as [`Workspace`],
 * bound to one [`ScriptorView`], but with NO ribbon, rulers, status bar, or
 * reviewing pane - just the rendered pages. Selection (for copy) stays on.
 *
 * Use this to *preview* a document; use `Workspace` for the full editor chrome;
 * use `ScriptorView` directly for a bespoke UI.
 *
 *   const v = await Viewer.create(document.getElementById('preview'));
 *   v.loadDocx(bytes);
 */
export class Viewer {
  readonly element: HTMLElement;
  readonly view: ScriptorView;

  static async create(container: HTMLElement, options: ViewerOptions = {}): Promise<Viewer> {
    injectStyles();

    const root = h('div', { class: 'scr-viewer' });
    const stage = h('div', { class: 'scr-stage' });
    const inner = h('div', { class: 'scr-stage-inner' });
    const pageHost = h('div', { class: 'scr-page-host' });
    inner.append(pageHost);
    stage.append(inner);
    root.append(stage);
    container.append(root);

    // Read mode: render + select, never a caret / keyboard / edits.
    const view = await ScriptorView.create(pageHost, { ...options, mode: options.mode ?? 'read' });

    return new Viewer(root, view);
  }

  private constructor(element: HTMLElement, view: ScriptorView) {
    this.element = element;
    this.view = view;
  }

  /** Load a `.docx` (raw OPC zip bytes) and render it. */
  loadDocx(bytes: Uint8Array): void {
    this.view.loadDocx(bytes);
  }

  /** Load a collaboration join snapshot (the authoritative server state). */
  loadSnapshot(bytes: Uint8Array): void {
    this.view.loadSnapshot(bytes);
  }

  /** Tear down the view and remove the element from the DOM. */
  destroy(): void {
    this.view.destroy();
    this.element.remove();
  }
}
