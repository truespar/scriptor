// Small pure helpers shared across the view.
//
// Region encoding for the header/footer paragraph namespaces, unit conversions, the picture-file
// helpers, and the little formatting utilities the chrome needs. Nothing here touches the view or
// the engine, so it can be read and changed on its own.

import type { ChangeMarker } from './types';

// The engine namespaces header/footer paragraphs into disjoint high ranges above the body (see the
// wasm region encoding); body paragraphs occupy [0, 1<<28). Used to tell a body caret from a
// header/footer one without a wasm round-trip.
export const BODY_PARA_LIMIT = 1 << 28;
export const FOOTER_PARA_BASE = 2 << 28;
// The story a namespaced paragraph belongs to (0 = body, 1 = header, 2 = footer) - mirrors the wasm
// region encoding. Used to dim the inactive region(s) like Word when the caret enters a header/footer.
export const regionOf = (para: number): number =>
  para >= FOOTER_PARA_BASE ? 2 : para >= BODY_PARA_LIMIT ? 1 : 0;
// OOXML twip -> EMU (1 twip = 1/20 pt, 1 pt = 12700 EMU).
export const EMU_PER_TWIP = 635;

/** The accent colour for a picture's selection box + resize handles (opaque, so the handles read on
 *  a white page). */
export const SELECT_BLUE = '#2563eb';

/** CSS cursor per resize handle (diagonal pairs share a cursor). */
export const RESIZE_CURSORS: Record<string, string> = {
  nw: 'nwse-resize',
  se: 'nwse-resize',
  ne: 'nesw-resize',
  sw: 'nesw-resize',
  n: 'ns-resize',
  s: 'ns-resize',
  e: 'ew-resize',
  w: 'ew-resize',
};

/** Open a file picker for an image, resolving to the chosen File (or null if cancelled). Uses the
 *  input's `cancel` event (modern browsers) so a dismissed dialog resolves rather than hanging. */
export function pickImageFile(): Promise<File | null> {
  return new Promise((resolve) => {
    if (typeof document === 'undefined') {
      resolve(null);
      return;
    }
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = 'image/*';
    input.style.display = 'none';
    const done = (f: File | null): void => {
      input.remove();
      resolve(f);
    };
    input.addEventListener('change', () => done(input.files?.[0] ?? null));
    input.addEventListener('cancel', () => done(null));
    document.body.append(input);
    input.click();
  });
}

/** The natural pixel dimensions of encoded image `bytes` (MIME `mime`). Prefers `createImageBitmap`,
 *  falling back to an `<img>`; defaults to 1 inch (96 px) square if decoding fails, so an insert never
 *  produces a zero-size picture. */
export async function decodeImageSize(bytes: Uint8Array, mime: string): Promise<[number, number]> {
  const blob = new Blob([bytes as BlobPart], { type: mime });
  if (typeof createImageBitmap === 'function') {
    try {
      const bmp = await createImageBitmap(blob);
      const dims: [number, number] = [bmp.width, bmp.height];
      bmp.close();
      return dims;
    } catch {
      // fall through to the <img> path
    }
  }
  return new Promise((resolve) => {
    if (typeof Image === 'undefined') {
      resolve([96, 96]);
      return;
    }
    const url = URL.createObjectURL(blob);
    const img = new Image();
    img.onload = () => {
      resolve([img.naturalWidth || 96, img.naturalHeight || 96]);
      URL.revokeObjectURL(url);
    };
    img.onerror = () => {
      resolve([96, 96]);
      URL.revokeObjectURL(url);
    };
    img.src = url;
  });
}

/** Verb for a tracked-change kind - the hover tooltip ("{author} {verb}: ...") + popup header. */
export const TRACK_VERB: Record<string, string> = {
  ins: 'inserted',
  del: 'deleted',
  fmt: 'formatted',
  movefrom: 'moved from here',
  moveto: 'moved here',
};
/** Noun for a tracked-change kind - the Accept / Reject popup buttons ("Accept {noun}"). */
export const TRACK_NOUN: Record<string, string> = {
  ins: 'Insertion',
  del: 'Deletion',
  fmt: 'Formatting',
  movefrom: 'Move',
  moveto: 'Move',
};

/** Format an ISO-8601 timestamp for a comment header (short, locale-aware); falls back to the raw
 *  string when unparseable. */
export function formatCommentDate(iso: string): string {
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

/** Word's font-size ladder (points) for the Grow / Shrink Font commands - the size box's presets. */
export const FONT_SIZE_LADDER = [8, 9, 10, 10.5, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 36, 48, 72];

/** Coerce a user-typed string into a valid Word bookmark name: letters/digits/underscore only (others
 *  become `_`), letter-initial (a leading non-letter is prefixed with `B`), max 40 chars. `''` if empty. */
export function sanitizeBookmarkName(raw: string): string {
  let n = raw.trim().replace(/[^A-Za-z0-9_]/g, '_');
  if (!n) return '';
  if (!/^[A-Za-z]/.test(n)) n = `B${n}`;
  return n.slice(0, 40);
}

// Transparent by default: the inter-page gutters show the per-page frames' drop shadows + the
// host's page-stage backdrop through the gaps. A host can set an opaque `gutter` color to fill the
// gaps instead (which then covers the frame shadows - the pre-frames flat look).
export const GUTTER_DEFAULT = 'transparent';

/** Translucent fill for a change band, by kind + focus. */
export function changeBandColor(kind: ChangeMarker['kind'], active: boolean): string {
  const a = active ? 0.3 : 0.13;
  if (kind === 'insert') return `rgba(37, 99, 235, ${a})`; // blue
  if (kind === 'delete') return `rgba(220, 38, 38, ${a})`; // red
  return `rgba(217, 119, 6, ${a})`; // edited: amber
}

/** Solid accent (the active change's left bar), by kind. */
export function changeAccentColor(kind: ChangeMarker['kind']): string {
  if (kind === 'insert') return 'rgba(37, 99, 235, 0.9)';
  if (kind === 'delete') return 'rgba(220, 38, 38, 0.9)';
  return 'rgba(217, 119, 6, 0.9)';
}
