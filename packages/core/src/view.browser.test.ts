import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { ScriptorView } from './view';

// A smoke suite, not a unit suite. It drives the real editor in real Chromium: real WebAssembly, a
// real canvas, real fonts. The point is to catch the class of breakage a compiler cannot - the view
// mounting but never painting, an edit not reaching the engine, a controller that was refactored
// out and quietly stopped being wired up.
//
// Everything goes through the public API, so the suite stays honest about what a host can do.

let container: HTMLDivElement;
let view: ScriptorView;

/**
 * Wait for the view to repaint.
 *
 * Rendering is scheduled on an animation frame and the document's derived numbers - word count,
 * page count, paragraph text - are read off the layout that pass produces. So a query taken in the
 * same tick as an edit still sees the state before it. Two frames, because the first is the one the
 * edit scheduled and the second guarantees it has been through.
 */
const painted = () =>
  new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())));

beforeEach(async () => {
  container = document.createElement('div');
  // Layout needs a real box: a zero-size container paginates to nothing.
  container.style.width = '900px';
  container.style.height = '700px';
  document.body.append(container);
  view = await ScriptorView.create(container, { mode: 'edit' });
  await painted();
});

afterEach(() => {
  view.destroy();
  container.remove();
});

describe('mounting', () => {
  it('renders a canvas into the container', () => {
    const canvas = container.querySelector('canvas');
    expect(canvas).not.toBeNull();
    expect((canvas as HTMLCanvasElement).width).toBeGreaterThan(0);
  });

  it('starts in the mode it was asked for, on one page', () => {
    expect(view.mode).toBe('edit');
    expect(view.pageCount()).toBeGreaterThanOrEqual(1);
  });

  it('has no caret until a document exists, then places one at the start', async () => {
    // A freshly mounted view is showing nothing, so there is nowhere for a caret to be. Opening or
    // creating a document is what gives it one - worth pinning, because `replaceSelection` and the
    // other caret-relative commands are no-ops until then.
    expect(view.selection).toBeNull();
    view.newDocument();
    await painted();
    expect(view.selection?.focus).toEqual({ para: 0, off: 0 });
  });

  it('paints pixels rather than leaving the canvas blank', () => {
    // The strongest cheap assertion that the engine actually ran: an all-transparent canvas means
    // layout or paint silently did nothing.
    const canvas = container.querySelector('canvas') as HTMLCanvasElement;
    const ctx = canvas.getContext('2d');
    const w = Math.min(canvas.width, 50);
    const h = Math.min(canvas.height, 50);
    const data = ctx?.getImageData(0, 0, w, h).data;
    expect(data).toBeDefined();
    expect(data && [...data].some((b) => b !== 0)).toBe(true);
  });
});

describe('the document lifecycle', () => {
  it('starts empty and counts words once the edit has been laid out', async () => {
    view.newDocument();
    await painted();
    expect(view.wordCount()).toBe(0);

    view.replaceSelection('Hello brave new world');
    await painted();
    expect(view.wordCount()).toBe(4);
  });

  it('round-trips through .docx bytes', async () => {
    view.newDocument();
    await painted();
    view.replaceSelection('Round trip me');
    await painted();

    const bytes = view.toDocx();
    // A .docx is an OPC zip, so it must start with the local file header magic.
    expect(bytes.length).toBeGreaterThan(0);
    expect(bytes[0]).toBe(0x50);
    expect(bytes[1]).toBe(0x4b);

    view.loadDocx(bytes);
    await painted();
    expect(view.wordCount()).toBe(3);
  });

  it('undoes and redoes an edit', async () => {
    view.newDocument();
    await painted();
    view.replaceSelection('first second');
    await painted();
    const after = view.wordCount();
    expect(after).toBe(2);

    view.undo();
    await painted();
    expect(view.wordCount()).toBeLessThan(after);

    view.redo();
    await painted();
    expect(view.wordCount()).toBe(after);
  });
});

describe('headers and footers', () => {
  it('writes and reads back header and footer text', async () => {
    view.newDocument();
    await painted();
    view.setHeader('Confidential');
    view.setFooter('Page footer');
    await painted();
    expect(view.headerText()).toContain('Confidential');
    expect(view.footerText()).toContain('Page footer');
  });
});

describe('the extracted controllers are still wired up', () => {
  // These moved out of view.ts into their own modules. The compiler proves the delegations exist;
  // these prove they reach a live controller rather than an uninitialised field.

  it('picture state answers through the image controller', () => {
    expect(view.selectedImageId).toBeNull();
    expect(view.cropActive).toBe(false);
    expect(view.selectedImageWrap).toBeNull();
  });

  it('presence accepts names, avatars, markers and cursors without disturbing the page', async () => {
    view.newDocument();
    await painted();
    view.replaceSelection('Some text to mark');
    await painted();
    const before = view.wordCount();

    view.setAuthorNames({ a1: 'Alice' });
    view.setAuthorAvatars({ a1: 'https://example.invalid/a.png' });
    view.setChangeMarkers([{ para: 0, kind: 'insert', active: true }]);
    view.setRemoteCursors([]);
    await painted();

    expect(view.wordCount()).toBe(before);
    expect(container.querySelector('canvas')).not.toBeNull();
  });

  it('tears down cleanly, and a second teardown is harmless', () => {
    const v = view;
    v.destroy();
    expect(container.querySelector('canvas')).toBeNull();
    // afterEach destroys again, so this must not throw.
    expect(() => v.destroy()).not.toThrow();
  });
});
